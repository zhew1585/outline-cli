//! API key management and credential precedence.
//!
//! Three credentials can exist at once - an OAuth session, a key in the
//! credential file, and a key in the environment - and exactly one of them
//! must be sent. The order is asserted here against a wiremock instance
//! that records which bearer value actually arrived.

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
        .env_remove("OUTLINE_NO_KEY_WARNING")
        .env("OUTLINE_URL", base_url)
        .env("OUTLINE_CONFIG_DIR", config_dir);
    cmd
}

/// Seed the credential file with the requested combination.
fn seed(dir: &Path, base: &str, oauth: bool, stored_key: bool) {
    if !oauth && !stored_key {
        return;
    }
    let store = CredentialStore::at(dir.to_path_buf());
    let mut file = CredentialFile::default();
    // Credentials are bound to the instance that issued them; without the
    // binding they are refused outright (see the cross-instance test).
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
            account: Some("Alice <alice@example.com>".to_string()),
            workspace: Some("Acme".to_string()),
        });
    }
    store.save(&file).unwrap();
}

/// Run one request and report which bearer value the server received.
async fn bearer_used(
    oauth: bool,
    stored_key: bool,
    env_key: bool,
) -> (Option<i32>, String, String) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "id": "d" } })))
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
            let output = cmd
                .args(["api", "documents.info", "id=d"])
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
async fn an_oauth_session_wins_over_both_kinds_of_api_key() {
    let (code, sent, stderr) = bearer_used(true, true, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(sent, format!("Bearer {OAUTH_TOKEN}"), "stderr: {stderr}");
    // The environment key is not in use, so its warning must stay quiet.
    assert!(
        !stderr.contains("OUTLINE_API_KEY"),
        "warned about an unused environment key: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_key_wins_over_the_environment() {
    let (code, sent, stderr) = bearer_used(false, true, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(sent, format!("Bearer {STORED_KEY}"), "stderr: {stderr}");
    assert!(
        !stderr.contains("OUTLINE_API_KEY"),
        "warned about an unused environment key: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_environment_key_is_used_when_nothing_is_stored() {
    let (code, sent, stderr) = bearer_used(false, false, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert_eq!(sent, format!("Bearer {ENV_KEY}"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn using_the_environment_key_warns_once_about_plaintext() {
    let (code, _, stderr) = bearer_used(false, false, true).await;
    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stderr.contains("OUTLINE_API_KEY"), "no warning: {stderr}");
    assert!(stderr.contains("otl auth set-key"), "no remedy: {stderr}");
    assert!(
        stderr.contains("shell history"),
        "the risk is not explained: {stderr}"
    );
    // Once, not once per request.
    assert_eq!(
        stderr.matches("otl auth set-key").count(),
        1,
        "the notice repeated: {stderr}"
    );
    assert!(
        !stderr.contains(ENV_KEY),
        "the key itself was printed: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_plaintext_warning_can_be_silenced() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let stderr = tokio::task::spawn_blocking(move || {
        let output = otl(&path, &base)
            .env("OUTLINE_API_KEY", ENV_KEY)
            .env("OUTLINE_NO_KEY_WARNING", "1")
            .args(["api", "documents.info", "id=d"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stderr).to_string()
    })
    .await
    .unwrap();

    assert!(stderr.is_empty(), "expected silence, got: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_never_sent_to_an_instance_that_did_not_issue_them() {
    // The attack: sign in to instance A, then point OUTLINE_URL at an
    // instance the attacker controls. Nothing must be sent to B - not the
    // stored access token, and not a freshly minted one either.
    let attacker = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&attacker)
        .await;

    let victim = "https://docs.example.com";
    let attacker_url = attacker.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let (code, stderr) = {
        let attacker_url = attacker_url.clone();
        tokio::task::spawn_blocking(move || {
            // Credentials issued by the victim instance...
            seed(&path, victim, true, true);
            // ...and a command pointed at the attacker's.
            let output = otl(&path, &attacker_url)
                .args(["api", "documents.info", "id=d"])
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

    assert_eq!(code, Some(2), "stderr: {stderr}");
    assert!(stderr.contains("docs.example.com"), "stderr: {stderr}");
    assert!(stderr.contains("refused"), "stderr: {stderr}");
    // Nothing at all reached the attacker.
    assert!(
        attacker
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a request reached an instance that never issued these credentials"
    );
    for secret in [OAUTH_TOKEN, STORED_KEY] {
        assert!(!stderr.contains(secret), "{secret} leaked: {stderr}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_environment_key_is_not_bound_to_a_stored_instance() {
    // An environment key is supplied per invocation next to OUTLINE_URL, so
    // there is no stored binding for it to contradict and it must keep
    // working against any instance.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let code = tokio::task::spawn_blocking(move || {
        otl(&path, &base)
            .env("OUTLINE_API_KEY", ENV_KEY)
            .env("OUTLINE_NO_KEY_WARNING", "1")
            .args(["api", "documents.info", "id=d"])
            .output()
            .unwrap()
            .status
            .code()
    })
    .await
    .unwrap();
    assert_eq!(code, Some(0));
}

#[test]
fn nothing_configured_names_every_way_to_authenticate() {
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://127.0.0.1:9")
        .args(["api", "documents.info", "id=d"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("otl auth login"), "stderr: {stderr}");
    assert!(stderr.contains("otl auth set-key"), "stderr: {stderr}");
    assert!(stderr.contains("OUTLINE_API_KEY"), "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_info_names_the_method_in_use_and_what_it_shadows() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "user": { "name": "Alice", "email": "alice@example.com" },
                "team": { "name": "Acme" }
            }
        })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let (code, stdout) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed(&path, &base, true, true);
            let output = otl(&path, &base)
                .env("OUTLINE_API_KEY", ENV_KEY)
                .args(["auth", "info"])
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(0), "stdout: {stdout}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["profile"], "default");
    assert!(
        report["method"].as_str().unwrap().starts_with("oauth"),
        "method: {}",
        report["method"]
    );
    // What the release gate would hand over, in precedence order. The
    // credential FILE's two entries, session first.
    //
    // The exported `OUTLINE_API_KEY` is deliberately NOT in this list:
    // `available` lists only credentials the gate would actually release
    // for these settings, and a profile-scoped setup would not release this
    // one at all - so the
    // exported key is reported as an observation on its own field below.
    let available: Vec<&str> = report["available"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert_eq!(available.len(), 2, "{available:?}");
    assert!(available[0].starts_with("oauth"), "{available:?}");
    assert!(available[1].contains("credential file"), "{available:?}");
    // ...and the shadowed environment key is still surfaced, so a user who
    // exported one is told why it is not in use.
    assert_eq!(report["plaintext_key_in_environment"], true, "{stdout}");
    assert_eq!(report["scope"], "read write");
    assert_eq!(report["account"], "Alice <alice@example.com>");
    assert!(report["credential_file"]
        .as_str()
        .unwrap()
        .ends_with("credentials.toml"));
    // The instance is reported as an origin, never as a full URL.
    assert_eq!(
        report["instance"].as_str().unwrap(),
        engine::base_url_origin(&base).unwrap()
    );
}

#[test]
fn auth_info_works_with_no_credentials_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://127.0.0.1:9")
        .args(["auth", "info", "--offline"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "stdout: {stdout}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(report["method"].is_null(), "{stdout}");
    assert_eq!(report["credential_file_exists"], false);
}

// --- finding [17]: cross-instance WRITES must not create a mixed profile --

/// Seed an OAuth session issued by `issuer`, bound to it.
fn seed_session_for(dir: &Path, issuer: &str) {
    let store = CredentialStore::at(dir.to_path_buf());
    let mut file = CredentialFile::default();
    let entry = file.profile_mut("default");
    entry.origin = engine::base_url_origin(issuer);
    entry.oauth = Some(OAuthSession {
        access_token: OAUTH_TOKEN.to_string(),
        refresh_token: Some("refresh-1".to_string()),
        expires_at: Some(otl::auth::oauth::now_unix() + 3600),
        scope: Some("read write".to_string()),
        client_id: "client-1".to_string(),
        token_endpoint: format!("{issuer}/oauth/token"),
        revocation_endpoint: None,
        account: None,
        workspace: None,
    });
    store.save(&file).unwrap();
}

#[test]
fn set_key_for_another_instance_is_refused_rather_than_merged() {
    // `set-key` must not rewrite `profile.origin` to the new instance
    // while leaving the old one's OAuth session in place: the profile
    // would LOOK bound to B, but OAuth outranks an API key, so the next
    // request would send A's access token to B.
    use std::io::Write;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    seed_session_for(dir.path(), "https://a.example.com");

    let mut child = otl(dir.path(), "https://b.example.com")
        .args(["auth", "set-key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"ol_api_FOR_B\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("a.example.com"), "stderr: {stderr}");
    assert!(stderr.contains("b.example.com"), "stderr: {stderr}");
    assert!(stderr.contains("OUTLINE_PROFILE"), "stderr: {stderr}");

    // Nothing was written, and the profile still belongs to A alone.
    let stored = CredentialStore::at(dir.path().to_path_buf())
        .load()
        .unwrap();
    let entry = stored.profile("default").unwrap();
    assert!(entry.api_key.is_none(), "a key for B was stored anyway");
    assert_eq!(entry.origin.as_deref(), Some("https://a.example.com"));
}

#[test]
fn a_hand_mixed_profile_is_refused_by_the_sessions_own_origin() {
    // Defence in depth for the same bug: even if `profile.origin` says B -
    // by a hand edit, or a future write path that forgets the guard - the
    // session names its own issuer through the token endpoint discovery
    // validated at login, so it cannot be passed off as B's.
    let dir = tempfile::tempdir().unwrap();
    seed_session_for(dir.path(), "https://a.example.com");

    let store = CredentialStore::at(dir.path().to_path_buf());
    let mut file = store.load().unwrap();
    file.profile_mut("default").origin = Some("https://b.example.com".to_string());
    store.save(&file).unwrap();

    let output = otl(dir.path(), "https://b.example.com")
        .args(["api", "documents.info", "id=d"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("a.example.com"), "stderr: {stderr}");
    assert!(!stderr.contains(OAUTH_TOKEN), "token leaked: {stderr}");
}

#[test]
fn login_against_another_instance_is_refused_before_any_network_work() {
    let dir = tempfile::tempdir().unwrap();
    seed_session_for(dir.path(), "https://a.example.com");

    let output = otl(dir.path(), "https://b.example.invalid")
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("a.example.com"), "stderr: {stderr}");
    // Refused locally: no DNS lookup for the unreachable host was made.
    assert!(!stderr.contains("network error"), "stderr: {stderr}");
}

#[test]
fn a_purge_hint_is_offered_when_a_dynamic_registration_would_linger() {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::at(dir.path().to_path_buf());
    let mut file = CredentialFile::default();
    let entry = file.profile_mut("default");
    entry.origin = Some("https://a.example.com".to_string());
    entry.api_key = Some("key-a".to_string());
    entry.client = Some(otl::auth::credentials::ClientRegistration {
        client_id: "c".to_string(),
        client_secret: None,
        registration_access_token: Some("rat".to_string()),
        registration_client_uri: Some("https://a.example.com/oauth/clients/c".to_string()),
        redirect_uri: "http://127.0.0.1:41234/callback".to_string(),
        dynamic: true,
        origin: Some("https://a.example.com".to_string()),
    });
    store.save(&file).unwrap();

    let output = otl(dir.path(), "https://b.example.com")
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("logout --purge"),
        "a dynamic registration needs --purge to clear: {stderr}"
    );
}

// --- finding [21]: a leftover registration must not block an env key -----

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_client_registration_alone_does_not_block_an_environment_key() {
    // A client registration cannot authenticate anything, so a leftover one
    // from another instance must not stop an environment key supplied for
    // this one.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&server)
        .await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let (code, sent) = {
        let base = base.clone();
        tokio::task::spawn_blocking(move || {
            let store = CredentialStore::at(path.clone());
            let mut file = CredentialFile::default();
            let entry = file.profile_mut("default");
            entry.origin = Some("https://a.example.com".to_string());
            entry.client = Some(otl::auth::credentials::ClientRegistration {
                client_id: "c".to_string(),
                client_secret: None,
                registration_access_token: None,
                registration_client_uri: None,
                redirect_uri: "http://127.0.0.1:41234/callback".to_string(),
                dynamic: true,
                origin: Some("https://a.example.com".to_string()),
            });
            store.save(&file).unwrap();

            let output = otl(&path, &base)
                .env("OUTLINE_API_KEY", ENV_KEY)
                .env("OUTLINE_NO_KEY_WARNING", "1")
                .args(["api", "documents.info", "id=d"])
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
    assert_eq!(code, Some(0), "stderr: {sent}");

    let bearer = server
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
        .next()
        .unwrap_or_default();
    assert_eq!(bearer, format!("Bearer {ENV_KEY}"));
}

// --- finding [18]: the ordinary API channel must require TLS -------------

#[test]
fn a_remote_plaintext_instance_is_refused_for_ordinary_api_calls() {
    // Transport is checked for ordinary API calls too, not just `auth
    // login`: against http://remote-host the bearer token would go on the
    // wire in the clear.
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://docs.example.invalid")
        .env("OUTLINE_API_KEY", ENV_KEY)
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .args(["api", "documents.info", "id=d"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("https://"), "stderr: {stderr}");
    assert!(!stderr.contains(ENV_KEY), "key leaked: {stderr}");
    // Refused locally, before any resolution or connection.
    assert!(!stderr.contains("network error"), "stderr: {stderr}");
}

#[test]
fn a_remote_plaintext_instance_is_refused_for_a_stored_key_too() {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::at(dir.path().to_path_buf());
    let mut file = CredentialFile::default();
    let entry = file.profile_mut("default");
    entry.origin = Some("http://docs.example.invalid".to_string());
    entry.api_key = Some(STORED_KEY.to_string());
    store.save(&file).unwrap();

    let output = otl(dir.path(), "http://docs.example.invalid")
        .args(["api", "documents.info", "id=d"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("https://"), "stderr: {stderr}");
    assert!(!stderr.contains(STORED_KEY), "key leaked: {stderr}");
}

#[test]
fn a_remote_plaintext_instance_is_refused_by_auth_info_too() {
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://docs.example.invalid")
        .env("OUTLINE_API_KEY", ENV_KEY)
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .args(["auth", "info"])
        .output()
        .unwrap();
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!printed.contains(ENV_KEY), "key leaked: {printed}");
    assert!(
        printed.contains("https://") || printed.contains("not usable"),
        "the transport problem must be reported: {printed}"
    );
}

#[test]
fn a_loopback_instance_still_works_over_plaintext() {
    // The documented exception must keep working, or every local
    // development setup breaks.
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://127.0.0.1:9")
        .env("OUTLINE_API_KEY", ENV_KEY)
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .args(["api", "documents.info", "id=d"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Nothing listens on port 9, so this is a transport failure (7) - which
    // proves the request was attempted rather than refused as insecure (2).
    assert_eq!(output.status.code(), Some(7), "stderr: {stderr}");
}
