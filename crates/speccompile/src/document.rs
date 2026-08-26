//! Bounded parsing of an untrusted OpenAPI document.
//!
//! # Why a byte limit on the download is not a memory limit
//!
//! A JSON document expands when it is parsed. `[0,0,0,...]` costs two
//! bytes per element on the wire and about forty in a `serde_json::Value`
//! tree; a 16 MiB document of that shape was measured at 367 MB of
//! resident memory. Capping the DOWNLOAD therefore does not do what the
//! cap says it does - it moves the out-of-memory threshold, it does not
//! remove it - and every later limit (the IR, the cache) applies to
//! something that only exists after this parse has already happened.
//!
//! # What this module does
//!
//! Two things, in the order that matters:
//!
//! 1. **Only what the compiler reads is materialized.** The top level
//!    keeps `paths` and `components`; every other key is consumed with
//!    `IgnoredAny`, which parses without building anything. The measured
//!    367 MB case is entirely made of such a key.
//! 2. **What IS materialized is charged against a budget.** Each value
//!    charges its own approximate heap cost as it is built, and parsing
//!    stops the moment the total passes [`MAX_PARSED_BYTES`] - not after,
//!    the way a size check on the finished tree would.
//!
//! The charge is an estimate of heap cost, not a measurement of it. The
//! REAL peak is asserted by `crates/otl/tests/memory_bounds.rs`, which
//! runs a heap profiler over exactly these paths - including the 16 MiB
//! document that was measured at 367 MB before this module existed.
//! Prose arithmetic about allocation has been wrong here twice; the
//! numbers live in a test now.

use std::cell::Cell;
use std::fmt;

use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use crate::CompileError;

/// Ceiling on the estimated heap cost of the parts of a document that are
/// kept.
///
/// A real API description of the size this CLI vendors charges about
/// 2 MiB against it, so the headroom is roughly twelvefold, while a
/// document engineered to expand is refused long before it becomes
/// interesting. (Measured figures live in the consumer's own tests; this
/// crate names no service.)
pub const MAX_PARSED_BYTES: usize = 24 * 1024 * 1024;

/// Approximate cost of one `Value` node, whatever it holds.
const VALUE_COST: usize = 32;
/// Approximate cost of one array slot, including growth slack.
const SEQ_SLOT: usize = 64;
/// Approximate cost of one object entry: map bookkeeping plus the key's
/// own `String` header.
const MAP_ENTRY: usize = 96;

/// The parts of an OpenAPI document this compiler reads.
#[derive(Debug)]
pub(crate) struct Document {
    /// The `paths` object, or `Value::Null` when the document has none.
    pub(crate) paths: Value,
    /// The `components` object, used to resolve local `$ref`s.
    pub(crate) components: Value,
}

/// A shrinking allowance, threaded through the parse by seed.
///
/// `exhausted` exists so the caller can tell two failures apart: a
/// document that is not JSON, and a document that is perfectly good JSON
/// and simply too large. serde hands back only a string, and those two
/// deserve different messages.
struct Budget {
    remaining: Cell<usize>,
    limit: usize,
    exhausted: Cell<bool>,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            remaining: Cell::new(limit),
            limit,
            exhausted: Cell::new(false),
        }
    }

    /// Charge `cost`, or report that the document is too large to parse.
    fn charge<E: serde::de::Error>(&self, cost: usize) -> Result<(), E> {
        match self.remaining.get().checked_sub(cost) {
            Some(left) => {
                self.remaining.set(left);
                Ok(())
            }
            None => {
                self.exhausted.set(true);
                Err(E::custom("the parse budget is exhausted"))
            }
        }
    }

    fn spent(&self) -> usize {
        self.limit - self.remaining.get()
    }
}

/// Parse the parts of a JSON document that the compiler reads, refusing
/// anything that would expand past `limit`.
pub(crate) fn parse(raw: &str, limit: usize) -> Result<Document, CompileError> {
    let budget = Budget::new(limit);
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let outcome = DocumentSeed(&budget)
        .deserialize(&mut deserializer)
        .and_then(|document| deserializer.end().map(|()| document));
    match outcome {
        Ok(document) => Ok(document),
        // Two quite different failures reach here, and "not valid JSON" is
        // a bad thing to tell someone whose JSON is fine and merely huge.
        Err(_) if budget.exhausted.get() => Err(CompileError::TooLarge {
            document_bytes: raw.len(),
            charged: budget.spent(),
            limit,
        }),
        Err(error) => Err(CompileError::NotJson {
            reason: error.to_string(),
        }),
    }
}

/// The estimated cost of a parsed document, for tests and diagnostics.
#[cfg(test)]
pub(crate) fn parse_cost(raw: &str) -> Result<usize, CompileError> {
    let budget = Budget::new(MAX_PARSED_BYTES);
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    DocumentSeed(&budget)
        .deserialize(&mut deserializer)
        .map_err(|error| CompileError::NotJson {
            reason: error.to_string(),
        })?;
    Ok(budget.spent())
}

struct DocumentSeed<'a>(&'a Budget);

impl<'de> DeserializeSeed<'de> for DocumentSeed<'_> {
    type Value = Document;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Document, D::Error> {
        deserializer.deserialize_map(DocumentVisitor(self.0))
    }
}

struct DocumentVisitor<'a>(&'a Budget);

impl<'de> Visitor<'de> for DocumentVisitor<'_> {
    type Value = Document;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an OpenAPI document")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Document, A::Error> {
        let mut paths = Value::Null;
        let mut components = Value::Null;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "paths" => paths = map.next_value_seed(ValueSeed(self.0))?,
                "components" => components = map.next_value_seed(ValueSeed(self.0))?,
                // Parsed and discarded, allocating nothing: the compiler
                // reads neither, and an unread key is where a document
                // engineered to expand puts its payload.
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Document { paths, components })
    }
}

/// Render a byte count the way a person reads it, keeping the exact
/// figure: `24.0 MiB (25165824 bytes)`.
pub(crate) fn human_bytes(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    const KIB: usize = 1024;
    if bytes >= MIB {
        format!("{:.1} MiB ({bytes} bytes)", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB ({bytes} bytes)", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

/// Deserializes one `Value`, charging the budget as it builds it.
struct ValueSeed<'a>(&'a Budget);

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(ValueVisitor(self.0))
    }
}

struct ValueVisitor<'a>(&'a Budget);

impl<'de> Visitor<'de> for ValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        self.0.charge(VALUE_COST)?;
        Ok(Value::Null)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        self.0.charge(VALUE_COST)?;
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        self.0.charge(VALUE_COST)?;
        Ok(Value::from(value))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        self.0.charge(VALUE_COST)?;
        Ok(Value::from(value))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        self.0.charge(VALUE_COST)?;
        Ok(Value::from(value))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        self.0.charge(VALUE_COST + value.len())?;
        Ok(Value::String(value.to_string()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        self.0.charge(VALUE_COST)?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(ValueSeed(self.0))? {
            self.0.charge(SEQ_SLOT)?;
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        self.0.charge(VALUE_COST)?;
        let mut entries = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            self.0.charge(MAP_ENTRY + key.len())?;
            let value = map.next_value_seed(ValueSeed(self.0))?;
            entries.insert(key, value);
        }
        Ok(Value::Object(entries))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn keeps_only_paths_and_components() {
        let raw = r#"{"openapi":"3.0.0","info":{"title":"x"},
            "paths":{"/a.b":{"post":{}}},"components":{"schemas":{"S":{}}}}"#;
        let document = parse(raw, MAX_PARSED_BYTES).expect("parses");
        assert!(document.paths.get("/a.b").is_some());
        assert!(document.components.pointer("/schemas/S").is_some());
    }

    /// The measured 367 MB case: a huge array under a key the compiler
    /// never reads. It must cost nothing at all.
    #[test]
    fn an_unread_key_costs_nothing() {
        let filler = vec!["0"; 200_000].join(",");
        let raw = format!(r#"{{"paths":{{}},"x":[{filler}]}}"#);
        let cost = parse_cost(&raw).expect("parses");
        assert!(
            cost < 4 * 1024,
            "an ignored key was materialized: {cost} bytes charged"
        );
    }

    /// The same payload under a key the compiler DOES read is charged, and
    /// past the budget it is refused - while parsing, not afterwards.
    #[test]
    fn expansion_under_a_read_key_is_refused() {
        let filler = vec!["0"; 200_000].join(",");
        let raw = format!(r#"{{"paths":{{"/a.b":[{filler}]}}}}"#);
        let error = parse(&raw, 64 * 1024).expect_err("must be refused");
        let text = error.to_string();
        // The input is perfectly good JSON: saying otherwise sends the
        // reader looking for a syntax error that is not there.
        assert!(
            !text.contains("not a valid JSON"),
            "a legal document was called malformed: {text}"
        );
        assert!(text.contains("expands to"), "unexpected error: {text}");
        // Readable size, exact size, and the input's own size.
        assert!(text.contains("64.0 KiB"), "{text}");
        assert!(text.contains("65536"), "{text}");
        assert!(text.contains(&raw.len().to_string()), "{text}");
    }

    #[test]
    fn a_genuine_syntax_error_is_still_reported_as_one() {
        let error = parse("{\"paths\": ", MAX_PARSED_BYTES).expect_err("must fail");
        assert!(
            error.to_string().contains("not a valid JSON"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn human_bytes_keeps_the_exact_figure() {
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(64 * 1024), "64.0 KiB (65536 bytes)");
        assert_eq!(human_bytes(24 * 1024 * 1024), "24.0 MiB (25165824 bytes)");
    }

    #[test]
    fn charges_strings_by_their_length() {
        let short = parse_cost(r#"{"paths":{"a":"x"}}"#).unwrap();
        let long = parse_cost(&format!(r#"{{"paths":{{"a":"{}"}}}}"#, "x".repeat(1000))).unwrap();
        assert!(long > short + 900, "short {short}, long {long}");
    }

    #[test]
    fn rejects_trailing_content_after_the_document() {
        assert!(parse(r#"{"paths":{}} trailing"#, MAX_PARSED_BYTES).is_err());
    }
}
