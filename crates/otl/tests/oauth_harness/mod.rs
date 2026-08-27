//! Shared fixtures for the OAuth end-to-end suites.
//!
//! The tests split across `auth_login_e2e`, `auth_refresh_e2e` and
//! `auth_logout_e2e` all drive the real binary against a wiremock instance,
//! and they need the same scaffolding: a command with credential state
//! pinned to a scratch directory, a metadata document, and a way to play
//! the browser.
//!
//! Playing the browser is the interesting part. `otl auth login
//! --no-browser` prints the authorization URL on stderr and waits on its
//! loopback listener, so [`drive_login`] reads that URL, extracts `state`
//! and `redirect_uri` the way a browser would be handed them, and performs
//! the redirect itself - exercising real port binding, state checking, code
//! exchange and storage instead of mocking them out.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use reqwest::Url;
use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use otl::auth::credentials::{ClientRegistration, CredentialFile, CredentialStore, OAuthSession};

/// Marker that identifies the authorization URL among the stderr lines.
pub const AUTH_URL_MARKER: &str = "code_challenge=";

/// How many stderr lines to read before giving up on finding the URL.
pub const MAX_STDERR_LINES: usize = 40;

/// A `otl` command with every credential-related environment variable
/// pinned to the test's own scratch state.
pub fn otl(config_dir: &Path, base_url: &str) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("otl"));
    cmd.env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_URL", base_url)
        .env("OUTLINE_CONFIG_DIR", config_dir)
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// The metadata document a mock instance advertises.
pub fn metadata(base: &str) -> Value {
    json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "revocation_endpoint": format!("{base}/oauth/revoke"),
        "scopes_supported": ["read", "write"],
        "code_challenge_methods_supported": ["S256"],
        "grant_types_supported": ["authorization_code", "refresh_token"]
    })
}

/// Mount discovery, dynamic registration, token exchange and identity.
pub async fn mount_full_instance(server: &MockServer) {
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata(&base)))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/register"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "client_id": "dcr-client-1",
            "registration_access_token": "rat-1",
            "registration_client_uri": format!("{base}/oauth/clients/dcr-client-1"),
            "token_endpoint_auth_method": "none"
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "read write"
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/auth.info"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "user": { "name": "Alice Example", "email": "alice@example.com" },
                "team": { "name": "Acme Docs" }
            }
        })))
        .mount(server)
        .await;
}

/// What driving a login produced.
pub struct LoginRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The authorization URL the CLI told the browser to open.
    pub authorization_url: Option<Url>,
}

/// Read stderr until the authorization URL shows up.
pub fn take_authorization_url(child: &mut Child, sink: &mut String) -> Option<Url> {
    let stderr = child.stderr.as_mut()?;
    let mut reader = BufReader::new(stderr);
    for _ in 0..MAX_STDERR_LINES {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        sink.push_str(&line);
        if line.contains(AUTH_URL_MARKER) {
            let start = line.find("http")?;
            return Url::parse(line[start..].trim()).ok();
        }
    }
    None
}

/// Play the browser: complete the redirect the CLI is waiting for.
///
/// `mangle` may alter the query the "browser" sends back, which is how the
/// state-mismatch and denial paths are exercised.
pub fn perform_redirect(url: &Url, mangle: impl FnOnce(&mut Vec<(String, String)>)) {
    let redirect_uri = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .expect("the authorization URL must carry a redirect_uri");
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("the authorization URL must carry a state");

    let mut query = vec![
        ("code".to_string(), "auth-code-1".to_string()),
        ("state".to_string(), state),
    ];
    mangle(&mut query);
    let mut callback = Url::parse(&redirect_uri).expect("a valid redirect URI");
    callback.query_pairs_mut().extend_pairs(query);
    // The loopback listener answers and closes; a transport error here would
    // mean the CLI was not listening, which the assertions below catch.
    let _ = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("http client")
        .get(callback)
        .send();
}

/// Run a login to completion, acting as the browser.
pub fn drive_login(
    config_dir: &Path,
    base_url: &str,
    args: &[&str],
    mangle: impl FnOnce(&mut Vec<(String, String)>),
) -> LoginRun {
    let mut child = otl(config_dir, base_url)
        .args(["auth", "login", "--no-browser", "--timeout", "20"])
        .args(args)
        .spawn()
        .expect("otl auth login should start");

    let mut stderr = String::new();
    let authorization_url = take_authorization_url(&mut child, &mut stderr);
    if let Some(url) = &authorization_url {
        perform_redirect(url, mangle);
    }

    let mut stdout = String::new();
    if let Some(handle) = child.stdout.as_mut() {
        let _ = handle.read_to_string(&mut stdout);
    }
    if let Some(handle) = child.stderr.as_mut() {
        let _ = handle.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("otl auth login should exit");
    LoginRun {
        code: status.code(),
        stdout,
        stderr,
        authorization_url,
    }
}

/// The credential store for a scratch config directory.
pub fn store(dir: &Path) -> CredentialStore {
    CredentialStore::at(dir.to_path_buf())
}

/// The stored session for the default profile.
pub fn stored_session(dir: &Path) -> OAuthSession {
    store(dir)
        .load()
        .expect("credential file should be readable")
        .profile("default")
        .and_then(|profile| profile.oauth.clone())
        .expect("an OAuth session should be stored")
}

/// The stored client registration for the default profile.
pub fn stored_client(dir: &Path) -> ClientRegistration {
    store(dir)
        .load()
        .expect("credential file should be readable")
        .profile("default")
        .and_then(|profile| profile.client.clone())
        .expect("a client registration should be stored")
}

/// Whether no OAuth session is stored (the file may not exist at all).
pub fn stored_session_absent(dir: &Path) -> bool {
    store(dir)
        .load()
        .map(|file| {
            file.profile("default")
                .and_then(|profile| profile.oauth.as_ref())
                .is_none()
        })
        .unwrap_or(true)
}

/// Write a credential file holding a session with the given expiry.
pub fn seed_session(dir: &Path, base: &str, expires_at: Option<i64>) {
    let store = store(dir);
    let mut file = CredentialFile::default();
    file.profile_mut("default").origin = engine::base_url_origin(base);
    file.profile_mut("default").oauth = Some(OAuthSession {
        access_token: "access-old".to_string(),
        refresh_token: Some("refresh-old".to_string()),
        expires_at,
        scope: Some("read write".to_string()),
        client_id: "dcr-client-1".to_string(),
        token_endpoint: format!("{base}/oauth/token"),
        revocation_endpoint: Some(format!("{base}/oauth/revoke")),
        account: Some("Alice Example <alice@example.com>".to_string()),
        workspace: Some("Acme Docs".to_string()),
    });
    file.profile_mut("default").client = Some(ClientRegistration {
        client_id: "dcr-client-1".to_string(),
        client_secret: None,
        registration_access_token: Some("rat-1".to_string()),
        registration_client_uri: Some(format!("{base}/oauth/clients/dcr-client-1")),
        redirect_uri: "http://127.0.0.1:41234/callback".to_string(),
        dynamic: true,
        origin: Some(engine::base_url_origin(base).unwrap()),
    });
    store.save(&file).expect("seeding the credential file");
}

/// Mount a refresh grant that rotates both tokens, plus the API call the
/// new access token is expected to make.
pub async fn mount_refresh(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-new",
            "refresh_token": "refresh-new",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "read write"
        })))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .and(header("authorization", "Bearer access-new"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "data": { "id": "doc-1", "title": "Renewed" } })),
        )
        .mount(server)
        .await;
}

/// Mount a token endpoint that answers 308 pointing at `elsewhere`.
pub async fn mount_redirecting_token_endpoint(server: &MockServer, elsewhere: &str) {
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata(&base)))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            // 307/308 preserve the method AND replay the body, and
            // reqwest's cross-origin header stripping does not touch
            // bodies - so following this would post the refresh token to
            // `elsewhere`.
            ResponseTemplate::new(308).insert_header("location", format!("{elsewhere}/steal")),
        )
        .mount(server)
        .await;
}
