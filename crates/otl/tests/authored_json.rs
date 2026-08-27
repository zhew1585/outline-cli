//! Every `--json` surface either round-trips a server response or is
//! scrubbed. Nothing gets to be a third thing by accident.
//!
//! # Why this file exists
//!
//! `crate::render` offers two JSON renderers, and the choice between them is
//! a security decision:
//!
//! - [`render_json`] emits the value verbatim. That is the contract for a
//!   SERVER RESPONSE: `otl api documents.info --json` has to round-trip
//!   byte-for-byte or it cannot be diffed, replayed or verified. Changing a
//!   byte would break the one promise that output makes.
//! - `render_json_scrubbed` strips terminal-control and residual format
//!   characters from every string. That is the rule for an object `otl`
//!   AUTHORS - a doctor report, an `api describe` contract, an `auth info`
//!   status. Nothing round-trips those, so the exemption's premise does not
//!   hold, while the third-party text mixed into them is exactly the hazard
//!   the scrubber exists for.
//!
//! The distinction was not obvious, and the codebase got it wrong three
//! separate times. The old exemption was written as "`--json` is exempt"
//! while its own justification said "JSON is the payload the server sent" -
//! a conclusion wider than its reason. Three authored surfaces each applied
//! the wider form to themselves, and each was found by hand, one per review
//! round: `doctor`'s report, `api describe`'s contract, and `auth info`'s
//! status. Every fix was correct and none of them stopped the next one.
//!
//! So the point here is not to re-check those three. It is that a fourth
//! authored surface written tomorrow - reaching for `render_json` because it
//! is the obvious name - would pass every other gate in this repository. The
//! registry below makes that impossible to do silently: a new call site
//! fails until someone writes down which of the two kinds it is and why.
//!
//! # Why a registry rather than a ban
//!
//! `render_json` cannot simply be forbidden: four call sites need it, and
//! need it for the reason it exists. What can be required is that each one
//! is REVIEWED - named, counted, and justified in a place a reader will
//! find. Same shape as `no_phone_home.rs`, which cannot ban `.send()`
//! either and instead pins it to three declared channels.
//!
//! # What the residual set is, and why "JSON encoding makes it safe" is
//! only half true
//!
//! `serde_json` escapes C0 control characters, so `\x1b` leaves as
//! `` and cannot start an escape sequence. It does NOT escape the
//! residual format characters - `U+200E`, `U+200F`, `U+061C`, `U+202E`,
//! `U+206A`, `U+00AD` and friends - which travel through JSON verbatim and
//! reorder or hide text the moment the value is printed. Any justification
//! resting on "it is inside JSON" is therefore sound for the first class and
//! unsound for the second. `EXEMPT` records where that argument is load
//! bearing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// A reviewed `render_json` call site.
///
/// `context` must name the CALL SITE, not the file: a file-wide exemption
/// would let a later authored surface into the same file unnoticed, which is
/// the whole failure mode this guard exists to prevent. `count` catches a
/// second occurrence that happens to match the same context anyway, and
/// fails when a site is removed too, so a stale entry cannot sit here
/// looking like it constrains something.
struct Reviewed {
    /// Path relative to the workspace root.
    file: &'static str,
    /// Substring that must appear on the call-site line.
    context: &'static str,
    /// How many times the pattern may appear on lines matching `context`.
    count: usize,
    /// Which of the two kinds this is, and why.
    why: &'static str,
}

/// The verbatim renderer's reviewed call sites.
///
/// Four of the five are the same case: the value handed to `render_json` is
/// server rows or a server document, forwarded unchanged. The fifth is not,
/// and says so.
const EXEMPT: &[Reviewed] = &[
    Reviewed {
        file: "crates/otl/src/commands/collections.rs",
        context: "render::render_json(&payload)",
        count: 1,
        why: "SERVER RESPONSE. `payload` is `Value::Array(collections.items)` \
              - the rows as the API returned them, with no synthetic field \
              added, so a script never sees a value the API cannot confirm.",
    },
    Reviewed {
        file: "crates/otl/src/commands/docs/search.rs",
        context: "render::render_json(&payload)",
        count: 1,
        why: "SERVER RESPONSE. `payload` is `Value::Array(hits)`, the raw \
              result rows exactly as the server sent them.",
    },
    Reviewed {
        file: "crates/otl/src/commands/docs/view.rs",
        context: "render::render_json(document)",
        count: 1,
        why: "SERVER RESPONSE. A single server document, forwarded whole.",
    },
    Reviewed {
        file: "crates/otl/src/commands/docs/detail.rs",
        context: "render::render_json(document)",
        count: 1,
        why: "SERVER RESPONSE. A single server document, forwarded whole.",
    },
    Reviewed {
        file: "crates/otl/src/commands/docs/export.rs",
        context: "render::render_json(&payload)",
        count: 1,
        why: "AUTHORED, and deliberately unscrubbed - the one entry here \
              that is not a server response. `print_json` builds the object \
              with `serde_json::json!`, mixing in server text (`failure.id`, \
              `failure.label`, `failure.reason`) and local paths. Story 3.6 \
              chose to keep `Failure::id` byte-exact so a script can retry \
              precisely the entries where `id != null`, and pinned it with \
              `a_hostile_document_id_cannot_rewrite_the_terminal`. That \
              reasoning holds for control characters, which serde escapes. \
              It does NOT hold for the residual format characters, which \
              serde does not - so this surface can still reorder a terminal \
              through `label` or `reason`, neither of which any script \
              retries on. Registered rather than silently inherited: the \
              decision is real, its limit is real, and both are now written \
              where the next reader will see them.",
    },
];

/// Call sites of [`render::render`], the THIRD door.
///
/// Its JSON branch is `serde_json::to_string_pretty` - byte-for-byte
/// verbatim, exactly like `render_json` - but it never names `render_json`,
/// so the scans above cannot see it. That blind spot shipped: the MCP
/// command surface routes every payload through one `output::emit` helper
/// built on `render`, and an authored object (`otl fetch attachment`'s
/// `{id, signedUrl}`, whose `id` is echoed from the command line) went out
/// unscrubbed while this file claimed every `--json` surface was covered.
///
/// The fix is structural rather than per-command: `commands/output.rs` has
/// one verbatim emitter and one scrubbing emitter, so each command names
/// the kind of value it is printing, and only those two internal call sites
/// need to be registered here.
const RENDER: &[Reviewed] = &[
    Reviewed {
        file: "crates/otl/src/commands/api/mod.rs",
        context: "render::render(payload, mode, schema)",
        count: 1,
        why: "SERVER RESPONSE for every operation but one, and this is the \
              call that keeps `otl api`'s promise that the reply \
              round-trips: the payload is the server's, the schema only \
              picks table columns. The exception is the redirect contract \
              (`attachments.redirect`), where the value is \
              `{\"data\":{\"signedUrl\": location}}` - a wrapper authored \
              here around one header value. It stays verbatim ON PURPOSE: \
              the URL is signed, so scrubbing could invalidate it, and \
              `HeaderValue::to_str` already constrains it to visible ASCII, \
              which carries neither a control nor a format character. \
              AUTHORED, inert, and deliberately not scrubbed - said here \
              rather than left to look like an oversight.",
    },
    Reviewed {
        file: "crates/otl/src/commands/output.rs",
        context: "render::render(value, mode, &[])",
        count: 1,
        why: "SERVER RESPONSE. This is `emit_server`, whose contract is that \
              its argument came from the server unchanged; the authored case \
              is `emit_authored` and goes through the scrubbing renderer. \
              Every caller picks one by name, which is what makes the choice \
              reviewable at the call site rather than here.",
    },
];

/// Call sites of the scrubbing renderer, so an authored surface cannot
/// quietly stop scrubbing.
///
/// The guard would otherwise be one-sided: it notices a new `render_json`
/// but not an existing `render_json_scrubbed` being downgraded. Each of
/// these three cost a review round to find.
const SCRUBBED: &[Reviewed] = &[
    Reviewed {
        file: "crates/otl/src/commands/auth/output.rs",
        context: "render::render_json_scrubbed(&output.value)",
        count: 1,
        why: "AUTHORED. `auth info`/`login`/`logout`/`set-key` status, \
              carrying a config profile name, local paths, and server \
              `account`/`workspace`/`scope`.",
    },
    Reviewed {
        file: "crates/otl/src/commands/doctor/report.rs",
        context: "render::render_json_scrubbed(&report.value())",
        count: 1,
        why: "AUTHORED. The doctor report, carrying operation names from a \
              fetched document and an instance origin.",
    },
    Reviewed {
        file: "crates/otl/src/commands/api/describe.rs",
        context: "render::render_json_scrubbed(value)",
        count: 1,
        why: "AUTHORED. An operation contract built from the compiled spec: \
              summaries, enum values, formats and parameter prose, all of \
              it third-party text an agent will feed to a model.",
    },
    Reviewed {
        file: "crates/otl/src/commands/output.rs",
        context: "render::render_json_scrubbed(value)",
        count: 1,
        why: "AUTHORED. `emit_authored`, the half of this module's pair that \
              prints objects this CLI built - today `otl fetch attachment`'s \
              `{id, signedUrl}`, where `id` is echoed from an argument or \
              extracted from a URL that came out of a server document.",
    },
    Reviewed {
        file: "crates/otl/src/commands/skill/mod.rs",
        context: "render::render_json_scrubbed(&json(outcomes))",
        count: 1,
        why: "AUTHORED. What `otl skill install` did, carrying paths derived \
              from the environment and the `name` frontmatter of a SKILL.md \
              that some other tool wrote.",
    },
];

/// Workspace root: `crates/otl` -> `crates` -> root.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

/// Every `.rs` file under a crate's `src/`, which is what ships.
///
/// Tests are out of scope on purpose: a test that renders JSON is asserting
/// about a renderer, not shipping an output surface.
fn runtime_sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for crate_dir in [
        "crates/engine/src",
        "crates/otl/src",
        "crates/speccompile/src",
    ] {
        collect(&workspace_root().join(crate_dir), &mut found);
    }
    found.sort();
    assert!(
        found.len() > 20,
        "found only {} runtime sources; the walk is broken, and a broken \
         walk makes this whole file pass vacuously",
        found.len()
    );
    found
}

fn collect(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Count `render_json(` occurrences that are NOT `render_json_scrubbed(`.
///
/// The plain name is a prefix of the scrubbed one, so a substring search
/// finds both. Matching on the trailing `(` separates them: a scrubbed call
/// reads `render_json_scrubbed(`, whose character after `render_json` is
/// `_`, not `(`.
fn count_verbatim(line: &str) -> usize {
    line.match_indices("render_json")
        .filter(|(at, _)| line[at + "render_json".len()..].starts_with('('))
        .count()
}

fn count_scrubbed(line: &str) -> usize {
    line.matches("render_json_scrubbed(").count()
}

/// Calls to `render` itself - the schema-aware renderer whose JSON branch
/// is verbatim.
///
/// Matched by the BARE name, like the two scans above, and that is the
/// point: the first version required the `render::` qualifier, so a module
/// that wrote `use crate::render::render;` and then called `render(&value,
/// mode, &[])` - which the compiler and this codebase's style both accept -
/// was counted zero, and could have shipped an unregistered authored
/// surface through the very door this scan was added to watch.
///
/// `render_json(` and `render_json_scrubbed(` do not contain `render(`, so
/// they cannot be double-counted; `try_render_table(` does not either. The
/// renderer's own definition is in `render.rs`, which [`check`] skips.
fn count_render(line: &str) -> usize {
    line.match_indices("render(")
        // Only a whole word: `xrender(` is some other function.
        .filter(|(at, _)| {
            line[..*at]
                .chars()
                .next_back()
                .is_none_or(|previous| !previous.is_alphanumeric() && previous != '_')
        })
        .count()
}

/// Lines of shipped code, with comments and `#[cfg(test)]` modules dropped.
///
/// A mention in a comment is not a call site, and this file's own prose
/// names both renderers repeatedly.
fn code_lines(source: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut in_test_module = false;
    let mut depth = 0i32;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#[cfg(test)]") {
            in_test_module = true;
            depth = 0;
            continue;
        }
        if in_test_module {
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if depth <= 0 && line.contains('}') {
                in_test_module = false;
            }
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        lines.push(line);
    }
    lines
}

/// Registry entries for one file and one renderer.
fn entries_for<'a>(registry: &'a [Reviewed], file: &str) -> Vec<&'a Reviewed> {
    registry.iter().filter(|entry| entry.file == file).collect()
}

/// Both renderers, checked the same way: every call site must match a
/// registered context, and the per-context counts must be exact.
fn check(registry: &[Reviewed], renderer: &str, count_calls: fn(&str) -> usize) -> Vec<String> {
    let root = workspace_root();
    let mut problems = Vec::new();
    for path in runtime_sources() {
        let relative = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        // The renderer's own definition is not a call site.
        if relative == "crates/otl/src/render.rs" {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let registered = entries_for(registry, &relative);
        let mut found = 0usize;
        for line in code_lines(&source) {
            let calls = count_calls(line);
            if calls == 0 {
                continue;
            }
            found += calls;
            if !registered.iter().any(|entry| line.contains(entry.context)) {
                problems.push(format!(
                    "  {relative}: unregistered `{renderer}` call site\n    at: {}\n    \
                     Decide which kind it is and add it to the registry in \
                     tests/authored_json.rs:\n      - a SERVER RESPONSE that must \
                     round-trip -> `EXEMPT`, saying so;\n      - an object otl \
                     AUTHORS -> switch it to `render_json_scrubbed` and register \
                     it in `SCRUBBED`.",
                    line.trim()
                ));
            }
        }
        let allowed: usize = registered.iter().map(|entry| entry.count).sum();
        if found != allowed {
            problems.push(format!(
                "  {relative}: registry allows {allowed} `{renderer}` call(s), found \
                 {found}. A count that no longer matches means either a new call \
                 arrived at a registered site, or a registered site is gone and its \
                 entry is now decoration."
            ));
        }
    }
    problems
}

#[test]
fn every_verbatim_json_call_site_is_reviewed() {
    let problems = check(EXEMPT, "render_json", count_verbatim);
    assert!(
        problems.is_empty(),
        "`render_json` emits its value with no scrubbing, which is correct \
         only for a server response that must round-trip:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_scrubbed_json_call_site_is_reviewed() {
    let problems = check(SCRUBBED, "render_json_scrubbed", count_scrubbed);
    assert!(
        problems.is_empty(),
        "an authored `--json` surface stopped scrubbing, or a new one \
         appeared unregistered:\n{}",
        problems.join("\n")
    );
}

#[test]
fn every_render_call_site_is_reviewed() {
    let problems = check(RENDER, "render::render", count_render);
    assert!(
        problems.is_empty(),
        "`render::render` emits JSON verbatim, so a value that is not a \
         server response must not go through it - use the scrubbing \
         emitter and register it:\n{}",
        problems.join("\n")
    );
}

/// A registration has to name a call site, not a file.
///
/// Without this, `context: ""` - or any substring every line contains -
/// would turn an entry into a file-wide hole, and the next authored surface
/// in that file would inherit the exemption in silence. That is exactly how
/// the three known cases happened, one review round apart.
#[test]
fn a_registration_names_a_call_site_not_a_whole_file() {
    for entry in EXEMPT.iter().chain(SCRUBBED).chain(RENDER) {
        assert!(
            entry.context.contains("render_json") || entry.context.contains("render::render("),
            "{}: context {:?} does not name a render call, so it exempts \
             more than a call site",
            entry.file,
            entry.context
        );
        assert!(
            entry.count > 0,
            "{}: a count of 0 registers nothing",
            entry.file
        );
        assert!(
            entry.why.len() > 40,
            "{}: justification is too short to be one - it is the only thing \
             a future reader has to judge whether this is still right",
            entry.file
        );
        let kinds = ["SERVER RESPONSE", "AUTHORED"];
        assert!(
            kinds.iter().any(|kind| entry.why.contains(kind)),
            "{}: the justification must open by naming the kind ({}), \
             because that is the decision being reviewed",
            entry.file,
            kinds.join(" or ")
        );
    }
}

/// The scan must actually be able to see a call, or every test above passes
/// by finding nothing.
///
/// The specific way this could rot: `code_lines` drops comment lines, and a
/// mistake there that dropped everything would leave the guard green and
/// blind. So assert against a known call site in a real file.
#[test]
fn the_scan_finds_a_call_site_it_is_supposed_to_find() {
    let source =
        std::fs::read_to_string(workspace_root().join("crates/otl/src/commands/docs/view.rs"))
            .unwrap();
    let calls: usize = code_lines(&source)
        .iter()
        .map(|line| count_verbatim(line))
        .sum();
    assert_eq!(
        calls, 1,
        "expected to see the one `render_json` call in docs/view.rs; seeing \
         none means the scan is blind and the other tests here prove nothing"
    );
}

/// The `render::render` scan must see a call too, for the same reason.
#[test]
fn the_render_scan_finds_a_call_site_it_is_supposed_to_find() {
    let source =
        std::fs::read_to_string(workspace_root().join("crates/otl/src/commands/output.rs"))
            .unwrap();
    let calls: usize = code_lines(&source)
        .iter()
        .map(|line| count_render(line))
        .sum();
    assert_eq!(
        calls, 1,
        "expected to see the one `render::render` call in commands/output.rs"
    );
}

/// The `render` scan must see a call however it is spelled.
///
/// The qualifier-only version of `count_render` was blind to the
/// `use`-imported form, which is the spelling a new module is most likely to
/// reach for.
#[test]
fn the_render_scan_sees_every_spelling_of_the_call() {
    for line in [
        "    let rendered = render::render(value, mode, &[])",
        "    let rendered = crate::render::render(value, mode, &[])",
        "    let rendered = render(value, mode, &[])",
    ] {
        assert_eq!(count_render(line), 1, "missed a call in {line:?}");
    }
    for line in [
        "    render::render_json(&payload)",
        "    render::render_json_scrubbed(&value)",
        "    try_render_table(payload, schema)",
        "    stdio::write_data_line(&rendered)",
    ] {
        assert_eq!(count_render(line), 0, "counted a non-call in {line:?}");
    }
}

/// `render_json_scrubbed` must not be counted as `render_json`.
///
/// The plain name is a prefix of the scrubbed one, so the obvious substring
/// search conflates them - and would report three registered scrubbed calls
/// as unregistered verbatim ones.
#[test]
fn the_scrubbed_renderer_is_not_mistaken_for_the_verbatim_one() {
    let scrubbed = "    render::render_json_scrubbed(&output.value)";
    assert_eq!(
        count_verbatim(scrubbed),
        0,
        "counted a scrubbed call as verbatim"
    );
    assert_eq!(count_scrubbed(scrubbed), 1);

    let verbatim = "    let rendered = render::render_json(&payload)";
    assert_eq!(count_verbatim(verbatim), 1);
    assert_eq!(
        count_scrubbed(verbatim),
        0,
        "counted a verbatim call as scrubbed"
    );
}
