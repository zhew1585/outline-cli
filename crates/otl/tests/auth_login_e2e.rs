//! `otl auth login`: discovery, client acquisition, consent, storage.
//!
//! Stories 2.1 and 2.2. The browser is played by the test - see
//! `oauth_harness` - so the real port binding, state check and code
//! exchange are all exercised.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod oauth_harness;

use oauth_harness::*;
use reqwest::Url;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

// --- transport and issuer refusals before any credential moves ------

// --- finding [2]: plaintext transport --------------------------------------

#[test]
fn a_plaintext_remote_instance_is_refused_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://docs.example.invalid")
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("https://"), "stderr: {stderr}");
    assert!(stderr.contains("instance URL"), "stderr: {stderr}");
    // Refused locally, so no DNS lookup or connection was attempted: the
    // message is about transport, not about the network.
    assert!(!stderr.contains("network error"), "stderr: {stderr}");
}

#[test]
fn a_plaintext_localhost_by_name_is_refused_too() {
    // `localhost` is a NAME: a resolver or hosts file can point it
    // elsewhere, so only IP literals get the loopback exception.
    let dir = tempfile::tempdir().unwrap();
    let output = otl(dir.path(), "http://localhost:3000")
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("127.0.0.1"), "stderr: {stderr}");
}

// --- finding [12]: RFC 8414 issuer ----------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_metadata_document_claiming_another_issuer_is_refused() {
    let server = MockServer::start().await;
    let base = server.uri();
    let mut document = metadata(&base);
    // Same origin, different tenant path: origin checks alone pass.
    document["issuer"] = json!(format!("{base}/other-tenant"));
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

    assert_eq!(run.code, Some(4), "stderr: {}", run.stderr);
    assert!(run.stderr.contains("issuer"), "stderr: {}", run.stderr);
    assert!(!store(dir.path()).path().exists());
}
