//! The renew-and-replay hook on the single request channel.
//!
//! The engine stays service-agnostic here: the "renewal" a test source
//! performs is just handing back a different string. What is under test is
//! the channel's contract - when it asks, how often, and what it replays.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use engine::{
    Client, CredentialError, CredentialFault, CredentialSource, EngineError, OpSpec, ValidationMode,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OP_PATH: &str = "/api/things.info";

fn op() -> OpSpec {
    OpSpec {
        name: Cow::Borrowed("things.info"),
        path: Cow::Borrowed(OP_PATH),
        summary: Cow::Borrowed("Retrieve a thing"),
        content_type: Cow::Borrowed("application/json"),
        body_mode: engine::BodyMode::KeyValue,
        params: Cow::Borrowed(&[]),
    }
}

/// A source that hands out `stale` until asked to renew, then `fresh`.
struct Rotating {
    stale: String,
    fresh: String,
    renewals: AtomicUsize,
    /// Set once `renew` has run, so `bearer` follows suit.
    rotated: AtomicUsize,
}

impl Rotating {
    fn new(stale: &str, fresh: &str) -> Arc<Self> {
        Arc::new(Self {
            stale: stale.to_string(),
            fresh: fresh.to_string(),
            renewals: AtomicUsize::new(0),
            rotated: AtomicUsize::new(0),
        })
    }
}

impl CredentialSource for Rotating {
    fn bearer(&self) -> Result<String, CredentialError> {
        if self.rotated.load(Ordering::SeqCst) == 0 {
            Ok(self.stale.clone())
        } else {
            Ok(self.fresh.clone())
        }
    }

    fn renew(&self, rejected: &str) -> Result<Option<String>, CredentialError> {
        assert_eq!(rejected, self.stale, "channel renewed the wrong value");
        self.renewals.fetch_add(1, Ordering::SeqCst);
        self.rotated.store(1, Ordering::SeqCst);
        Ok(Some(self.fresh.clone()))
    }
}

/// A source that cannot renew at all (the default trait behaviour).
struct NoRenewal(&'static str);

impl CredentialSource for NoRenewal {
    fn bearer(&self) -> Result<String, CredentialError> {
        Ok(self.0.to_string())
    }
}

/// A source whose renewal always fails.
struct FailingRenewal(&'static str);

impl CredentialSource for FailingRenewal {
    fn bearer(&self) -> Result<String, CredentialError> {
        Ok(self.0.to_string())
    }

    fn renew(&self, _rejected: &str) -> Result<Option<String>, CredentialError> {
        Err(CredentialError::reauth_required(
            "stored credential can no longer be renewed",
        ))
    }
}

/// Mount a server that answers 401 for `stale` and 200 for `fresh`.
async fn rotating_server(stale: &str, fresh: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(OP_PATH))
        .and(header("authorization", format!("Bearer {stale}")))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "authentication_required",
            "message": "Authentication error"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(OP_PATH))
        .and(header("authorization", format!("Bearer {fresh}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "data": { "id": "thing-1" } })),
        )
        .expect(1)
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthorized_triggers_one_renewal_and_a_replay() {
    let server = rotating_server("stale-token", "fresh-token").await;
    let base_url = server.uri();
    let source = Rotating::new("stale-token", "fresh-token");

    let probe = Arc::clone(&source);
    let value = tokio::task::spawn_blocking(move || {
        let client = Client::with_credentials(&base_url, source)?;
        client.execute(&op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap()
    .expect("replay after renewal should succeed");

    assert_eq!(value["data"]["id"], "thing-1");
    assert_eq!(
        probe.renewals.load(Ordering::SeqCst),
        1,
        "the channel must ask for renewal exactly once"
    );
    // `.expect(1)` on both mocks: verified on drop.
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn renewal_is_attempted_at_most_once_per_request() {
    // A server that answers 401 no matter which credential arrives: the
    // channel must give up after a single replay rather than spin.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(OP_PATH))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "authentication_required",
            "message": "Authentication error"
        })))
        // One original request plus exactly one replay.
        .expect(2)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let source = Rotating::new("stale-token", "also-rejected");
    let probe = Arc::clone(&source);
    let error = tokio::task::spawn_blocking(move || {
        let client = Client::with_credentials(&base_url, source)?;
        client.execute(&op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap()
    .expect_err("a persistently rejected credential must surface the 401");

    assert!(
        matches!(error, EngineError::Api { status: 401, .. }),
        "expected the server's own 401, got {error:?}"
    );
    assert_eq!(probe.renewals.load(Ordering::SeqCst), 1);
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_cannot_renew_surfaces_the_401_unchanged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(OP_PATH))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "authentication_required",
            "message": "Authentication error"
        })))
        // No renewal is available, so there must be no replay.
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let error = tokio::task::spawn_blocking(move || {
        let client = Client::with_credentials(&base_url, Arc::new(NoRenewal("api-key")))?;
        client.execute(&op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap()
    .expect_err("401 without renewal must stay an API error");

    match error {
        EngineError::Api {
            status: 401,
            message,
            ..
        } => assert!(message.contains("Authentication error"), "{message}"),
        other => panic!("expected an API 401, got {other:?}"),
    }
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_renewal_surfaces_as_a_credential_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(OP_PATH))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "x" })))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let error = tokio::task::spawn_blocking(move || {
        let client = Client::with_credentials(&base_url, Arc::new(FailingRenewal("stale")))?;
        client.execute(&op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap()
    .expect_err("a failed renewal must not be reported as a plain 401");

    match error {
        EngineError::Credential(inner) => {
            assert_eq!(inner.fault, CredentialFault::ReauthRequired);
            assert!(inner.message.contains("renewed"), "{}", inner.message);
        }
        other => panic!("expected a credential error, got {other:?}"),
    }
    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_credential_fails_before_any_request() {
    struct Broken;
    impl CredentialSource for Broken {
        fn bearer(&self) -> Result<String, CredentialError> {
            Err(CredentialError::unavailable("no credential stored"))
        }
    }

    // Nothing is mounted: any request at all would surface as an
    // unmatched-request failure instead of the credential error below.
    let server = MockServer::start().await;
    let base_url = server.uri();
    let error = tokio::task::spawn_blocking(move || {
        let client = Client::with_credentials(&base_url, Arc::new(Broken))?;
        client.execute(&op(), &[], ValidationMode::Strict)
    })
    .await
    .unwrap()
    .expect_err("an unavailable credential must fail locally");

    match error {
        EngineError::Credential(inner) => assert_eq!(inner.fault, CredentialFault::Unavailable),
        other => panic!("expected a credential error, got {other:?}"),
    }
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a request went out despite having no credential"
    );
}
