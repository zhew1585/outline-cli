//! Story 4.1 end-to-end: a named profile actually points the request at
//! that profile's instance, and the precedence order holds at the process
//! level (not just in the resolver).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `otl` with every configuration input scrubbed: each test states its own
/// layers, and the developer's real config file is never read.
fn otl() -> Command {
    let mut cmd = Command::cargo_bin("otl").unwrap();
    // Every OUTLINE_* variable, not just the four fixed ones: per-profile
    // API keys are named after the profile, so a developer with
    // OUTLINE_API_KEY_WORK exported must not change what these tests prove.
    for (name, _) in std::env::vars() {
        if name.starts_with("OUTLINE_") {
            cmd.env_remove(name);
        }
    }
    cmd
}

/// Write a config file into a temp dir; the guard keeps it alive.
fn config_file(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, body).unwrap();
    (dir, path)
}

/// A mock instance that answers `documents.info` ONLY when the bearer token
/// is its own.
///
/// This is the assertion that matters for profile switching: a server that
/// accepted any token would let one workspace's key be sent to another and
/// still pass. A request carrying the wrong token gets 401, which the CLI
/// surfaces as exit code 4.
async fn instance(marker: &str, expected_token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .and(header(
            "authorization",
            &*format!("Bearer {expected_token}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "doc-1", "title": marker }
        })))
        .mount(&server)
        .await;
    // Anything else reaching this instance is a credential sent to the wrong
    // place; answer 401 so the test fails loudly instead of silently.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "message": "wrong credential for this instance"
        })))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_flag_sends_the_requests_instance_its_own_credential() {
    let work = instance("from-work", "key-for-work").await;
    let personal = instance("from-personal", "key-for-personal").await;
    let (_dir, config) = config_file(&format!(
        "default_profile = \"personal\"\n\
         [profiles.work]\nurl = \"{}\"\n\
         [profiles.personal]\nurl = \"{}\"\n",
        work.uri(),
        personal.uri()
    ));

    let assert_profile = |profile: &str, expected: &str| {
        let config = config.clone();
        let profile = profile.to_string();
        let expected = expected.to_string();
        tokio::task::spawn_blocking(move || {
            otl()
                // Both keys are exported, as they would be in a shell used
                // for two workspaces; each profile must pick its own.
                .env("OUTLINE_API_KEY_WORK", "key-for-work")
                .env("OUTLINE_API_KEY_PERSONAL", "key-for-personal")
                .args([
                    "--config",
                    config.to_str().unwrap(),
                    "--profile",
                    &profile,
                    "api",
                    "documents.info",
                    "id=doc-1",
                ])
                .assert()
                .success()
                .stdout(predicate::str::contains(expected));
        })
    };
    assert_profile("work", "from-work").await.unwrap();
    assert_profile("personal", "from-personal").await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn one_profiles_key_is_never_sent_to_another_profiles_instance() {
    // The R1 finding: with only work's key exported (as the GLOBAL variable
    // or as work's own), `--profile personal` must not reach personal's
    // instance with it. The mock records every request, so the test can
    // assert nothing arrived at all.
    let personal = instance("from-personal", "key-for-personal").await;
    let (_dir, config) = config_file(&format!(
        "[profiles.work]\nurl = \"http://127.0.0.1:9\"\n\
         [profiles.personal]\nurl = \"{}\"\n",
        personal.uri()
    ));
    let config_arg = config.to_str().unwrap().to_string();

    let output = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY", "key-for-work")
            .env("OUTLINE_API_KEY_WORK", "key-for-work")
            .args([
                "--config",
                &config_arg,
                "--profile",
                "personal",
                "api",
                "documents.info",
                "id=doc-1",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("OUTLINE_API_KEY_PERSONAL"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("key-for-work"), "key echoed: {stderr}");
    // Nothing was sent: the refusal happens before the request channel.
    let received = personal.received_requests().await.unwrap_or_default();
    assert!(
        received.is_empty(),
        "{} request(s) reached the other instance",
        received.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_env_var_selects_the_instance_and_the_flag_outranks_it() {
    let work = instance("from-work", "key-for-work").await;
    let personal = instance("from-personal", "key-for-personal").await;
    let (_dir, config) = config_file(&format!(
        "[profiles.work]\nurl = \"{}\"\n[profiles.personal]\nurl = \"{}\"\n",
        work.uri(),
        personal.uri()
    ));
    let config_arg = config.to_str().unwrap().to_string();

    let by_env = config_arg.clone();
    tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY_WORK", "key-for-work")
            .env("OUTLINE_API_KEY_PERSONAL", "key-for-personal")
            .env("OUTLINE_PROFILE", "work")
            .args(["--config", &by_env, "api", "documents.info", "id=doc-1"])
            .assert()
            .success()
            .stdout(predicate::str::contains("from-work"));
    })
    .await
    .unwrap();

    tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY_WORK", "key-for-work")
            .env("OUTLINE_API_KEY_PERSONAL", "key-for-personal")
            .env("OUTLINE_PROFILE", "work")
            .args([
                "--config",
                &config_arg,
                "--profile",
                "personal",
                "api",
                "documents.info",
                "id=doc-1",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("from-personal"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn default_profile_applies_with_no_flag_and_no_env_var() {
    let personal = instance("from-personal", "key-for-personal").await;
    let (_dir, config) = config_file(&format!(
        "default_profile = \"personal\"\n[profiles.personal]\nurl = \"{}\"\n",
        personal.uri()
    ));
    let config_arg = config.to_str().unwrap().to_string();
    tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY_PERSONAL", "key-for-personal")
            .args(["--config", &config_arg, "api", "documents.info", "id=doc-1"])
            .assert()
            .success()
            .stdout(predicate::str::contains("from-personal"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_url_flag_outranks_the_env_var_and_the_profile() {
    let chosen = instance("from-flag", "key-for-work").await;
    let (_dir, config) = config_file("[profiles.work]\nurl = \"http://127.0.0.1:9\"\n");
    let config_arg = config.to_str().unwrap().to_string();
    let uri = chosen.uri();
    tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY_WORK", "key-for-work")
            .env("OUTLINE_URL", "http://127.0.0.1:9")
            .args([
                "--config",
                &config_arg,
                "--profile",
                "work",
                "--url",
                &uri,
                "api",
                "documents.info",
                "id=doc-1",
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("from-flag"));
    })
    .await
    .unwrap();
}

#[test]
fn an_unknown_profile_exits_2_before_any_network_request() {
    let (_dir, config) = config_file("[profiles.work]\nurl = \"http://127.0.0.1:9\"\n");
    otl()
        .env("OUTLINE_API_KEY", "test-key")
        .env("OUTLINE_API_KEY_WORK", "test-key")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--profile",
            "nope",
            "api",
            "documents.info",
            "id=doc-1",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown profile"))
        .stderr(predicate::str::contains("work"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn a_malformed_config_file_exits_2_with_a_readable_error() {
    let (_dir, config) = config_file("[profiles.work\nurl = 1\n");
    otl()
        .env("OUTLINE_API_KEY", "test-key")
        .args([
            "--config",
            config.to_str().unwrap(),
            "api",
            "documents.info",
            "id=doc-1",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("is not valid"))
        .stderr(predicate::str::contains("line 1"));
}

#[test]
fn a_credential_in_the_config_file_exits_2_and_is_never_echoed() {
    let (_dir, config) = config_file(
        "[profiles.work]\nurl = \"http://127.0.0.1:9\"\napi_key = \"leaked-secret-value\"\n",
    );
    let output = otl()
        .env("OUTLINE_API_KEY", "test-key")
        .args([
            "--config",
            config.to_str().unwrap(),
            "api",
            "documents.info",
            "id=doc-1",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        !stderr.contains("leaked-secret-value") && !stdout.contains("leaked-secret-value"),
        "secret echoed: {stderr}"
    );
    assert!(stderr.contains("credentials.toml"), "stderr: {stderr}");
}

#[test]
fn a_missing_explicit_config_file_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.toml");
    otl()
        .env("OUTLINE_URL", "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", "test-key")
        .args([
            "--config",
            missing.to_str().unwrap(),
            "api",
            "documents.info",
            "id=doc-1",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("nope.toml"));
}

#[test]
fn an_empty_config_override_pins_the_run_to_environment_variables() {
    // `OUTLINE_CONFIG=` means "read no config file"; with no URL anywhere
    // the run fails on configuration, proving no file was consulted.
    otl()
        .env("OUTLINE_CONFIG", "")
        .env("OUTLINE_API_KEY", "test-key")
        .args(["api", "documents.info", "id=doc-1"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("OUTLINE_URL"));
}

#[test]
fn an_oauth_profile_reports_that_only_api_keys_are_wired_up() {
    let (_dir, config) =
        config_file("[profiles.work]\nurl = \"http://127.0.0.1:9\"\nauth = \"oauth\"\n");
    otl()
        .env("OUTLINE_API_KEY_WORK", "test-key")
        .args([
            "--config",
            config.to_str().unwrap(),
            "--profile",
            "work",
            "api",
            "documents.info",
            "id=doc-1",
        ])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("oauth"))
        .stderr(predicate::str::contains("api-key"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_env_url_pointing_away_from_the_profile_warns_on_stderr() {
    // Precedence keeps the env URL, but the profile's credential is now
    // going somewhere the profile did not name, so say so. The warning is a
    // diagnostic: stdout stays pure data.
    let elsewhere = instance("from-elsewhere", "key-for-work").await;
    let (_dir, config) = config_file("[profiles.work]\nurl = \"http://127.0.0.1:9\"\n");
    let config_arg = config.to_str().unwrap().to_string();
    let uri = elsewhere.uri();

    let output = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY_WORK", "key-for-work")
            .env("OUTLINE_URL", &uri)
            .args([
                "--config",
                &config_arg,
                "--profile",
                "work",
                "api",
                "documents.info",
                "id=doc-1",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.contains("warning:"), "no warning: {stderr}");
    assert!(stderr.contains("OUTLINE_URL"), "{stderr}");
    assert!(stdout.contains("from-elsewhere"), "{stdout}");
    // The URL itself is not printed: a base URL can carry credentials.
    assert!(!stderr.contains("127.0.0.1:9"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_matching_env_url_produces_no_warning() {
    let work = instance("from-work", "key-for-work").await;
    let (_dir, config) = config_file(&format!("[profiles.work]\nurl = \"{}\"\n", work.uri()));
    let config_arg = config.to_str().unwrap().to_string();
    let uri = work.uri();

    let output = tokio::task::spawn_blocking(move || {
        otl()
            .env("OUTLINE_API_KEY_WORK", "key-for-work")
            .env("OUTLINE_URL", &uri)
            .args([
                "--config",
                &config_arg,
                "--profile",
                "work",
                "api",
                "documents.info",
                "id=doc-1",
            ])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}
