//! End-to-end `otl doctor`: the ENVIRONMENT half (Story 4.3).
//!
//! Config file, instance URL, credential file, the credential a request
//! would carry, and whether the instance answers. The spec half - which
//! operation table is in use, and how it differs from the online API
//! description - is `doctor_spec_e2e.rs`.
//!
//! Every case runs the real binary against wiremock servers and throwaway
//! directories (`common::doctor`): no test touches the real network, the
//! developer's credential file, or their spec cache.
//!
//! Two properties get most of the attention here, because they are the ones
//! a report can get wrong while still looking right:
//!
//! 1. **the exit code names the right check**, so a script can act on it;
//! 2. **a request is made only when it should be** - the mock servers are
//!    asked how many requests they received, which is the only way to tell
//!    "skipped" from "silently succeeded".

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;

mod common;
use common::doctor::{check, hits, instance, instance_answering, parse, run, spec_host};
use common::doctor::{document, Env, DEAD, KNOWN_OP, SPEC_PATH};

#[tokio::test(flavor = "multi_thread")]
async fn a_working_environment_reports_every_check_and_exits_zero() {
    let api = instance(200).await;
    let specs = spec_host(document(&[KNOWN_OP])).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(
        Some(&api.uri()),
        Some("test-key"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    assert_eq!(code, 0, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(report["healthy"], Value::from(true), "{report}");
    assert_eq!(report["exit_code"], Value::from(0));
    assert_eq!(report["problems"], Value::from(0));

    // The instance was really contacted, through the request channel, and
    // the identity it returned is in the report.
    let connectivity = check(&report, "connectivity");
    assert_eq!(connectivity["status"], Value::from("ok"), "{connectivity}");
    assert_eq!(connectivity["reachable"], Value::from(true));
    assert_eq!(
        connectivity["account"],
        Value::from("Alice Example <alice@example.com>")
    );
    assert_eq!(connectivity["workspace"], Value::from("Acme"));
    assert_eq!(hits(&api).await, 1, "the probe must be exactly one request");

    // The credential the gate approved is named, and it is the environment
    // key - not something doctor read on its own.
    assert!(
        check(&report, "credential")["method"]
            .as_str()
            .unwrap()
            .contains("OUTLINE_API_KEY"),
        "{report}"
    );
    // And the local table is described.
    let local = check(&report, "local-spec");
    assert_eq!(local["synced"], Value::from(false));
    assert!(local["operations"].as_u64().unwrap() > 100, "{local}");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_credential_anywhere_exits_two_and_never_contacts_the_instance() {
    let api = instance(200).await;
    let env = Env::new();
    let (stdout, stderr, code) = run(env.command(Some(&api.uri()), None, &["--offline"])).await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    assert_eq!(report["healthy"], Value::from(false));
    assert_eq!(report["exit_code"], Value::from(2));

    // The credential check is the blocking one, and it names every way in.
    let credential = check(&report, "credential");
    assert_eq!(credential["status"], Value::from("problem"), "{credential}");
    assert_eq!(credential["exit_code"], Value::from(2));
    let detail = credential["detail"].to_string();
    assert!(detail.contains("otl auth login"), "{detail}");
    assert!(detail.contains("otl auth set-key"), "{detail}");
    // The finding reaches stderr too, since that is what `main` prints.
    assert!(stderr.contains("credential"), "{stderr}");
    // Everything local is still reported rather than abandoned.
    assert_eq!(check(&report, "instance")["status"], Value::from("ok"));
    assert_eq!(hits(&api).await, 0, "nothing may be sent without one");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_instance_exits_seven_from_the_connectivity_check() {
    let env = Env::new();
    let (stdout, stderr, code) = run(env.command(Some(DEAD), Some("test-key"), &[])).await;

    assert_eq!(code, 7, "{stdout}{stderr}");
    let report = parse(&stdout);
    let connectivity = check(&report, "connectivity");
    assert_eq!(connectivity["status"], Value::from("problem"), "{report}");
    assert_eq!(connectivity["exit_code"], Value::from(7));
    assert_eq!(connectivity["reachable"], Value::from(false));
    // Nothing local is blamed for it: the code came from the network check.
    for local in ["configuration", "instance", "credentials", "credential"] {
        assert_ne!(
            check(&report, local)["status"],
            Value::from("problem"),
            "{local} must not be a problem here: {report}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_credential_exits_four() {
    let api = instance(401).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(Some(&api.uri()), Some("test-key"), &[])).await;

    assert_eq!(code, 4, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(
        check(&report, "connectivity")["exit_code"],
        Value::from(4),
        "{report}"
    );
}

/// A credential FILE other users can read is exit 2, and nothing is sent.
///
/// The invariant is deliberately about the FILE and not about "a compromised
/// store": `secret_file::read_checked` refuses a widened file on the open
/// descriptor, so every command - `doctor` included - stops there. A
/// world-writable DIRECTORY around a sound file is a different case with a
/// different grade, pinned by
/// `a_writable_directory_around_a_sound_file_is_a_warning` below.
///
/// The environment key is exported ON PURPOSE: without it, "nothing was
/// sent" would hold simply because there was nothing to send, and the test
/// would pass even if `doctor` decided to ignore the unusable file. With it,
/// there IS a usable credential and the run still has to refuse to use one.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_over_wide_credential_file_exits_two_before_anything_is_sent() {
    use std::os::unix::fs::PermissionsExt;

    let api = instance(200).await;
    let env = Env::new();
    env.seed_key(&api.uri(), "KEY-SECRET-9c7a");
    let file = env.dir().join("credentials.toml");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), Some("ENV-SECRET-3f1b"), &[])).await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let credentials = check(&report, "credentials");
    assert_eq!(credentials["status"], Value::from("problem"), "{report}");
    assert_eq!(credentials["file_usable"], Value::from(false));
    // The offending file and its mode are named, with the fix.
    let text = credentials.to_string();
    assert!(text.contains(&file.display().to_string()), "{text}");
    assert!(text.contains("0644"), "{text}");
    assert!(text.contains("chmod 600"), "{text}");
    assert_eq!(
        hits(&api).await,
        0,
        "a credential file other users can read must not be sent"
    );
    // And no part of the credential is anywhere in the output.
    for secret in ["SECRET-9c7a", "SECRET-3f1b"] {
        assert!(!stdout.contains(secret), "{secret} in stdout: {stdout}");
        assert!(!stderr.contains(secret), "{secret} in stderr: {stderr}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_contacts_neither_the_instance_nor_the_spec_host() {
    let api = instance(200).await;
    let specs = spec_host(document(&[KNOWN_OP])).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(
        Some(&api.uri()),
        Some("test-key"),
        &[
            "--offline",
            "--spec-url",
            &format!("{}{SPEC_PATH}", specs.uri()),
        ],
    ))
    .await;

    assert_eq!(code, 0, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(
        check(&report, "connectivity")["status"],
        Value::from("skipped"),
        "{report}"
    );
    assert_eq!(
        check(&report, "online-spec")["status"],
        Value::from("skipped")
    );
    assert_eq!(check(&report, "online-spec")["checked"], Value::from(false));
    assert_eq!(hits(&api).await, 0, "--offline sent a request");
    assert_eq!(hits(&specs).await, 0, "--offline fetched a document");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_report_never_carries_a_credential_or_a_fragment_of_one() {
    let api = instance(200).await;
    let specs = spec_host(document(&[KNOWN_OP])).await;
    let env = Env::new();
    env.seed_key(&api.uri(), "STORED-SECRET-9c7a");
    let (stdout, stderr, code) = run(env.command(
        Some(&api.uri()),
        Some("ENV-SECRET-3f1b"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    assert_eq!(code, 0, "{stdout}{stderr}");
    let both = format!("{stdout}\n{stderr}");
    for secret in [
        "STORED-SECRET-9c7a",
        "ENV-SECRET-3f1b",
        "SECRET",
        "9c7a",
        "3f1b",
    ] {
        assert!(
            !both.contains(secret),
            "{secret} appears in doctor output: {both}"
        );
    }
    // The report still SAYS what is stored, which is the point of it.
    let report = parse(&stdout);
    assert!(
        check(&report, "credentials")["profiles_stored"]
            .to_string()
            .contains("api key"),
        "{report}"
    );
}

/// A config file that cannot be parsed is the FIRST thing to fix, so it is
/// the check that decides the exit code even though later checks fail too.
#[tokio::test(flavor = "multi_thread")]
async fn an_unparsable_config_file_exits_two_from_the_configuration_check() {
    let env = Env::new();
    let bad = env.dir().join("broken.toml");
    std::fs::write(&bad, "default_profile = \n[profiles.work]\n").unwrap();
    let (stdout, stderr, code) = run(env.command(
        None,
        Some("test-key"),
        &["--offline", "--config", &bad.display().to_string()],
    ))
    .await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let configuration = check(&report, "configuration");
    assert_eq!(
        configuration["status"],
        Value::from("problem"),
        "{configuration}"
    );
    assert_eq!(configuration["exit_code"], Value::from(2));
    // It is the FIRST check, so it is the one that decided the exit code
    // even though the instance check failed for the same reason.
    assert_eq!(report["checks"][0]["check"], Value::from("configuration"));
    assert!(
        configuration["detail"].to_string().contains("line 1"),
        "the parse error must locate itself: {configuration}"
    );
    assert!(stderr.contains("configuration"), "{stderr}");
}

/// A world-writable DIRECTORY around a sound 0600 file is a WARNING, the run
/// exits 0, and the credential in that file is used as usual.
///
/// This is the R1 decision, and the test exists so it cannot be quietly
/// reverted: `require_regular_owned` checks the OPEN descriptor for the
/// caller's own uid, so another user cannot plant a file this CLI would read;
/// the file is unreadable to them anyway; and Story 2.6 deliberately does not
/// re-permission an existing directory. What is left is nuisance, not a
/// confidentiality or integrity failure - and a warning never changes the
/// exit code, which is right because no other command fails in this state
/// either.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_writable_directory_around_a_sound_file_is_a_warning() {
    use std::os::unix::fs::PermissionsExt;

    let api = instance(200).await;
    let env = Env::new();
    env.seed_key(&api.uri(), "KEY-SECRET-9c7a");
    // The file keeps the 0600 the store created; only the directory is opened up.
    std::fs::set_permissions(env.dir(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), Some("ENV-SECRET-3f1b"), &["--offline"])).await;
    let restore = std::fs::set_permissions(env.dir(), std::fs::Permissions::from_mode(0o700));

    assert_eq!(
        code, 0,
        "a directory problem must not block: {stdout}{stderr}"
    );
    let report = parse(&stdout);
    let credentials = check(&report, "credentials");
    assert_eq!(credentials["status"], Value::from("warn"), "{credentials}");
    // The FILE is fine, and the report says so rather than calling the store
    // unusable while going on to use it.
    assert_eq!(credentials["file_usable"], Value::from(true));
    let problem = credentials["directory_problem"]
        .as_str()
        .unwrap_or_default();
    assert!(
        problem.contains("0777"),
        "the actual mode must be reported: {credentials}"
    );
    // R2 F1: the text has to agree with what the run then DOES. "refusing to
    // use it" is true of the write path, which is where that sentence comes
    // from; the read path this check describes does not look at the
    // directory at all, and `connectivity` below used the file.
    assert!(
        !problem.contains("refusing"),
        "the report must not claim a refusal it did not perform: {problem}"
    );
    let detail = credentials["detail"].to_string();
    assert!(
        !detail.contains("usable:"),
        "the store-wide verdict must not appear beside a check that used the \
         file: {detail}"
    );
    assert!(
        !detail.contains("refusing"),
        "the detail must not claim a refusal either: {detail}"
    );
    // And the credential in that file is the one a request would carry.
    assert_eq!(
        check(&report, "credential")["status"],
        Value::from("ok"),
        "{report}"
    );
    assert!(!stdout.contains("SECRET-9c7a"), "{stdout}");
    restore.unwrap();
}

/// The same case with the network on: the credential is actually used, which
/// is the half `--offline` cannot show.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_writable_directory_does_not_stop_the_instance_from_being_checked() {
    use std::os::unix::fs::PermissionsExt;

    let api = instance(200).await;
    let env = Env::new();
    env.seed_key(&api.uri(), "KEY-SECRET-9c7a");
    std::fs::set_permissions(env.dir(), std::fs::Permissions::from_mode(0o777)).unwrap();

    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), None, &["--spec-url", DEAD])).await;
    let restore = std::fs::set_permissions(env.dir(), std::fs::Permissions::from_mode(0o700));

    assert_eq!(code, 0, "{stdout}{stderr}");
    let report = parse(&stdout);
    assert_eq!(check(&report, "credentials")["status"], Value::from("warn"));
    assert_eq!(
        check(&report, "connectivity")["status"],
        Value::from("ok"),
        "{report}"
    );
    assert_eq!(
        hits(&api).await,
        1,
        "the sound file's credential must still be used"
    );
    restore.unwrap();
}

/// An instance that answers 200 with something that is not JSON: exit 1, and
/// the report must NOT claim the instance could not be reached - it was
/// reached, and it answered.
#[tokio::test(flavor = "multi_thread")]
async fn an_instance_answering_with_something_other_than_json_exits_one() {
    let api = instance_answering("<html>captive portal</html>").await;
    let env = Env::new();
    // The instance must be contacted, so this cannot be `--offline`; the
    // spec source is pointed at a dead port instead, which is a warning and
    // therefore does not compete for the exit code.
    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), Some("test-key"), &["--spec-url", DEAD])).await;

    assert_eq!(code, 1, "{stdout}{stderr}");
    let report = parse(&stdout);
    let connectivity = check(&report, "connectivity");
    assert_eq!(connectivity["status"], Value::from("problem"), "{report}");
    assert_eq!(connectivity["exit_code"], Value::from(1));
    // Reached, and it answered: saying otherwise sends the user to look at
    // their network for a problem that is on the server.
    assert_eq!(connectivity["reachable"], Value::from(true));
    let summary = connectivity["summary"].as_str().unwrap_or_default();
    assert!(
        !summary.contains("could not be reached"),
        "the instance answered; the summary must not deny it: {summary}"
    );
    assert!(summary.contains("answered"), "{summary}");
    assert!(stderr.contains("connectivity"), "{stderr}");
    assert_eq!(hits(&api).await, 1);
}

/// A dangling symlink where the credential file belongs: something IS at
/// that path, and every read of it fails. The report must not call the path
/// empty and healthy while the credential check refuses it.
///
/// The two checks used to disagree here: `permissions()` followed the link,
/// got `NotFound`, and reported "file does not exist yet / nothing stored
/// yet", while `read_checked`'s `O_NOFOLLOW` open failed and made
/// `credential` a problem.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_dangling_credential_symlink_is_reported_as_a_file_problem() {
    let api = instance(200).await;
    let env = Env::new();
    std::os::unix::fs::symlink(
        env.dir().join("nowhere"),
        env.dir().join("credentials.toml"),
    )
    .unwrap();

    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), Some("test-key"), &["--offline"])).await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let credentials = check(&report, "credentials");
    assert_eq!(credentials["status"], Value::from("problem"), "{report}");
    assert_eq!(credentials["file_usable"], Value::from(false));
    // Not "there is nothing there": there is, and it cannot be used.
    assert_eq!(credentials["credential_file_exists"], Value::from(true));
    let text = credentials.to_string();
    assert!(text.contains("symbolic link"), "{text}");
    assert!(
        !text.contains("nothing stored yet"),
        "the path is not empty: {text}"
    );
    // And the two checks agree, which is the whole point.
    assert_eq!(
        check(&report, "credential")["status"],
        Value::from("problem"),
        "{report}"
    );
    assert_eq!(hits(&api).await, 0);
}

/// A credential that cannot be turned into an HTTP header: the request is
/// never built, so nothing is sent - and the report must not say the
/// instance answered.
///
/// A newline in `OUTLINE_API_KEY` is the failure mode `INVALID_REQUEST_HINT`
/// exists for. `auth set-key` refuses such a key, but an exported variable
/// is not validated, so the probe is where it surfaces.
#[tokio::test(flavor = "multi_thread")]
async fn a_credential_that_cannot_be_sent_is_not_reported_as_an_answer() {
    let api = instance(200).await;
    let env = Env::new();
    // Not `--offline`: the probe has to be attempted for this to be about
    // the probe. The spec source is a dead port, which is only a warning.
    let (stdout, stderr, code) = run(env.command(
        Some(&api.uri()),
        Some("KEY-SECRET-3f1b\nsecond-line"),
        &["--spec-url", DEAD],
    ))
    .await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let connectivity = check(&report, "connectivity");
    assert_eq!(connectivity["status"], Value::from("problem"), "{report}");
    assert_eq!(connectivity["exit_code"], Value::from(2));
    // The two assertions that matter, and the mock is what makes the second
    // one mean something.
    assert_eq!(connectivity["reachable"], Value::from(false));
    assert_eq!(
        hits(&api).await,
        0,
        "nothing can have been sent: the request could not be built"
    );
    let summary = connectivity["summary"].as_str().unwrap_or_default();
    assert!(
        summary.contains("nothing was sent"),
        "the summary must say so: {summary}"
    );
    assert!(
        !summary.contains("answered"),
        "no answer was received: {summary}"
    );
    // The key is not echoed anywhere, in any form.
    for stream in [&stdout, &stderr] {
        assert!(!stream.contains("SECRET-3f1b"), "the key leaked: {stream}");
        assert!(!stream.contains("second-line"), "the key leaked: {stream}");
    }
}
