//! Addressing one markdown section of a document, and turning an edit to it
//! into the smallest request that expresses it.
//!
//! # Why this module exists
//!
//! Outline has no section-level endpoint. `documents.update` accepts either
//! the whole new body (`editMode=replace`, the default) or a fragment plus
//! an instruction: `append`, `prepend`, or `patch` with a `findText` the
//! server replaces. Nothing in that surface says "change the deployment
//! section", so a caller who wants to change one section has to express it
//! as one of those two things.
//!
//! Doing that by hand is what this module removes. An agent that edits a
//! section otherwise has to pull the entire body into its context, splice
//! it there, and push the entire body back - paying for the whole page
//! twice to change four lines of it. Here the page is read and spliced by
//! the CLI, so what crosses the agent boundary is one section in and one
//! section out, and what crosses the network is a `findText`/`text` pair.
//!
//! # Why the anchor is a string and not an offset
//!
//! `documents.update` has no `offset`/`length` parameter, so a byte range
//! cannot be sent even though this module computes one. `findText` is the
//! only positional primitive there is, which makes the anchor a string
//! whether we like it or not.
//!
//! That turns out to be the right shape anyway, for the reason local file
//! editing settled on it: a string anchor carries its own verification. An
//! offset that has drifted still points somewhere, and the write lands in
//! the wrong place silently; a string that no longer matches fails. The
//! failure is the feature.
//!
//! # Why the anchor is widened until it is unique
//!
//! `findText` is a plain string match, so an anchor occurring twice in the
//! body leaves the server to pick, and which one it picks is not part of
//! the published contract. That is the one hazard a caller cannot see
//! coming, so it is closed here rather than documented: the anchor starts
//! as the section's own bytes and grows outward, one line at a time, until
//! the document contains it exactly once. Uniqueness is then a counted
//! fact, not an assumption.
//!
//! This is legitimate here and would not be in a local-file `old_string`
//! tool, and the difference is worth naming. Widening an anchor is only
//! sound when the intended position is already known from something other
//! than the string - here the resolved heading supplies it. A tool handed
//! nothing but a string has no such anchor to grow from, which is why those
//! tools can only reject an ambiguous match and ask the caller to widen it.
//!
//! # How the parts divide
//!
//! - [`parse`] finds the headings and the span each one owns. It knows
//!   about markdown and nothing about editing.
//! - [`address`] turns a caller's string into one of those headings, and
//!   explains itself when it cannot. It also locates a plain string anchor,
//!   which is what lets a rejected `--find-text` name the sections its
//!   matches fall in.
//! - [`edit`] turns "this span becomes this text" into the request, which is
//!   where the widening lives.
//!
//! Everything in all three is pure, so all of it is testable without a
//! server.

mod address;
mod edit;
mod parse;

// `Unresolved` is deliberately not re-exported: a caller pairs `resolve`
// with `unresolved_message` and never needs to match on the reason, so
// keeping the enum inside `address` is what stops a second, divergent
// rendering of the same failure from being written somewhere else.
pub use address::{locate, resolve, unresolved_message, Location};
pub use edit::{delete, replace, Edit};
pub use parse::{headings, Heading};

/// Separator between the segments of a section address.
///
/// `>` rather than `/`, because a slash appears in heading text often
/// enough to matter ("read/write", a date) and an angle bracket does not.
pub const PATH_SEPARATOR: char = '>';

/// A document exercising the shapes all three submodules care about: a
/// preamble, nesting, and a section that runs to the end.
#[cfg(test)]
pub(super) const SAMPLE: &str = "\
intro line

## Deploy

deploy steps

### Rollback

rollback steps

## FAQ

answers
";
