//! End-to-end behaviour of `otl skill install`, `otl skill show`, and the
//! `skill` check in `otl doctor`.
//!
//! The unit tests in `commands/skill` own the per-target decisions (what is
//! upgraded, what is refused). What can only be asserted from outside is
//! here: that the whole thing works with no credentials, no instance and no
//! network; that the installed file is the document the binary carries; and
//! that a stale copy turns `otl doctor` into a warning WITHOUT changing its
//! exit code, which is the promise that makes the check safe to add.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use serde_json::Value;

/// The check that keeps the version in one place: the document's own
/// frontmatter is what the CLI reports and what the doctor compares.
fn bundled_version(document: &str) -> String {
    document
        .lines()
        .find_map(|line| line.strip_prefix("version:"))
        .expect("the skill document declares a version")
        .trim()
        .to_string()
}

/// `otl skill show`, which is also how a test gets the expected bytes.
fn shown() -> String {
    let output = common::otl().args(["skill", "show"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn install_json(dir: &std::path::Path, extra: &[&str]) -> (Value, i32) {
    let output = common::otl()
        .args(["skill", "install", "--json", "--dir"])
        .arg(dir)
        .args(extra)
        .output()
        .unwrap();
    let json = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("not JSON: {error}\n{output:?}"));
    (json, output.status.code().unwrap_or(-1))
}

fn doctor_skill_check(skill_dir: &std::path::Path) -> (Value, i32) {
    let output = common::otl()
        .args(["doctor", "--offline", "--json"])
        .env(common::SKILL_DIR_ENV, skill_dir)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("not JSON: {error}\n{output:?}"));
    let check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "skill")
        .expect("the report has a skill check")
        .clone();
    (check, output.status.code().unwrap_or(-1))
}

/// Nothing is fetched and no credential is needed: the document is in the
/// binary, so this works on a machine that has never been configured.
#[test]
fn install_writes_the_bundled_document_and_says_where() {
    let dir = tempfile::tempdir().unwrap();
    let document = shown();
    let version = bundled_version(&document);

    let (report, code) = install_json(dir.path(), &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["version"], Value::from(version.clone()));
    assert_eq!(report["complete"], Value::from(true));
    let target = &report["targets"][0];
    assert_eq!(target["action"], Value::from("installed"));
    assert_eq!(target["source"], Value::from("--dir"));

    let path = dir
        .path()
        .join(report["skill"].as_str().unwrap())
        .join("SKILL.md");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), document);
    assert!(
        target["path"].as_str().unwrap().ends_with("SKILL.md"),
        "{target}"
    );

    // Idempotent: the same command again is a no-op, not a rewrite.
    let (again, code) = install_json(dir.path(), &[]);
    assert_eq!(code, 0);
    assert_eq!(again["targets"][0]["action"], Value::from("unchanged"));
    assert_eq!(
        again["targets"][0]["previous_version"],
        Value::from(version)
    );
}

/// Another skill's document is not collateral damage of an install.
#[test]
fn install_refuses_a_foreign_document_until_forced() {
    let dir = tempfile::tempdir().unwrap();
    let (report, _) = install_json(dir.path(), &[]);
    let path = dir
        .path()
        .join(report["skill"].as_str().unwrap())
        .join("SKILL.md");
    let foreign = "---\nname: someone-elses\nversion: 3.1.4\n---\n\n# Theirs\n";
    std::fs::write(&path, foreign).unwrap();

    let (refused, code) = install_json(dir.path(), &[]);
    assert_eq!(code, 2, "a refusal with nothing installed is a usage error");
    assert_eq!(refused["complete"], Value::from(false));
    assert_eq!(refused["targets"][0]["action"], Value::from("refused"));
    assert!(
        refused["targets"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("someone-elses"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        foreign,
        "the foreign document was modified"
    );

    let (forced, code) = install_json(dir.path(), &["--force"]);
    assert_eq!(code, 0, "{forced}");
    assert_eq!(forced["targets"][0]["action"], Value::from("replaced"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), shown());
}

/// With no skills directory to write to, the command says so rather than
/// creating a directory tree for an agent that is not installed.
#[test]
fn install_without_any_target_is_a_usage_error() {
    let empty = tempfile::tempdir().unwrap();
    let output = common::otl()
        .args(["skill", "install"])
        // An existing but empty directory as the whole "home": no agent
        // home under it, and the env override is what the harness sets.
        .env(common::SKILL_DIR_ENV, "")
        .env("HOME", empty.path())
        .env("USERPROFILE", empty.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--dir"), "{stderr}");
}

/// The reason the check exists: an installed copy that no longer matches
/// the binary is REPORTED, and reporting it never changes the exit code.
#[test]
fn doctor_warns_about_a_stale_copy_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let (report, _) = install_json(dir.path(), &[]);
    let path = dir
        .path()
        .join(report["skill"].as_str().unwrap())
        .join("SKILL.md");
    let version = bundled_version(&shown());

    let (fresh, _) = doctor_skill_check(dir.path());
    assert_eq!(fresh["status"], Value::from("ok"), "{fresh}");
    assert_eq!(fresh["version"], Value::from(version.clone()));
    assert_eq!(fresh["installed"][0]["matches"], Value::from(true));

    let stale = std::fs::read_to_string(&path)
        .unwrap()
        .replace(&format!("version: {version}"), "version: 0.0.1");
    std::fs::write(&path, stale).unwrap();

    let (drifted, code) = doctor_skill_check(dir.path());
    assert_eq!(drifted["status"], Value::from("warn"), "{drifted}");
    assert_eq!(
        drifted["exit_code"],
        Value::Null,
        "a warning blocks nothing"
    );
    assert_eq!(drifted["installed"][0]["version"], Value::from("0.0.1"));
    assert_eq!(drifted["installed"][0]["state"], Value::from("behind"));
    // The remedy belongs to the TARGET, not to the summary: different
    // states are answered by different commands.
    assert_eq!(
        drifted["installed"][0]["remedy"],
        Value::from("run `otl skill install`"),
        "{drifted}"
    );
    assert!(
        drifted["detail"][0]
            .as_str()
            .unwrap()
            .contains("this binary ships"),
        "{drifted}"
    );
    // The run's own code comes from the environment, which has no instance
    // URL here - never from the skill check.
    assert_eq!(code, 2, "the skill check must not decide the exit code");
}

/// An `otl` nobody has installed the skill for is not a broken `otl`.
#[test]
fn doctor_reports_a_missing_skill_without_warning() {
    let dir = tempfile::tempdir().unwrap();
    let (check, _) = doctor_skill_check(dir.path());
    assert_eq!(check["status"], Value::from("ok"), "{check}");
    assert_eq!(check["installed"][0]["version"], Value::Null);
    assert!(
        check["summary"].as_str().unwrap().contains("not installed"),
        "{check}"
    );
}

/// `otl skill show` prints the document itself in every output state: it is
/// the payload, and `otl skill show > SKILL.md` has to produce a file an
/// agent can read.
#[test]
fn show_prints_markdown_even_with_the_json_flag() {
    let document = shown();
    assert!(
        document.starts_with("---\n"),
        "no frontmatter: {document:.40}"
    );
    let output = common::otl()
        .args(["skill", "show", "--json"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap(), document);
}

/// The regression the doctor check was rewritten for: a foreign document at
/// the skill path must be a WARNING whose remedy works, not an `ok` that
/// sends the user at a command which then refuses.
#[test]
fn doctor_and_install_agree_about_a_foreign_document() {
    let dir = tempfile::tempdir().unwrap();
    let (report, _) = install_json(dir.path(), &[]);
    let path = dir
        .path()
        .join(report["skill"].as_str().unwrap())
        .join("SKILL.md");
    std::fs::write(&path, "---\nname: someone-elses\nversion: 3.1.4\n---\n").unwrap();

    let (check, code) = doctor_skill_check(dir.path());
    assert_eq!(check["status"], Value::from("warn"), "{check}");
    assert_eq!(check["exit_code"], Value::Null, "the check must not block");
    assert_eq!(check["installed"][0]["state"], Value::from("foreign"));
    let remedy = check["installed"][0]["remedy"].as_str().unwrap();
    assert!(remedy.contains("--force"), "{remedy}");
    assert_eq!(
        code, 2,
        "the code comes from the environment, not the skill"
    );

    // And the remedy the report gives is the one that works.
    let (forced, code) = install_json(dir.path(), &["--force"]);
    assert_eq!(code, 0, "{forced}");
    assert_eq!(forced["targets"][0]["action"], Value::from("replaced"));
}

/// A symlink where this command creates its own directory is refused: the
/// module claims it, so a test has to hold it. The skills root ABOVE it may
/// still be a symlink, which the second half checks.
#[cfg(unix)]
#[test]
fn a_symlinked_skill_directory_is_refused_but_a_symlinked_root_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("skills");
    std::fs::create_dir_all(&root).unwrap();
    let victim = dir.path().join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::os::unix::fs::symlink(&victim, root.join("outline-cli")).unwrap();

    let (report, code) = install_json(&root, &["--force"]);
    assert_eq!(code, 2, "--force must not extend to a symlinked directory");
    assert_eq!(report["targets"][0]["action"], Value::from("refused"));
    assert!(
        report["targets"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("symlink"),
        "{report}"
    );
    assert!(
        !victim.join("SKILL.md").exists(),
        "the write followed the symlink"
    );

    // The root itself being a symlink is ordinary (a dotfiles checkout).
    let real_root = dir.path().join("real-skills");
    std::fs::create_dir_all(&real_root).unwrap();
    let linked_root = dir.path().join("linked-skills");
    std::os::unix::fs::symlink(&real_root, &linked_root).unwrap();
    let (through_link, code) = install_json(&linked_root, &[]);
    assert_eq!(code, 0, "{through_link}");
    assert!(real_root.join("outline-cli/SKILL.md").exists());
}

/// `--dir` pointing at a FILE is a usage error, and the reason has to name
/// the actual fix rather than telling the user to delete their file.
#[test]
fn install_into_a_dir_that_is_a_file_says_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("skills.txt");
    std::fs::write(&not_a_dir, "not a directory").unwrap();

    let (report, code) = install_json(&not_a_dir, &[]);
    assert_eq!(code, 2, "{report}");
    let reason = report["targets"][0]["reason"].as_str().unwrap();
    assert!(reason.contains("--dir"), "{reason}");
    assert!(
        !reason.contains("remove it by hand"),
        "wrong remedy for a --dir that names a file: {reason}"
    );
    assert!(
        !reason.contains("os error"),
        "a raw platform errno reached the user: {reason}"
    );
    assert_eq!(
        std::fs::read_to_string(&not_a_dir).unwrap(),
        "not a directory"
    );
}

/// A hostile installed copy cannot flood a report: its frontmatter is
/// foreign text, so it is scrubbed and capped like every other name.
#[test]
fn a_huge_foreign_skill_name_is_capped_in_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let (report, _) = install_json(dir.path(), &[]);
    let path = dir
        .path()
        .join(report["skill"].as_str().unwrap())
        .join("SKILL.md");
    let huge = "z".repeat(200_000);
    std::fs::write(&path, format!("---\nname: {huge}\nversion: 1.0.0\n---\n")).unwrap();

    let (refused, code) = install_json(dir.path(), &[]);
    assert_eq!(code, 2);
    let reason = refused["targets"][0]["reason"].as_str().unwrap();
    assert!(
        reason.len() < 500,
        "unbounded foreign text: {} bytes",
        reason.len()
    );

    let (check, _) = doctor_skill_check(dir.path());
    let state = check["installed"][0]["state"].as_str().unwrap();
    assert_eq!(state, "foreign");
    let detail = check["detail"][0].as_str().unwrap();
    assert!(
        detail.len() < 500,
        "unbounded foreign text: {} bytes",
        detail.len()
    );
}
