//! Integration: the curated commands resolve their
//! credential through the same path as `otl api`.
//!
//! Before the integration they did not. `otl docs ...` and
//! `otl collections ...` built their client from `Config::load`, which knows
//! only about `OUTLINE_API_KEY`: a stored key was ignored, an OAuth session
//! was ignored, and neither the transport rule nor the instance binding
//! applied. Credential precedence asserted for one command family and not
//! the other is worth very little, so it is asserted here for the other one.
//!
//! Separate binary from `auth_api_key.rs` rather than appended to it: that
//! file answers "which of three credentials is sent", this one answers
//! "which code path sends it", and the fixtures they need differ (a curated
//! command needs a document-shaped response).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use otl::auth::credentials::{CredentialFile, CredentialStore, OAuthSession};

/// Bearer value each credential kind carries, so the one that arrived is
/// identifiable from the request the mock recorded.
const OAUTH_TOKEN: &str = "from-oauth-session";
const STORED_KEY: &str = "from-credential-file";
const ENV_KEY: &str = "from-environment";

/// `otl` with credential state pinned to `config_dir` and no environment
/// key unless a test adds one.
fn otl(config_dir: &Path, base_url: &str) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("otl"));
    cmd.env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env_remove("OUTLINE_CONFIG")
        .env_remove("OUTLINE_NO_KEY_WARNING")
        .env_remove("PAGER")
        .env("OUTLINE_URL", base_url)
        .env("OUTLINE_CONFIG_DIR", config_dir);
    cmd
}

/// Seed the credential file for `default`, bound to `base`.
fn seed(dir: &Path, base: &str, oauth: bool, stored_key: bool) {
    if !oauth && !stored_key {
        return;
    }
    let store = CredentialStore::at(dir.to_path_buf());
    let mut file = CredentialFile::default();
    // Credentials are bound to the instance that issued them; without the
    // binding they are refused outright.
    file.profile_mut("default").origin = engine::base_url_origin(base);
    if stored_key {
        file.profile_mut("default").api_key = Some(STORED_KEY.to_string());
    }
    if oauth {
        file.profile_mut("default").oauth = Some(OAuthSession {
            access_token: OAUTH_TOKEN.to_string(),
            refresh_token: Some("refresh-1".to_string()),
            // Valid for another hour: no refresh must be attempted.
            expires_at: Some(otl::auth::oauth::now_unix() + 3600),
            scope: Some("read write".to_string()),
            client_id: "client-1".to_string(),
            token_endpoint: format!("{base}/oauth/token"),
            revocation_endpoint: None,
            account: None,
            workspace: None,
        });
    }
    store.save(&file).unwrap();
}

// ---------------------------------------------------------------------------
// The curated commands use the same credential path as `otl api`.
//
// Before the develop integration they did not: `otl docs ...` and
// `otl collections ...` built their client from `Config::load`, which knows
// only about `OUTLINE_API_KEY`. A stored key was ignored, an OAuth session
// was ignored, and neither the transport rule nor the instance binding
// applied. The precedence asserted above is worthless if it holds for one
// command family and not the other, so it is asserted for both.
// ---------------------------------------------------------------------------

/// Run one curated command and report which bearer value arrived.
async fn bearer_used_by_curated_command(
    oauth: bool,
    stored_key: bool,
    env_key: bool,
) -> (Option<i32>, String, String) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "doc-1",
                "title": "Deploy runbook",
                "text": "# Deploy runbook\n",
                "url": "/doc/deploy-runbook-abc123",
                "updatedAt": "2026-08-20T15:30:37.000Z",
            }
        })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();

    let (code, stderr) = {
        let (path, base) = (dir.path().to_path_buf(), base.clone());
        tokio::task::spawn_blocking(move || {
            seed(&path, &base, oauth, stored_key);
            let mut cmd = otl(&path, &base);
            if env_key {
                cmd.env("OUTLINE_API_KEY", ENV_KEY);
            }
            let output = cmd.args(["docs", "view", "doc-1"]).output().unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        })
        .await
        .unwrap()
    };

    let sent = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/api/documents.info")
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .next()
        .unwrap_or_default();
    (code, sent, stderr)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_curated_command_uses_the_oauth_session_too() {
    let (code, sent, stderr) = bearer_used_by_curated_command(true, true, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(sent, format!("Bearer {OAUTH_TOKEN}"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_curated_command_prefers_the_stored_key_over_the_environment() {
    let (code, sent, stderr) = bearer_used_by_curated_command(false, true, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(sent, format!("Bearer {STORED_KEY}"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_curated_command_warns_about_a_plaintext_environment_key() {
    // Same notice `otl api` prints, from the same place: a user who is told
    // about it for one command family and not the other would reasonably
    // conclude the other one is safe.
    let (code, sent, stderr) = bearer_used_by_curated_command(false, false, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(sent, format!("Bearer {ENV_KEY}"), "stderr: {stderr}");
    assert!(stderr.contains("OUTLINE_API_KEY"), "stderr: {stderr}");
    assert!(stderr.contains("otl auth set-key"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_curated_command_refuses_a_credential_from_another_instance() {
    // The binding check has to be on this path as well: without it, pointing
    // OUTLINE_URL at another host and running `otl docs view` would hand it
    // this profile's session token.
    //
    // TWO live servers, not one plus a name that resolves nowhere. With a
    // dead hostname the command fails either way and the assertion would
    // hold with the check deleted - the failure would just be a DNS error
    // instead of a refusal. The second server is what makes "nothing was
    // sent" mean something: it is reachable, it would answer, and it must
    // still receive no request at all.
    let issuer = MockServer::start().await;
    let other = MockServer::start().await;
    for server in [&issuer, &other] {
        Mock::given(method("POST"))
            .and(path("/api/documents.info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "id": "doc-1", "title": "t", "text": "x", "url": "/doc/x" }
            })))
            .mount(server)
            .await;
    }
    let issuer_uri = issuer.uri();
    let other_uri = other.uri();
    let dir = tempfile::tempdir().unwrap();

    let (code, stderr) = {
        let (path, issuer_uri, other_uri) = (
            dir.path().to_path_buf(),
            issuer_uri.clone(),
            other_uri.clone(),
        );
        tokio::task::spawn_blocking(move || {
            // A session issued by `issuer`...
            seed(&path, &issuer_uri, true, false);
            // ...and a command pointed at `other`.
            let output = otl(&path, &other_uri)
                .args(["docs", "view", "doc-1"])
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        })
        .await
        .unwrap()
    };

    assert_ne!(code, Some(0), "the credential was accepted: {stderr}");
    let sent_elsewhere = other.received_requests().await.unwrap();
    assert!(
        sent_elsewhere.is_empty(),
        "{} request(s) reached the other instance despite the binding check",
        sent_elsewhere.len()
    );
    assert!(!stderr.contains(OAUTH_TOKEN), "stderr: {stderr}");
}

// ---------------------------------------------------------------------------
// `otl auth info` releases its credential through the same gate as
// everything else.
//
// `auth info` resolves the instance through config and the CREDENTIAL
// through the same gate, whose environment branch reads `OUTLINE_API_KEY`
// directly. The config gate deliberately refuses that fallback when a profile
// is in effect - a profile scopes its key to `OUTLINE_API_KEY_<PROFILE>`,
// because falling back to the global one sends one workspace's key to another
// workspace's server. So on one configuration `otl api` refused and
// `otl auth info` sent the key.
//
// The test is the reviewer's reproduction: same config, both commands, and
// the assertion is on what the INSTANCE RECEIVED rather than on an exit code.
// A refusal that still made the request would satisfy an exit-code assertion.
// ---------------------------------------------------------------------------

/// A global key that only ever belongs to some other instance.
const GLOBAL_KEY: &str = "global-key-for-another-instance";

/// Run `args` with a profile selected, a global key exported, no
/// profile-scoped key, and nothing stored for the profile.
///
/// Returns the exit code and every Authorization header the instance saw.
async fn with_global_key_only(args: &[&str]) -> (Option<i32>, Vec<String>, String) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "id": "u1", "name": "Someone", "email": "someone@example.com" }
        })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, format!("[profiles.work]\nurl = \"{base}\"\n")).unwrap();

    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let (code, stderr) = {
        let (path, config) = (dir.path().to_path_buf(), config.clone());
        tokio::task::spawn_blocking(move || {
            let output = Command::new(assert_cmd::cargo::cargo_bin("otl"))
                .env_remove("OUTLINE_URL")
                .env_remove("OUTLINE_API_KEY_WORK")
                .env("OUTLINE_API_KEY", GLOBAL_KEY)
                .env("OUTLINE_CONFIG_DIR", &path)
                .env("OUTLINE_NO_KEY_WARNING", "1")
                .args(["--config", config.to_str().unwrap(), "--profile", "work"])
                .args(&owned)
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        })
        .await
        .unwrap()
    };

    let sent = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect();
    (code, sent, stderr)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_without_its_own_key_gets_no_credential_from_otl_api() {
    // The control: this is the behaviour `auth info` has to match.
    let (code, sent, stderr) = with_global_key_only(&["api", "documents.info", "id=x"]).await;
    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(sent.is_empty(), "the global key was sent: {sent:?}");
    assert!(
        stderr.contains("OUTLINE_API_KEY_WORK"),
        "the diagnostic does not name the variable to set: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_info_does_not_send_a_global_key_to_a_profiles_instance() {
    let (code, sent, stderr) = with_global_key_only(&["auth", "info"]).await;
    assert!(
        !sent.iter().any(|header| header.contains(GLOBAL_KEY)),
        "auth info sent the global key to the profile's instance: {sent:?}\n\
         stderr: {stderr}"
    );
    assert!(
        sent.is_empty(),
        "auth info made a request with no credential the gate approved: {sent:?}"
    );
    // And it still REPORTS, rather than aborting: that is what `auth info` is
    // for. The exit code is 0 and the report says there is no method.
    assert_eq!(code, Some(0), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_info_says_why_the_global_key_is_not_used_and_what_to_do() {
    // Refusing quietly would be its own bug: the user exported a key, the
    // command reports "none", and the reason is invisible. The report has to
    // carry the gate's own reason and the profile-scoped variable name -
    // otherwise the fix for [N1] just moves the confusion.
    let (code, sent, stdout) = with_global_key_only_stdout(&["auth", "info"]).await;
    assert_eq!(code, Some(0), "stdout: {stdout}");
    assert!(sent.is_empty(), "{sent:?}");
    assert!(
        stdout.contains("OUTLINE_API_KEY_WORK"),
        "the report does not name the variable to set: {stdout}"
    );
    assert!(
        stdout.contains("another workspace's server"),
        "the report does not say why the global key is refused: {stdout}"
    );
    assert!(
        !stdout.contains(GLOBAL_KEY),
        "the report echoed the credential: {stdout}"
    );
}

/// [`with_global_key_only`], reporting stdout instead of stderr.
async fn with_global_key_only_stdout(args: &[&str]) -> (Option<i32>, Vec<String>, String) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, format!("[profiles.work]\nurl = \"{base}\"\n")).unwrap();

    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let (code, stdout) = {
        let (path, config) = (dir.path().to_path_buf(), config.clone());
        tokio::task::spawn_blocking(move || {
            let output = Command::new(assert_cmd::cargo::cargo_bin("otl"))
                .env_remove("OUTLINE_URL")
                .env_remove("OUTLINE_API_KEY_WORK")
                .env("OUTLINE_API_KEY", GLOBAL_KEY)
                .env("OUTLINE_CONFIG_DIR", &path)
                .env("OUTLINE_NO_KEY_WARNING", "1")
                .args(["--config", config.to_str().unwrap(), "--profile", "work"])
                .args(&owned)
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).to_string(),
            )
        })
        .await
        .unwrap()
    };

    let sent = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect();
    (code, sent, stdout)
}
