//! Fixtures shared by the two `otl doctor` suites.
//!
//! `doctor_e2e.rs` covers the ENVIRONMENT half - config file, instance,
//! credential file, the credential a request would carry, reachability - and
//! `doctor_spec_e2e.rs` covers the SPEC half: which operation table is in
//! use and how it differs from the online description. They are split
//! because those are different subjects with different fixtures, and because
//! one file of both was over the 800-line limit.
//!
//! Everything here is inert: mock servers, throwaway directories, and the
//! `otl doctor` command with every machine-dependent input scrubbed. No
//! fixture reaches the real network, the developer's credential file, or
//! their spec cache.

#![allow(dead_code)]

use std::path::Path;

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use otl::auth::credentials::{CredentialFile, CredentialStore};

/// Path the mock spec host serves the document from.
pub const SPEC_PATH: &str = "/openapi/spec3.json";

/// An address nothing listens on: port 1 is reserved and a loopback IP
/// literal, so the transport rule allows plain `http` for it.
pub const DEAD: &str = "http://127.0.0.1:1";

/// An operation the vendored spec certainly has.
pub const KNOWN_OP: &str = "documents.info";
/// An operation name no vendored spec has.
pub const NEW_OP: &str = "things.brandNew";

/// A document declaring exactly the named operations.
pub fn document(ops: &[&str]) -> String {
    let entries: Vec<String> = ops
        .iter()
        .map(|name| {
            format!(
                r#""/{name}":{{"post":{{"summary":"An operation",
                   "requestBody":{{"content":{{"application/json":{{"schema":{{
                     "type":"object","properties":{{"id":{{"type":"string"}}}}}}}}}}}}}}}}"#
            )
        })
        .collect();
    format!(r#"{{"openapi":"3.0.0","paths":{{{}}}}}"#, entries.join(","))
}

/// A document declaring `KNOWN_OP` and marking it deprecated.
pub fn deprecated_document() -> String {
    format!(
        r#"{{"openapi":"3.0.0","paths":{{"/{KNOWN_OP}":{{"post":{{
             "summary":"An operation","deprecated":true}}}}}}}}"#
    )
}

/// A spec host serving `body` at [`SPEC_PATH`].
pub async fn spec_host(body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SPEC_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    server
}

/// An Outline instance answering `auth.info` with `status`.
pub async fn instance(status: u16) -> MockServer {
    let server = MockServer::start().await;
    let body = json!({
        "data": {
            "user": { "name": "Alice Example", "email": "alice@example.com" },
            "team": { "name": "Acme" }
        }
    });
    let response = if status == 200 {
        ResponseTemplate::new(200).set_body_json(body)
    } else {
        ResponseTemplate::new(status).set_body_json(json!({ "error": "unauthorized" }))
    };
    Mock::given(method("POST"))
        .and(path("/api/auth.info"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

/// An Outline instance answering `auth.info` with 200 and a body that is not
/// JSON at all - a captive portal, a proxy error page, an HTML login form.
pub async fn instance_answering(body: &'static str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth.info"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    server
}

/// Everything a `doctor` run needs pointed somewhere harmless.
pub struct Env {
    /// The credential directory (empty unless a test seeds it).
    pub config_dir: TempDir,
    /// A cache directory, when a test needs a real one. `common::isolate`
    /// otherwise points the CLI at a directory that cannot contain a cache.
    pub cache_dir: Option<TempDir>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            config_dir: tempfile::tempdir().unwrap(),
            cache_dir: None,
        }
    }

    pub fn dir(&self) -> &Path {
        self.config_dir.path()
    }

    /// Put something that is not a cache where the cache belongs.
    pub fn with_damaged_cache(mut self) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ir-cache.bin"), b"this is not a cache").unwrap();
        self.cache_dir = Some(dir);
        self
    }

    /// Store an API key for the default profile, bound to `base`.
    pub fn seed_key(&self, base: &str, key: &str) {
        let store = CredentialStore::at(self.dir().to_path_buf());
        let mut file = CredentialFile::default();
        let entry = file.profile_mut("default");
        entry.origin = engine::base_url_origin(base);
        entry.api_key = Some(key.to_string());
        store.save(&file).unwrap();
    }

    /// `otl doctor` with the given extra arguments and environment.
    pub fn command(&self, url: Option<&str>, key: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("otl").unwrap();
        super::isolate(&mut cmd);
        cmd.env("OUTLINE_CONFIG_DIR", self.dir())
            .env_remove("OUTLINE_URL")
            .env_remove("OUTLINE_API_KEY");
        if let Some(cache) = &self.cache_dir {
            cmd.env(super::CACHE_DIR_ENV, cache.path());
        }
        if let Some(url) = url {
            cmd.env("OUTLINE_URL", url);
        }
        if let Some(key) = key {
            cmd.env("OUTLINE_API_KEY", key);
        }
        cmd.arg("doctor").arg("--json").args(args);
        cmd
    }
}

/// Run a command off the async runtime; returns (stdout, stderr, exit code).
pub async fn run(mut cmd: Command) -> (String, String, i32) {
    tokio::task::spawn_blocking(move || {
        let output = cmd.output().unwrap();
        (
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.code().unwrap_or(-1),
        )
    })
    .await
    .unwrap()
}

/// The report object, which must be printed whatever the exit code.
pub fn parse(stdout: &str) -> Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|error| panic!("stdout is not the JSON report ({error}): {stdout}"))
}

/// One check out of a report.
pub fn check<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks is an array")
        .iter()
        .find(|check| check["check"] == key)
        .unwrap_or_else(|| panic!("no {key} check in {report}"))
}

/// How many requests a mock server received.
pub async fn hits(server: &MockServer) -> usize {
    server.received_requests().await.map_or(0, |all| all.len())
}
