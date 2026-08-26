//! Architectural constraints that no functional test can catch.
//!
//! `project-context.md` states these as hard rules, not style preferences:
//! files under 800 lines, functions under 50, nesting under 4. A module that
//! outgrows any of them keeps compiling and keeps passing its own tests, so
//! nothing notices until a reviewer counts by hand - which is how the file
//! limit was first breached, and then how the function limit was breached
//! by the very commit that added a guard for files only.
//!
//! The lesson from that is the reason this file checks all three: a rule
//! without a machine to enforce it is a rule that gets broken during the
//! next refactor, including by whoever just fixed it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Hard limits from `project-context.md` ("文件 <800 行... 函数 <50 行，
/// 嵌套 <4 层").
const MAX_SOURCE_LINES: usize = 800;
const MAX_FUNCTION_LINES: usize = 50;
const MAX_NESTING_DEPTH: usize = 4;

/// Size at which a file should be split before it becomes a problem.
/// Reported, not enforced: the rule is the limit above.
const ADVISORY_SOURCE_LINES: usize = 700;

#[test]
fn no_source_file_exceeds_the_size_limit() {
    let mut violations = Vec::new();
    let mut approaching = Vec::new();
    for file in source_files() {
        let lines = read(&file).lines().count();
        if lines > MAX_SOURCE_LINES {
            violations.push(format!("  {}: {lines} lines", show(&file)));
        } else if lines > ADVISORY_SOURCE_LINES {
            approaching.push(format!("  {}: {lines} lines", show(&file)));
        }
    }
    if !approaching.is_empty() {
        eprintln!(
            "note: approaching the {MAX_SOURCE_LINES}-line limit:\n{}",
            approaching.join("\n")
        );
    }
    assert!(
        violations.is_empty(),
        "these files exceed the {MAX_SOURCE_LINES}-line limit from \
         project-context.md; split them by responsibility rather than \
         raising the limit:\n{}",
        violations.join("\n")
    );
}

/// Function length and nesting are enforced on PRODUCTION code.
///
/// Test bodies are exempt, and the reason is not convenience: the rule
/// exists to bound how much has to be held in the head while CHANGING a
/// function. A test is a linear script - arrange, act, assert - with no
/// branching to follow, and breaking one into helpers usually costs the
/// reader the narrative that made it reviewable. File size still applies to
/// tests, because navigating a 1500-line file is hard whatever is in it.
///
/// This narrowing is deliberate and stated here rather than left implicit;
/// if it is the wrong call, it is one line to widen.
#[test]
fn no_production_function_exceeds_the_length_or_nesting_limit() {
    let mut too_long = Vec::new();
    let mut too_deep = Vec::new();
    for file in source_files()
        .into_iter()
        .filter(|file| is_production(file))
    {
        for function in functions_in(&production_part(&read(&file))) {
            let where_ = format!("  {}:{} {}", show(&file), function.line, function.name);
            if function.lines > MAX_FUNCTION_LINES {
                too_long.push(format!("{where_} - {} lines", function.lines));
            }
            if function.depth > MAX_NESTING_DEPTH {
                too_deep.push(format!("{where_} - nested {} deep", function.depth));
            }
        }
    }
    assert!(
        too_long.is_empty(),
        "these functions exceed the {MAX_FUNCTION_LINES}-line limit from \
         project-context.md; extract the parts that have their own name:\n{}",
        too_long.join("\n")
    );
    assert!(
        too_deep.is_empty(),
        "these functions nest deeper than {MAX_NESTING_DEPTH}; use early \
         returns or extract the inner block:\n{}",
        too_deep.join("\n")
    );
}

/// Whether a path is production code rather than a test target.
fn is_production(file: &Path) -> bool {
    file.components().any(|part| part.as_os_str() == "src")
}

/// The part of a source file before its `#[cfg(test)]` module.
///
/// Unit tests live inside `src/` files here, and they are tests: the
/// function rules stop where they begin.
fn production_part(source: &str) -> String {
    match source.find("#[cfg(test)]") {
        Some(cut) => source[..cut].to_string(),
        None => source.to_string(),
    }
}

/// One function found by [`functions_in`].
struct Function {
    name: String,
    line: usize,
    lines: usize,
    depth: usize,
}

/// Find every function in a source file, with its length and nesting depth.
///
/// A brace counter, not a parser: enough for a guard, and it keeps a `syn`
/// dependency out of the test tree. Strings, chars and comments are blanked
/// first so braces inside them cannot throw the count off, which matters on
/// these heavily-commented sources.
///
/// Functions are tracked at ANY depth, so methods inside `impl` blocks are
/// measured too - they are most of the code here, and a scanner that only
/// saw free functions would report a reassuring nothing.
fn functions_in(source: &str) -> Vec<Function> {
    let mut found = Vec::new();
    let mut open: Option<(Function, usize)> = None;
    let mut depth: usize = 0;
    for (index, raw) in source.lines().enumerate() {
        let line = strip_literals(raw);
        if open.is_none() {
            if let Some(name) = function_name(&line) {
                open = Some((
                    Function {
                        name,
                        line: index + 1,
                        lines: 0,
                        depth: 0,
                    },
                    depth,
                ));
            }
        }
        let before = depth;
        depth = next_depth(depth, &line);
        let Some((function, base)) = open.as_mut() else {
            continue;
        };
        function.lines += 1;
        // Depth INSIDE the body: the function's own brace is level 0.
        function.depth = function.depth.max(depth.saturating_sub(*base + 1));
        // A signature-only line has not opened the body yet.
        let body_started = before > *base || depth > *base;
        if body_started && depth <= *base {
            if let Some((finished, _)) = open.take() {
                found.push(finished);
            }
        }
    }
    found
}

/// Apply one line's braces to the running depth, never going below zero.
fn next_depth(depth: usize, line: &str) -> usize {
    let opened = line.matches('{').count();
    let closed = line.matches('}').count();
    (depth + opened).saturating_sub(closed)
}

/// The name of a function declared on this line, if any.
///
/// Matches at any indentation, so `impl` methods count. A multi-line
/// signature is fine: the body is delimited by brace depth, not by this.
fn function_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = [
        "pub fn ",
        "pub(crate) fn ",
        "pub(super) fn ",
        "pub async fn ",
        "async fn ",
        "fn ",
    ]
    .iter()
    .find_map(|prefix| trimmed.strip_prefix(prefix))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Blank out string/char literals and line comments so their braces do not
/// affect the depth counter.
fn strip_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' if in_string => {
                chars.next();
            }
            '"' => in_string = !in_string,
            '/' if !in_string && chars.peek() == Some(&'/') => break,
            _ if in_string => {}
            _ => out.push(c),
        }
    }
    out
}

/// Read a file, tolerating anything unreadable as empty.
fn read(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap_or_default()
}

/// Path relative to the workspace root, for readable failures.
fn show(file: &Path) -> String {
    file.strip_prefix(workspace_root())
        .unwrap_or(file)
        .display()
        .to_string()
}

/// Collect `crates/*/src/**/*.rs` and `crates/*/tests/**/*.rs`.
///
/// Tests are included deliberately: `project-context.md` grants them no
/// exemption, and a 1500-line test file is exactly as hard to navigate as a
/// 1500-line module.
fn source_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates_dir = workspace_root().join("crates");
    for entry in std::fs::read_dir(&crates_dir).unwrap() {
        let krate = entry.unwrap().path();
        for area in ["src", "tests"] {
            let dir = krate.join(area);
            if dir.is_dir() {
                collect_rs_files(&dir, &mut files);
            }
        }
    }
    assert!(
        !files.is_empty(),
        "found no sources under {}: the guard would pass vacuously",
        crates_dir.display()
    );
    files
}

/// Recursively push every `.rs` file under `dir` into `out`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<name> sits two levels below the workspace root")
        .to_path_buf()
}

#[cfg(test)]
mod self_tests {
    use super::*;

    #[test]
    fn the_function_scanner_measures_length_and_depth() {
        let source = "\
fn short() {
    let x = 1;
}

fn deep() {
    if a {
        for b in c {
            while d {
                if e {
                    call();
                }
            }
        }
    }
}
";
        let found = functions_in(source);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["short", "deep"]);
        assert_eq!(found[0].lines, 3);
        assert!(
            found[1].depth >= 4,
            "expected deep nesting, measured {}",
            found[1].depth
        );
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_confuse_the_scanner() {
        let source = "\
fn tricky() {
    let s = \"{{{ unbalanced\";
    // } not a real close
    done();
}

fn after() {
    ok();
}
";
        let found = functions_in(source);
        let names: Vec<&str> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["tricky", "after"],
            "a literal brace broke the scan"
        );
    }
}
