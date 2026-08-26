//! Automatic renewal on the request channel, and its single-flight
//! guarantee across processes (Story 2.3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod oauth_harness;

use oauth_harness::*;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
            drop(otl::auth::lock::CredentialLock::acquire(&path).unwrap());
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

// --- credential-bearing requests must not follow redirects ----------

#[tokio::test(flavor = "multi_thread")]
async fn a_redirecting_token_endpoint_never_forwards_the_refresh_token() {
    let attacker = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/steal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "attacker-issued",
            "token_type": "Bearer"
        })))
        .mount(&attacker)
        .await;

    let server = MockServer::start().await;
    mount_redirecting_token_endpoint(&server, &attacker.uri()).await;
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

    assert_ne!(code, Some(0), "the redirect was followed: {stderr}");
    // The attacker's server saw nothing at all.
    let stolen = attacker.received_requests().await.unwrap_or_default();
    assert!(
        stolen.is_empty(),
        "the refresh token was forwarded to a redirect target: {} request(s)",
        stolen.len()
    );
    // And the 3xx is surfaced as the unexpected status it is.
    assert!(stderr.contains("308"), "stderr: {stderr}");
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redirecting_api_endpoint_never_forwards_the_bearer_token() {
    // Same exposure on the ordinary request channel: reqwest only strips
    // Authorization across a HOST change, so a same-host scheme downgrade
    // would keep it - and bodies are never stripped.
    let attacker = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/steal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&attacker)
        .await;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/documents.info"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/steal", attacker.uri())),
        )
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let code = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            otl(&path, &base)
                .args(["api", "documents.info", "id=doc-1"])
                .output()
                .unwrap()
                .status
                .code()
        }
    })
    .await
    .unwrap();

    assert_ne!(code, Some(0), "the redirect was followed");
    assert!(
        attacker
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the bearer token was forwarded to a redirect target"
    );
    drop(server);
}
