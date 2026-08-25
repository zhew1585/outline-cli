//! End-to-end OAuth: `otl auth login` against a mock authorization server,
//! and automatic token renewal on the request channel (Stories 2.1-2.4).
//!
//! The browser is played by the test: `otl auth login --no-browser` prints
//! the authorization URL on stderr and waits on its loopback listener, so
//! the test reads that URL, extracts `state` and `redirect_uri` exactly as
//! a browser would be handed them, and performs the redirect itself. That
//! keeps the real code path - fixed/ephemeral port binding, state checking,
//! code exchange, atomic storage - under test without a real browser.
//!
//! Everything is wiremock. No test here touches the network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use reqwest::Url;
use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use otl::auth::credentials::{ClientRegistration, CredentialFile, CredentialStore, OAuthSession};

/// Marker that identifies the authorization URL among the stderr lines.
const AUTH_URL_MARKER: &str = "code_challenge=";

/// How many stderr lines to read before giving up on finding the URL.
const MAX_STDERR_LINES: usize = 40;

/// A `otl` command with every credential-related environment variable
/// pinned to the test's own scratch state.
fn otl(config_dir: &Path, base_url: &str) -> Command {
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
fn metadata(base: &str) -> Value {
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
async fn mount_full_instance(server: &MockServer) {
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
struct LoginRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// The authorization URL the CLI told the browser to open.
    authorization_url: Option<Url>,
}

/// Read stderr until the authorization URL shows up.
fn take_authorization_url(child: &mut Child, sink: &mut String) -> Option<Url> {
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
fn perform_redirect(url: &Url, mangle: impl FnOnce(&mut Vec<(String, String)>)) {
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
fn drive_login(
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
fn store(dir: &Path) -> CredentialStore {
    CredentialStore::at(dir.to_path_buf())
}

/// The stored session for the default profile.
fn stored_session(dir: &Path) -> OAuthSession {
    store(dir)
        .load()
        .expect("credential file should be readable")
        .profile("default")
        .and_then(|profile| profile.oauth.clone())
        .expect("an OAuth session should be stored")
}

/// The stored client registration for the default profile.
fn stored_client(dir: &Path) -> ClientRegistration {
    store(dir)
        .load()
        .expect("credential file should be readable")
        .profile("default")
        .and_then(|profile| profile.client.clone())
        .expect("a client registration should be stored")
}

#[tokio::test(flavor = "multi_thread")]
async fn login_registers_dynamically_and_stores_a_usable_session() {
    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let run = tokio::task::spawn_blocking(move || drive_login(&path, &base, &[], |_| {}))
        .await
        .unwrap();

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let url = run.authorization_url.expect("an authorization URL");
    let pairs: std::collections::HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    // Story 2.1: PKCE S256, scope read write, a random state, loopback URI.
    assert_eq!(pairs["code_challenge_method"], "S256");
    assert_eq!(pairs["scope"], "read write");
    assert_eq!(pairs["response_type"], "code");
    assert!(
        pairs["state"].len() >= 16,
        "state too short: {:?}",
        pairs["state"]
    );
    assert!(
        pairs["redirect_uri"].starts_with("http://127.0.0.1:"),
        "redirect must be loopback: {:?}",
        pairs["redirect_uri"]
    );
    // Story 2.2: the registration and its management token are persisted.
    let registration = stored_client(dir.path());
    assert_eq!(registration.client_id, "dcr-client-1");
    assert_eq!(
        registration.registration_access_token.as_deref(),
        Some("rat-1"),
        "without this token the client can never be deleted"
    );
    assert!(registration.dynamic);
    assert_eq!(registration.redirect_uri, pairs["redirect_uri"]);

    let session = stored_session(dir.path());
    assert_eq!(session.access_token, "access-1");
    assert_eq!(session.refresh_token.as_deref(), Some("refresh-1"));
    assert!(session.expires_at.is_some(), "expiry must be recorded");
    // Identity is reported and cached for `auth info`.
    assert_eq!(
        session.account.as_deref(),
        Some("Alice Example <alice@example.com>")
    );
    assert_eq!(session.workspace.as_deref(), Some("Acme Docs"));
    assert!(
        run.stdout.contains("Alice Example"),
        "stdout: {}",
        run.stdout
    );
    assert!(run.stdout.contains("Acme Docs"), "stdout: {}", run.stdout);
    // No credential ever reaches the terminal.
    let printed = format!("{}{}", run.stdout, run.stderr);
    for secret in ["access-1", "refresh-1", "rat-1", "auth-code-1"] {
        assert!(
            !printed.contains(secret),
            "{secret} leaked to the terminal: {printed}"
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn login_creates_the_credential_file_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    // A nested directory that does not exist yet: the parents must be
    // created as part of the first write.
    let config = dir.path().join("nested").join("outline-cli");
    let path = config.clone();

    let run = tokio::task::spawn_blocking(move || drive_login(&path, &base, &[], |_| {}))
        .await
        .unwrap();
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);

    let file = config.join("credentials.toml");
    let file_mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "credential file is {file_mode:04o}");
    let dir_mode = std::fs::metadata(&config).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "credential directory is {dir_mode:04o}");
}

#[tokio::test(flavor = "multi_thread")]
async fn login_reuses_a_cached_registration_instead_of_registering_again() {
    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    // The registration endpoint must be hit exactly once across two logins.
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();

    let first = {
        let (path, base) = (dir.path().to_path_buf(), base.clone());
        tokio::task::spawn_blocking(move || drive_login(&path, &base, &[], |_| {}))
            .await
            .unwrap()
    };
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);

    let second = {
        let (path, base) = (dir.path().to_path_buf(), base.clone());
        tokio::task::spawn_blocking(move || drive_login(&path, &base, &[], |_| {}))
            .await
            .unwrap()
    };
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert!(
        second.stdout.contains("\"client_source\": \"cached\""),
        "the second login should report a reused registration: {}",
        second.stdout
    );

    let registrations = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request: &Request| request.url.path() == "/oauth/register")
        .count();
    assert_eq!(
        registrations, 1,
        "a cached client registration must not be created twice"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn login_without_dynamic_registration_points_at_the_admin_path() {
    let server = MockServer::start().await;
    let base = server.uri();
    let mut document = metadata(&base);
    document
        .as_object_mut()
        .map(|map| map.remove("registration_endpoint"));
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(document))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let run = tokio::task::spawn_blocking(move || drive_login(&path, &base, &[], |_| {}))
        .await
        .unwrap();

    assert_eq!(run.code, Some(2), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("Settings -> Applications"),
        "no admin guidance: {}",
        run.stderr
    );
    assert!(run.stderr.contains("--client-id"), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("127.0.0.1:8586/callback"),
        "the documented redirect URI must be shown: {}",
        run.stderr
    );
    assert!(
        !store(dir.path()).path().exists(),
        "a failed login must not create a credential file"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provided_client_id_uses_a_fixed_callback_port() {
    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let run = tokio::task::spawn_blocking(move || {
        drive_login(&path, &base, &["--client-id", "admin-client-1"], |_| {})
    })
    .await
    .unwrap();

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let url = run.authorization_url.expect("an authorization URL");
    let redirect = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    let port: u16 = Url::parse(&redirect).unwrap().port().unwrap();
    assert!(
        [8586_u16, 18586, 28586, 38586].contains(&port),
        "a pre-registered client must use a documented port, got {port}"
    );
    let client_id = url
        .query_pairs()
        .find(|(key, _)| key == "client_id")
        .map(|(_, value)| value.into_owned())
        .unwrap();
    assert_eq!(client_id, "admin-client-1");
    // An administrator's client is never a purge target.
    assert!(!stored_client(dir.path()).dynamic);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_with_the_wrong_state_is_refused_without_exchanging_the_code() {
    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let run = tokio::task::spawn_blocking(move || {
        drive_login(&path, &base, &[], |query| {
            for entry in query.iter_mut() {
                if entry.0 == "state" {
                    entry.1 = "forged-state".to_string();
                }
            }
        })
    })
    .await
    .unwrap();

    assert_eq!(run.code, Some(4), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("unexpected state"),
        "stderr: {}",
        run.stderr
    );
    let exchanges = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request: &Request| request.url.path() == "/oauth/token")
        .count();
    assert_eq!(
        exchanges, 0,
        "a code from a mismatched redirect must never be exchanged"
    );
    assert!(stored_session_absent(dir.path()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_denied_authorization_is_reported_and_stores_nothing() {
    let server = MockServer::start().await;
    mount_full_instance(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let run = tokio::task::spawn_blocking(move || {
        drive_login(&path, &base, &[], |query| {
            query.retain(|(key, _)| key != "code");
            query.push(("error".to_string(), "access_denied".to_string()));
            query.push((
                "error_description".to_string(),
                "The user said no".to_string(),
            ));
        })
    })
    .await
    .unwrap();

    assert_eq!(run.code, Some(4), "stderr: {}", run.stderr);
    assert!(
        run.stderr.contains("access_denied"),
        "stderr: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("The user said no"),
        "stderr: {}",
        run.stderr
    );
    assert!(stored_session_absent(dir.path()));
}

/// Whether no OAuth session is stored (the file may not exist at all).
fn stored_session_absent(dir: &Path) -> bool {
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
fn seed_session(dir: &Path, base: &str, expires_at: Option<i64>) {
    let store = store(dir);
    let mut file = CredentialFile::default();
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
async fn mount_refresh(server: &MockServer) {
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

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_access_token_is_renewed_before_the_request() {
    let server = MockServer::start().await;
    mount_refresh(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();

    let path = dir.path().to_path_buf();
    let (code, stdout, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            // Recorded as already expired: renewal happens before the
            // request goes out, not after a 401.
            seed_session(&path, &base, Some(0));
            let output = otl(&path, &base)
                .args(["api", "documents.info", "id=doc-1"])
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stdout.contains("Renewed"), "stdout: {stdout}");
    // Story 2.3: the rotated tokens are persisted, and the spent refresh
    // token is gone.
    let session = stored_session(dir.path());
    assert_eq!(session.access_token, "access-new");
    assert_eq!(session.refresh_token.as_deref(), Some("refresh-new"));
    // Fields the refresh response did not carry survive.
    assert_eq!(
        session.account.as_deref(),
        Some("Alice Example <alice@example.com>")
    );
    for secret in ["access-new", "refresh-new", "refresh-old"] {
        assert!(
            !stderr.contains(secret) && !stdout.contains(secret),
            "{secret} leaked to the terminal"
        );
    }
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_processes_refresh_exactly_once() {
    // The reason the lock exists: Outline rotates the refresh token on every
    // use, so a second concurrent refresh would spend a token the first one
    // already retired. `mount_refresh` asserts `.expect(1)` on the grant,
    // and only one of the two processes may reach it - the other must wait,
    // re-read the file, and use the winner's access token.
    let server = MockServer::start().await;
    mount_refresh(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();

    let path = dir.path().to_path_buf();
    let outcomes = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(0));
            let children: Vec<_> = (0..2)
                .map(|_| {
                    otl(&path, &base)
                        .args(["api", "documents.info", "id=doc-1"])
                        .spawn()
                        .unwrap()
                })
                .collect();
            children
                .into_iter()
                .map(|child| child.wait_with_output().unwrap())
                .collect::<Vec<_>>()
        }
    })
    .await
    .unwrap();

    for output in &outcomes {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Renewed"),
            "a waiting process must end up with a usable token: {stderr}"
        );
    }
    assert_eq!(stored_session(dir.path()).access_token, "access-new");
    // `.expect(1)` on the refresh grant is verified here.
    drop(server);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn losing_rotated_tokens_is_reported_instead_of_being_swallowed() {
    use std::os::unix::fs::PermissionsExt;

    // The server has already retired the old refresh token by the time the
    // write fails, so silence here would leave the user with a credential
    // file that can never be refreshed again and no idea why.
    let server = MockServer::start().await;
    mount_refresh(&server).await;
    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();

    let path = dir.path().to_path_buf();
    let (code, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(0));
            // Create the lock file while the directory is still writable, so
            // the failure lands on the credential WRITE and not earlier.
            drop(otl::auth::lock::RefreshLock::acquire(&path).unwrap());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
            let output = otl(&path, &base)
                .args(["api", "documents.info", "id=doc-1"])
                .output()
                .unwrap();
            // Restore write permission so the temp dir can be cleaned up.
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(4), "stderr: {stderr}");
    assert!(stderr.contains("rotated"), "stderr: {stderr}");
    assert!(stderr.contains("otl auth login"), "stderr: {stderr}");
    for secret in ["access-new", "refresh-new", "refresh-old"] {
        assert!(!stderr.contains(secret), "{secret} leaked: {stderr}");
    }
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_access_token_is_renewed_and_the_request_replayed() {
    let server = MockServer::start().await;
    mount_refresh(&server).await;
    // The stale token is refused; the renewed one works (mounted above).
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .and(header("authorization", "Bearer access-old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "authentication_required",
            "message": "Authentication error"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stdout, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            // Recorded as valid for another hour, so only the server's 401
            // can trigger the renewal.
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            let output = otl(&path, &base)
                .args(["api", "documents.info", "id=doc-1"])
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(stdout.contains("Renewed"), "stdout: {stdout}");
    assert_eq!(stored_session(dir.path()).access_token, "access-new");
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_refresh_token_asks_for_a_new_login_with_exit_code_4() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "refresh token refresh-old is not valid"
        })))
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(0));
            let output = otl(&path, &base)
                .args(["api", "documents.info", "id=doc-1"])
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(4), "stderr: {stderr}");
    assert!(stderr.contains("otl auth login"), "stderr: {stderr}");
    assert!(stderr.contains("invalid_grant"), "stderr: {stderr}");
    assert!(
        !stderr.contains("refresh-old"),
        "the refresh token was echoed back from the server error: {stderr}"
    );
    // The dead session is left on disk untouched: the user re-logs in, and
    // nothing was silently discarded.
    assert_eq!(stored_session(dir.path()).access_token, "access-old");
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_revokes_both_tokens_and_removes_the_credential_file() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        // Access token and refresh token are both offered.
        .expect(2)
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            let output = otl(&path, &base).args(["auth", "logout"]).output().unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(0), "stderr: {stderr}");
    // The reusable registration is the only thing left, so the file stays.
    let file = store(dir.path()).load().unwrap();
    assert!(file.profile("default").unwrap().oauth.is_none());
    assert!(
        file.profile("default").unwrap().client.is_some(),
        "a plain logout keeps the reusable registration"
    );
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn purge_deletes_the_dynamic_registration_from_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/oauth/clients/dcr-client-1"))
        .and(header("authorization", "Bearer rat-1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stdout, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            let output = otl(&path, &base)
                .args(["auth", "logout", "--purge"])
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("\"registration_deleted\": true"),
        "stdout: {stdout}"
    );
    assert!(
        !store(dir.path()).path().exists(),
        "purge must leave no credential file behind"
    );
    drop(server);
}
