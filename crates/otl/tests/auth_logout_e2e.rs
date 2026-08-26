//! `otl auth logout`, `--purge`, and the registration lifecycle
//! (Story 2.4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod oauth_harness;

use oauth_harness::*;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

// --- finding [3]: credential-bearing requests must not follow redirects ---

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_purge_keeps_the_credential_that_can_retry_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    // The server refuses the deletion.
    Mock::given(method("DELETE"))
        .and(path("/oauth/clients/dcr-client-1"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({ "error": "unavailable" })))
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            let output = otl(&path, &base)
                .args(["auth", "logout", "--purge"])
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

    // Not success: the application is still registered on the server.
    assert_ne!(code, Some(0), "a failed purge reported success: {stderr}");
    assert!(stderr.contains("retried"), "stderr: {stderr}");

    // The management credential survived, so purge can actually be retried.
    let registration = stored_client(dir.path());
    assert_eq!(
        registration.registration_access_token.as_deref(),
        Some("rat-1"),
        "the only credential that can delete the orphan was discarded"
    );
    assert_eq!(
        registration.registration_client_uri.as_deref(),
        Some(&*format!("{base}/oauth/clients/dcr-client-1"))
    );
    // And so did the session: the DELETE can still succeed later, so
    // nothing that makes the retry possible is thrown away yet.
    assert!(
        !stored_session_absent(dir.path()),
        "a retryable failure destroyed the state the retry needs"
    );
    assert!(
        stderr.contains("--force"),
        "the way out must be stated: {stderr}"
    );
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn force_discards_local_credentials_after_a_failed_purge() {
    // The informed override: the user accepts that the application stays on
    // the server and the tokens stay live until they expire.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let base = server.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stderr) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            let output = otl(&path, &base)
                .args(["auth", "logout", "--purge", "--force"])
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

    // Still non-zero: the server-side step did not happen.
    assert_ne!(code, Some(0), "stderr: {stderr}");
    assert!(
        !store(dir.path()).path().exists(),
        "--force must discard the local credentials"
    );
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_purge_succeeds_once_the_server_recovers() {
    let server = MockServer::start().await;
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
    let code = tokio::task::spawn_blocking({
        let base = base.clone();
        move || {
            // A credential file left behind by a failed purge: registration
            // still recorded, session already gone.
            seed_session(&path, &base, Some(otl::auth::oauth::now_unix() + 3600));
            let store = store(&path);
            let mut file = store.load().unwrap();
            file.profile_mut("default").oauth = None;
            store.save(&file).unwrap();

            otl(&path, &base)
                .args(["auth", "logout", "--purge"])
                .output()
                .unwrap()
                .status
                .code()
        }
    })
    .await
    .unwrap();

    assert_eq!(code, Some(0));
    assert!(
        !store(dir.path()).path().exists(),
        "a confirmed purge must leave nothing behind"
    );
    drop(server);
}

// --- R3 [§1] / [26] / [28]: cleanup must not be one-way ------------------

#[tokio::test(flavor = "multi_thread")]
async fn logout_revokes_at_the_instance_that_issued_the_session() {
    // The R3 finding: revocation was anchored to OUTLINE_URL, so pointing
    // the shell at another instance refused to revoke A's tokens - while
    // still deleting them locally, turning a revocable token into one that
    // can never be revoked. The session records its own issuer; that is the
    // anchor. `dcr::delete` already worked this way, which is how the bug
    // was visible in a single command.
    let issuer = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        // Both tokens, at the instance that issued them.
        .expect(2)
        .mount(&issuer)
        .await;

    let issuer_url = issuer.uri();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let (code, stdout, stderr) = tokio::task::spawn_blocking({
        let issuer_url = issuer_url.clone();
        move || {
            seed_session(
                &path,
                &issuer_url,
                Some(otl::auth::oauth::now_unix() + 3600),
            );
            // Pointed somewhere else entirely, and at a plaintext remote
            // URL that the transport rule refuses for every other command.
            let output = otl(&path, "http://elsewhere.example.invalid")
                .args(["auth", "logout"])
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
        stdout.contains("\"revoked\": true"),
        "the tokens were not revoked at their own issuer: {stdout}"
    );
    assert!(stored_session_absent(dir.path()));
    // `.expect(2)` on the issuer's revocation endpoint is the assertion.
    drop(issuer);
}

#[test]
fn logout_works_for_a_profile_bound_to_a_plaintext_instance() {
    // [28]: `logout` used to run `instance_origin`, so a profile stored
    // against a plaintext URL - one that predates the rule, or was carried
    // from another machine - could not be cleaned up at all. The only way
    // out was deleting the file by hand, which takes the
    // registration_access_token with it and orphans the DCR registration.
    let dir = tempfile::tempdir().unwrap();
    seed_session(dir.path(), "http://legacy.example.invalid", Some(0));

    let output = otl(dir.path(), "http://legacy.example.invalid")
        .args(["auth", "logout"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The revocation endpoint is plaintext so it cannot be used, and no
    // retry would change that - the credentials still come off the machine.
    assert_ne!(
        output.status.code(),
        Some(2),
        "logout was blocked: {stderr}"
    );
    assert!(
        stored_session_absent(dir.path()),
        "a plaintext profile could not be cleaned up: {stderr}"
    );
}

#[test]
fn logout_works_with_no_instance_configured_at_all() {
    let dir = tempfile::tempdir().unwrap();
    seed_session(dir.path(), "https://a.example.com", Some(0));

    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("otl"));
    let output = cmd
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_CONFIG_DIR", dir.path())
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .args(["auth", "logout"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The point: logout is not blocked for want of an instance URL. It got
    // as far as trying to revoke at the session's own issuer, which is
    // unreachable here - so the credentials are kept for a retry, exactly
    // as designed.
    assert!(
        !stderr.contains("OUTLINE_URL is not set"),
        "logout demanded an instance URL it does not need: {stderr}"
    );
    assert!(
        stderr.contains("could not be revoked"),
        "logout never reached the revocation step: {stderr}"
    );

    // And `--force` completes the cleanup without an instance URL either.
    let forced = std::process::Command::new(assert_cmd::cargo::cargo_bin("otl"))
        .env_remove("OUTLINE_URL")
        .env_remove("OUTLINE_API_KEY")
        .env_remove("OUTLINE_PROFILE")
        .env("OUTLINE_CONFIG_DIR", dir.path())
        .env("OUTLINE_NO_KEY_WARNING", "1")
        .args(["auth", "logout", "--force"])
        .output()
        .unwrap();
    assert!(
        stored_session_absent(dir.path()),
        "stderr: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_revocation_keeps_the_tokens_and_exits_non_zero() {
    // [26]: the local copy used to be destroyed regardless, with exit 0 -
    // so the tokens stayed live on the server, nothing could revoke them
    // any more, and no script could tell.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(500))
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

    assert_eq!(
        code,
        Some(3),
        "a failed revocation reported success: {stderr}"
    );
    assert!(
        !stored_session_absent(dir.path()),
        "the only copy of a live token was destroyed: {stderr}"
    );
    assert!(stderr.contains("--force"), "stderr: {stderr}");
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn force_discards_tokens_that_could_not_be_revoked() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(500))
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
                .args(["auth", "logout", "--force"])
                .output()
                .unwrap()
                .status
                .code()
        }
    })
    .await
    .unwrap();

    // Still non-zero: the server was not told.
    assert_eq!(code, Some(3));
    assert!(stored_session_absent(dir.path()));
    drop(server);
}
