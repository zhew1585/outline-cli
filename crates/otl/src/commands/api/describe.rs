//! `otl api describe <operation>` - one operation's whole contract.
//!
//! Everything printed here was already compiled into the binary (or into
//! the synced cache) and is already used: the same facets drive the local
//! validation `--no-validate` turns off, and the same response fields drive
//! the table renderer's column ranking. This command opens them, it does
//! not compute them - nothing is parsed, fetched or inferred, and no
//! credential is read.
//!
//! # It describes the EFFECTIVE table
//!
//! [`crate::ops::table`] resolves to the synced cache when one is usable
//! and to the built-in table otherwise, and `api list` and the call path
//! both dispatch from it. Describing anything else would hand a caller a
//! contract that disagrees with what the next command does, which is the
//! one failure a discovery command must not have. The `source` field says
//! which of the two answered.
//!
//! # Text safety
//!
//! Summaries, parameter descriptions, formats and enumerated values are
//! third-party text: they come from an OpenAPI document, and with `otl spec
//! sync` that document need not be the vendored one. Both IR entry points
//! already filter it - `spec_compile` SANITIZES display text (summaries and
//! parameter descriptions: dangerous characters dropped, whitespace folded
//! to one line, length capped) and REJECTS a document whose text with
//! MEANING (parameter names, content types, formats, enumerated values)
//! carries a dangerous character, and `crate::spec` applies the same
//! `is_display_safe` test to every string of a cache it loads. So no control
//! character, newline, `U+2028`/`U+2029`/`U+FEFF` or bidi override/isolate
//! can be in the IR at all.
//!
//! That table is nonetheless a strict SUBSET of the one every rendering
//! surface in this crate uses ([`crate::text::hazard`], the whole assigned
//! `Cf` category): `U+200E`/`U+200F`/`U+061C`, the `U+206A..U+206F` block,
//! `U+00AD`, `U+180E` and the `U+13430` block pass the compiler's test and
//! fail this crate's. Two tables that disagree is exactly the defect
//! `crate::text`'s module documentation is about, and the compiler cannot
//! simply borrow the engine's table (it is a build dependency and must not
//! pull `engine` into the host build). So every consumer closes the gap at
//! its own sink - and every consumer does, which is worth stating because it
//! was once written here as an open gap and is not one:
//!
//! - this module's human rendering, per value, with [`safe`];
//! - this module's JSON, once, in [`to_json_text`];
//! - `otl api`'s validation diagnostics, which quote enumerated values
//!   ("allowed values are ..."), through `main`'s
//!   `stdio::write_diagnostic_line`. Verified rather than assumed: a synced
//!   document whose enum value carries `U+200F` and `U+206A` produces
//!   `allowed values are: ok, bad`, with both stripped.
//!
//! `--json` is scrubbed here too, unlike a response payload. The exemption
//! in [`crate::text`] covers ONE payload - the bytes a server sent, rendered
//! by [`crate::render::render`], which have to round-trip. This object is
//! not that: it is a document `otl` writes about a third-party spec, nothing
//! round-trips it, and its intended reader is a program that will put the
//! text in front of a language model.

use serde_json::{json, Value};

use engine::{BodyMode, FieldSpec, OpSpec, ParamSpec};

use crate::exit::CliError;
use crate::ops;
use crate::paging;
use crate::render::{self, OutputMode};
use crate::stdio;

/// Print one operation's contract in the resolved output state.
pub(super) fn run(op: &OpSpec, mode: OutputMode) -> Result<(), CliError> {
    let text = match mode {
        OutputMode::Json => to_json_text(&as_json(op))?,
        OutputMode::Table => as_text(op),
    };
    stdio::write_data_line(&text)
}

/// Serialize one of the two JSON documents this pair of commands authors.
///
/// [`render::render_json_scrubbed`], not `render_json`: this object is not
/// a server response to round-trip, so the `--json` exemption documented in
/// [`crate::text`] does not reach it. That call is also the ONLY place the
/// JSON path scrubs - the builders below hand it raw IR text - because a
/// sink holds for fields that do not exist yet, while a per-field call has
/// to be remembered by whoever adds the next field.
pub(super) fn to_json_text(value: &Value) -> Result<String, CliError> {
    render::render_json_scrubbed(value)
        .map_err(|error| CliError::failure(anyhow::anyhow!("failed to render: {error}")))
}

/// Make one string from the compiled spec safe to write to the HUMAN
/// rendering.
///
/// The JSON path does not call this: it is scrubbed once, at
/// [`to_json_text`]. This one is per value because the human form
/// interleaves foreign text with layout, so there is no single string to
/// scrub at the end.
///
/// [`stdio::scrub_terminal_controls`] rather than a filter of this module's
/// own, deliberately: it matches exhaustively on [`crate::text::Hazard`],
/// so a category added to that enum later has to be answered rather than
/// silently forwarded, and reusing it means there is no fourth scrubbing
/// policy to keep in step with the other three.
///
/// Its one exception - a newline survives, because a diagnostic legitimately
/// spans lines - cannot apply to this input: `is_display_safe` rejects any
/// control character at both IR entry points, so an IR string containing a
/// newline does not exist. `no_ir_string_carries_a_newline` pins that, so
/// the exception stays vacuous rather than becoming a hole.
///
/// No length cap either, and for the same kind of reason: the compiler caps
/// every one of these strings already (200 characters for a summary, 256
/// bytes for an enumerated value, 64 for a format, 128 for a name).
pub(super) fn safe(raw: &str) -> String {
    stdio::scrub_terminal_controls(raw)
}

/// A string, or JSON `null` when the spec declared nothing.
///
/// One rule for every optional string in this output: an absent facet is
/// `null`, never `""`. The empty string would claim the spec said something
/// and that it was empty. Scrubbing is [`to_json_text`]'s job, not this
/// one's - every caller is building the JSON document.
pub(super) fn optional(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::Null;
    }
    Value::String(raw.to_string())
}

/// Stable wire name for a body mode.
///
/// Spelled out rather than derived from the enum so that the published
/// string is this crate's decision: `engine` may rename a variant, and an
/// exhaustive match makes a new one an error here instead of a silent new
/// value in the output.
pub(super) fn body_mode_name(mode: BodyMode) -> &'static str {
    match mode {
        BodyMode::KeyValue => "key_value",
        BodyMode::RawJsonOnly => "raw_json_only",
        BodyMode::Unsupported => "unsupported",
    }
}

/// How this operation's body may be supplied, in one sentence.
fn body_mode_note(op: &OpSpec) -> String {
    match op.body_mode {
        BodyMode::KeyValue => "key=value arguments, or --body @file.json".to_string(),
        BodyMode::RawJsonOnly => {
            "--body @file.json only: the request body is a oneOf/anyOf union, \
             which flat key=value pairs cannot express"
                .to_string()
        }
        BodyMode::Unsupported => format!(
            "not callable via `otl api`: it requires {}",
            safe(&op.content_type)
        ),
    }
}

/// The whole contract as one JSON object.
fn as_json(op: &OpSpec) -> Value {
    json!({
        "operation": op.name.as_ref(),
        "summary": optional(&op.summary),
        "path": op.path.as_ref(),
        "content_type": optional(&op.content_type),
        "body_mode": body_mode_name(op.body_mode),
        "callable": op.body_mode != BodyMode::Unsupported,
        "paginates": paging::spec_for(op).is_some(),
        "source": source(),
        "parameters": Value::Array(op.params.iter().map(param_json).collect()),
        "response_fields": Value::Array(op.response_fields.iter().map(field_json).collect()),
    })
}

/// One request parameter, with every facet the IR carries.
fn param_json(param: &ParamSpec) -> Value {
    json!({
        "name": param.name.as_ref(),
        "type": param.ty.to_string(),
        "description": optional(&param.description),
        "required": param.required,
        "nullable": param.nullable,
        "enum_values": Value::Array(
            param.enum_values.iter().map(|value| Value::String(value.to_string())).collect(),
        ),
        "format": optional(&param.format),
        "minimum": param.minimum,
        "maximum": param.maximum,
    })
}

/// One response field, in the source schema's own declaration order.
fn field_json(field: &FieldSpec) -> Value {
    json!({
        "name": field.name.as_ref(),
        "type": field.ty.to_string(),
        "format": optional(&field.format),
        "nullable": field.nullable,
        "read_only": field.read_only,
    })
}

/// Which table this description came from.
fn source() -> &'static str {
    if ops::is_synced() {
        "synced"
    } else {
        "built-in"
    }
}

/// The human form: a header block, then one aligned line per parameter and
/// per response field.
///
/// [`render::render_pairs`] does the layout for all three blocks because it
/// is the one renderer here that does NOT truncate: a table cell stops at
/// 40 columns, and a 40-column enumeration of allowed values would be a
/// contract with some of the values missing. Every value has passed through
/// [`safe`] before it gets here.
fn as_text(op: &OpSpec) -> String {
    let mut blocks = vec![render::render_pairs(&header(op))];
    blocks.push(block(
        "parameters",
        op.params
            .iter()
            .map(|p| (p.name.as_ref(), param_line(p), p.description.to_string())),
    ));
    blocks.push(block(
        "response fields",
        op.response_fields
            .iter()
            .map(|f| (f.name.as_ref(), field_line(f), String::new())),
    ));
    blocks.join("\n\n")
}

/// The operation-level facts, as label/value pairs.
fn header(op: &OpSpec) -> Vec<(&'static str, String)> {
    let paginates = match paging::spec_for(op) {
        Some(_) => "yes: every page is fetched unless --limit caps the total",
        None => "no",
    };
    vec![
        ("operation", safe(&op.name)),
        ("summary", safe(&op.summary)),
        ("path", safe(&op.path)),
        ("content type", safe(&op.content_type)),
        ("request body", body_mode_note(op)),
        ("paginates", paginates.to_string()),
        ("source", source_note()),
    ]
}

/// Where the description came from, spelled out for a reader.
fn source_note() -> String {
    match ops::is_synced() {
        true => "the table from `otl spec sync` (run `otl spec reset` to go back)".to_string(),
        false => "the spec built into this binary".to_string(),
    }
}

/// Render one titled block, or say it is empty.
///
/// Each row is a name, its facet line, and optionally the schema's prose.
/// The prose becomes a SECOND pair with an EMPTY label, which
/// [`render::render_pairs`] pads to the same width - so it lands under the
/// facet line rather than extending it. Two reasons not to fold it in:
/// a sentence appended to `string, optional, format=uuid` buries the facets
/// a caller is scanning for, and prose is the one value here that can be
/// 200 characters long.
fn block<'a>(title: &str, rows: impl Iterator<Item = (&'a str, String, String)>) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (name, line, prose) in rows {
        pairs.push((format!("  {}", safe(name)), line));
        if !prose.is_empty() {
            pairs.push((String::new(), safe(&prose)));
        }
    }
    if pairs.is_empty() {
        return format!("{title}\n  (none)");
    }
    let borrowed: Vec<(&str, String)> = pairs
        .iter()
        .map(|(name, line)| (name.as_str(), line.clone()))
        .collect();
    format!("{title}\n{}", render::render_pairs(&borrowed))
}

/// One parameter as `<type>, required|optional[, facet...]`.
///
/// The schema's prose is deliberately not folded in; see [`block`].
fn param_line(param: &ParamSpec) -> String {
    let mut parts = vec![param.ty.to_string()];
    parts.push(
        if param.required {
            "required"
        } else {
            "optional"
        }
        .to_string(),
    );
    if param.nullable {
        parts.push("nullable".to_string());
    }
    parts.extend(facets(
        &param.format,
        &param.enum_values,
        param.minimum,
        param.maximum,
    ));
    parts.join(", ")
}

/// The facets shared by the parameter and field renderings.
fn facets(
    format: &str,
    enum_values: &[std::borrow::Cow<'static, str>],
    minimum: Option<f64>,
    maximum: Option<f64>,
) -> Vec<String> {
    let mut parts = Vec::new();
    if !format.is_empty() {
        parts.push(format!("format={}", safe(format)));
    }
    if !enum_values.is_empty() {
        let values: Vec<String> = enum_values.iter().map(|value| safe(value)).collect();
        parts.push(format!("one of [{}]", values.join(", ")));
    }
    if let Some(minimum) = minimum {
        parts.push(format!("minimum {}", number(minimum)));
    }
    if let Some(maximum) = maximum {
        parts.push(format!("maximum {}", number(maximum)));
    }
    parts
}

/// One response field as `<type>[, facet...]`.
fn field_line(field: &FieldSpec) -> String {
    let mut parts = vec![field.ty.to_string()];
    if field.nullable {
        parts.push("nullable".to_string());
    }
    if field.read_only {
        parts.push("read-only".to_string());
    }
    parts.extend(facets(&field.format, &[], None, None));
    parts.into_iter().collect::<Vec<_>>().join(", ")
}

/// Print a schema bound without the `.0` an integral `f64` would show.
fn number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    format!("{value}")
}

/// The JSON object as a map, for tests that ask about its keys.
#[cfg(test)]
fn as_map(op: &OpSpec) -> serde_json::Map<String, Value> {
    match as_json(op) {
        Value::Object(map) => map,
        other => panic!("describe did not produce an object: {other}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::text::has_hazard;

    fn op(name: &str) -> &'static OpSpec {
        ops::find(name).expect("operation missing from the built-in table")
    }

    #[test]
    fn the_json_form_carries_every_request_facet_the_ir_holds() {
        let map = as_map(op("documents.info"));
        assert_eq!(map["operation"], "documents.info");
        assert_eq!(map["path"], "/api/documents.info");
        assert_eq!(map["content_type"], "application/json");
        assert_eq!(map["body_mode"], "key_value");
        assert_eq!(map["callable"], true);
        let params = map["parameters"].as_array().expect("parameters");
        let id = params
            .iter()
            .find(|param| param["name"] == "id")
            .expect("documents.info takes an `id`");
        for key in [
            "name",
            "type",
            "required",
            "nullable",
            "enum_values",
            "format",
            "minimum",
            "maximum",
        ] {
            assert!(id.get(key).is_some(), "parameter facet {key} missing: {id}");
        }
        assert_eq!(id["type"], "string");
    }

    #[test]
    fn the_json_form_carries_the_response_shape() {
        let map = as_map(op("documents.info"));
        let fields = map["response_fields"].as_array().expect("response_fields");
        assert!(
            !fields.is_empty(),
            "documents.info declares a response schema; an empty list would make \
             every assertion below vacuous"
        );
        let id = fields
            .iter()
            .find(|field| field["name"] == "id")
            .expect("the document response has an `id`");
        for key in ["name", "type", "format", "nullable", "read_only"] {
            assert!(id.get(key).is_some(), "field facet {key} missing: {id}");
        }
    }

    /// The bounds and enumerations `--no-validate` turns off are exactly
    /// what a caller needs before it can guess a value, so they have to be
    /// reachable from here.
    #[test]
    fn numeric_bounds_and_enumerations_reach_the_output() {
        let mut bounded = 0;
        let mut enumerated = 0;
        for spec in ops::table() {
            let map = as_map(spec);
            let text = as_text(spec);
            for param in map["parameters"].as_array().expect("parameters") {
                let name = param["name"].as_str().unwrap_or_default();
                let line = text
                    .lines()
                    .find(|line| line.trim_start().starts_with(&format!("{name} ")))
                    .unwrap_or_default()
                    .to_string();
                if let Some(minimum) = param["minimum"].as_f64() {
                    bounded += 1;
                    assert!(
                        line.contains(&format!("minimum {}", number(minimum))),
                        "{} {name}: {line:?}",
                        spec.name
                    );
                }
                if let Some(values) = param["enum_values"].as_array() {
                    if let Some(first) = values.first().and_then(Value::as_str) {
                        enumerated += 1;
                        assert!(line.contains(first), "{} {name}: {line:?}", spec.name);
                    }
                }
            }
        }
        assert!(bounded > 0, "no numeric bound in the whole table: vacuous");
        assert!(enumerated > 0, "no enumeration in the whole table: vacuous");
    }

    #[test]
    fn an_absent_facet_is_null_rather_than_an_empty_string() {
        let unformatted = ops::table()
            .iter()
            .flat_map(|spec| spec.params.iter())
            .find(|param| param.format.is_empty())
            .expect("some parameter declares no format");
        assert_eq!(param_json(unformatted)["format"], Value::Null);
        assert_eq!(optional(""), Value::Null);
        assert_eq!(optional("x"), Value::String("x".to_string()));
    }

    #[test]
    fn the_human_form_names_the_operation_and_every_parameter() {
        let spec = op("documents.info");
        let text = as_text(spec);
        assert!(text.contains("documents.info"), "{text}");
        assert!(text.contains("/api/documents.info"), "{text}");
        assert!(text.contains("parameters"), "{text}");
        assert!(text.contains("response fields"), "{text}");
        for param in spec.params.iter() {
            assert!(text.contains(param.name.as_ref()), "{} missing", param.name);
        }
        for field in spec.response_fields.iter() {
            assert!(text.contains(field.name.as_ref()), "{} missing", field.name);
        }
    }

    /// The prose is the reason the IR went to schema version 6, and the
    /// human form is where it does the most work: `documents.info` marks
    /// NEITHER parameter required, and only the prose says that one of them
    /// is nonetheless needed.
    ///
    /// Its own line, indented under the facets rather than appended to
    /// them, which is the part that had no test at all until a mutation
    /// (deleting the prose line) came back GREEN.
    #[test]
    fn the_human_form_carries_each_parameters_prose_on_its_own_line() {
        let spec = op("documents.info");
        let text = as_text(spec);
        let described: Vec<&ParamSpec> = spec
            .params
            .iter()
            .filter(|param| !param.description.is_empty())
            .collect();
        assert!(
            !described.is_empty(),
            "documents.info declares prose on both parameters; an empty list \
             would make this assertion vacuous"
        );
        for param in described {
            let prose = safe(&param.description);
            let line = text
                .lines()
                .find(|line| line.trim() == prose.trim())
                .unwrap_or_else(|| panic!("no line of its own for {}: {text}", param.name));
            // Indented into the value column, not starting at the name.
            assert!(line.starts_with(' '), "{line:?}");
            // And the facet line is still its own line, without the prose
            // appended to it - which is what "on its own line" means.
            let facets = text
                .lines()
                .find(|line| line.trim_start().starts_with(&format!("{} ", param.name)))
                .unwrap_or_else(|| panic!("no facet line for {}: {text}", param.name));
            assert!(
                !facets.contains(prose.trim()),
                "prose folded into the facet line: {facets:?}"
            );
            assert!(facets.contains(&param.ty.to_string()), "{facets:?}");
        }
        // And the disambiguation an agent actually needs is present.
        assert!(text.contains("Either the UUID or the urlId"), "{text}");
    }

    #[test]
    fn an_uncallable_operation_says_so_in_both_states() {
        let spec = ops::table()
            .iter()
            .find(|spec| spec.body_mode == BodyMode::Unsupported)
            .expect("the vendored spec has a multipart operation");
        let map = as_map(spec);
        assert_eq!(map["callable"], false);
        assert_eq!(map["body_mode"], "unsupported");
        let text = as_text(spec);
        assert!(text.contains("not callable"), "{text}");
        assert!(text.contains(spec.content_type.as_ref()), "{text}");
    }

    /// `--limit` is only meaningful where the operation paginates, and the
    /// call path refuses it elsewhere, so the description has to say which.
    #[test]
    fn pagination_is_reported_and_agrees_with_the_call_path() {
        for spec in ops::table() {
            let expected = paging::spec_for(spec).is_some();
            assert_eq!(as_map(spec)["paginates"], expected, "{}", spec.name);
        }
        assert_eq!(as_map(op("documents.list"))["paginates"], true);
        assert_eq!(as_map(op("documents.info"))["paginates"], false);
    }

    /// Every string that leaves this module has been through [`safe`], in
    /// both states. The whole compiled table is checked rather than one
    /// operation, because the property has to hold for text nobody looked
    /// at.
    #[test]
    fn no_string_reaches_stdout_carrying_a_hazard() {
        assert!(
            has_hazard("a\u{200f}b"),
            "the hazard table no longer covers U+200F; this test would be vacuous"
        );
        // Line by line: both forms are multi-line by construction, and a
        // newline is itself a `Hazard::Control`. The layout's own newlines
        // are this module's, not the document's - what has to be free of
        // hazards is every line between them.
        for spec in ops::table() {
            let text = as_text(spec);
            let json = to_json_text(&as_json(spec)).expect("json");
            for line in text.lines().chain(json.lines()) {
                assert!(!has_hazard(line), "{}: {line:?}", spec.name);
            }
        }
    }

    /// [`safe`] uses the diagnostic scrubber, whose one exception is that a
    /// newline survives. That exception is only harmless because no IR
    /// string can contain one - which is a property of the compiler and the
    /// cache loader, not of this module, so it is asserted here.
    #[test]
    fn no_ir_string_carries_a_newline() {
        for spec in ops::table() {
            let mut strings = vec![
                spec.name.as_ref(),
                spec.path.as_ref(),
                spec.summary.as_ref(),
                spec.content_type.as_ref(),
            ];
            for param in spec.params.iter() {
                strings.push(param.name.as_ref());
                strings.push(param.format.as_ref());
                strings.extend(param.enum_values.iter().map(std::convert::AsRef::as_ref));
            }
            for field in spec.response_fields.iter() {
                strings.push(field.name.as_ref());
                strings.push(field.format.as_ref());
            }
            for text in strings {
                assert!(!text.contains('\n'), "{}: {text:?}", spec.name);
            }
        }
    }

    #[test]
    fn a_hostile_string_is_neutralised_in_both_states() {
        let hostile = "before\u{1b}]52;c;cGF5bG9hZA==\u{7}after\u{202e}\u{200f}";
        // The human path scrubs per value, because layout is interleaved.
        let cleaned = safe(hostile);
        assert!(!has_hazard(&cleaned), "{cleaned:?}");
        assert!(cleaned.starts_with("before"), "{cleaned:?}");
        assert!(cleaned.contains("after"), "{cleaned:?}");

        // The JSON path scrubs once, at the sink - so a builder that hands
        // it raw text (which every builder here does, on purpose) is still
        // safe. Asserted on the SERIALIZED form, since that is what reaches
        // stdout: `optional` itself deliberately does not scrub any more.
        assert_eq!(optional(hostile), Value::String(hostile.to_string()));
        let rendered = to_json_text(&json!({ "summary": hostile })).expect("json");
        for line in rendered.lines() {
            assert!(!has_hazard(line), "{line:?}");
        }
        assert!(rendered.contains("before"), "{rendered:?}");
    }

    #[test]
    fn integral_bounds_print_without_a_fraction() {
        assert_eq!(number(100.0), "100");
        assert_eq!(number(1.5), "1.5");
        assert_eq!(number(0.0), "0");
    }
}
