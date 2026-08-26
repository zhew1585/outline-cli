//! End-to-end `otl doctor` (Story 4.3, FR23).
//!
//! Every case runs the real binary against wiremock servers and throwaway
//! directories: no test touches the real network, the developer's credential
//! file, or their spec cache (`common::isolate`).
//!
//! Two properties get most of the attention here, because they are the ones
//! a report can get wrong while still looking right:
//!
//! 1. **the exit code names the right check**, so a script can act on it;
//! 2. **a request is made only when it should be** - the mock servers are
//!    asked how many requests they received, which is the only way to tell
//!    "skipped" from "silently succeeded".

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use otl::auth::credentials::{CredentialFile, CredentialStore};

mod common;

/// Path the mock spec host serves the document from.
const SPEC_PATH: &str = "/openapi/spec3.json";

/// An address nothing listens on: port 1 is reserved and a loopback IP
/// literal, so the transport rule allows plain `http` for it.
const DEAD: &str = "http://127.0.0.1:1";

/// An operation the vendored spec certainly has.
const KNOWN_OP: &str = "documents.info";
/// An operation name no vendored spec has.
const NEW_OP: &str = "things.brandNew";

/// A document declaring exactly the named operations.
fn document(ops: &[&str]) -> String {
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
fn deprecated_document() -> String {
    format!(
        r#"{{"openapi":"3.0.0","paths":{{"/{KNOWN_OP}":{{"post":{{
             "summary":"An operation","deprecated":true}}}}}}}}"#
    )
}

/// A spec host serving `body` at [`SPEC_PATH`].
async fn spec_host(body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SPEC_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    server
}

/// An Outline instance answering `auth.info` with `status`.
async fn instance(status: u16) -> MockServer {
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

/// Everything a `doctor` run needs pointed somewhere harmless.
struct Env {
    /// The credential directory (empty unless a test seeds it).
    config_dir: TempDir,
    /// A cache directory, when a test needs a real one. `common::isolate`
    /// otherwise points the CLI at a directory that cannot contain a cache.
    cache_dir: Option<TempDir>,
}

impl Env {
    fn new() -> Self {
        Self {
            config_dir: tempfile::tempdir().unwrap(),
            cache_dir: None,
        }
    }

    fn dir(&self) -> &Path {
        self.config_dir.path()
    }

    /// Put something that is not a cache where the cache belongs.
    fn with_damaged_cache(mut self) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ir-cache.bin"), b"this is not a cache").unwrap();
        self.cache_dir = Some(dir);
        self
    }

    /// Store an API key for the default profile, bound to `base`.
    fn seed_key(&self, base: &str, key: &str) {
        let store = CredentialStore::at(self.dir().to_path_buf());
        let mut file = CredentialFile::default();
        let entry = file.profile_mut("default");
        entry.origin = engine::base_url_origin(base);
        entry.api_key = Some(key.to_string());
        store.save(&file).unwrap();
    }

    /// `otl doctor` with the given extra arguments and environment.
    fn command(&self, url: Option<&str>, key: Option<&str>, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("otl").unwrap();
        common::isolate(&mut cmd);
        cmd.env("OUTLINE_CONFIG_DIR", self.dir())
            .env_remove("OUTLINE_URL")
            .env_remove("OUTLINE_API_KEY");
        if let Some(cache) = &self.cache_dir {
            cmd.env(common::CACHE_DIR_ENV, cache.path());
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
async fn run(mut cmd: Command) -> (String, String, i32) {
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
fn parse(stdout: &str) -> Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|error| panic!("stdout is not the JSON report ({error}): {stdout}"))
}

/// One check out of a report.
fn check<'a>(report: &'a Value, key: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .expect("checks is an array")
        .iter()
        .find(|check| check["check"] == key)
        .unwrap_or_else(|| panic!("no {key} check in {report}"))
}

/// How many requests a mock server received.
async fn hits(server: &MockServer) -> usize {
    server.received_requests().await.map_or(0, |all| all.len())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_working_environment_reports_every_check_and_exits_zero() {
    let api = instance(200).await;
    let specs = spec_host(document(&[KNOWN_OP])).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(
        Some(&api.uri()),
        Some("test-key"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    assert_eq!(code, 0, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(report["healthy"], Value::from(true), "{report}");
    assert_eq!(report["exit_code"], Value::from(0));
    assert_eq!(report["problems"], Value::from(0));

    // The instance was really contacted, through the request channel, and
    // the identity it returned is in the report.
    let connectivity = check(&report, "connectivity");
    assert_eq!(connectivity["status"], Value::from("ok"), "{connectivity}");
    assert_eq!(connectivity["reachable"], Value::from(true));
    assert_eq!(
        connectivity["account"],
        Value::from("Alice Example <alice@example.com>")
    );
    assert_eq!(connectivity["workspace"], Value::from("Acme"));
    assert_eq!(hits(&api).await, 1, "the probe must be exactly one request");

    // The credential the gate approved is named, and it is the environment
    // key - not something doctor read on its own.
    assert!(
        check(&report, "credential")["method"]
            .as_str()
            .unwrap()
            .contains("OUTLINE_API_KEY"),
        "{report}"
    );
    // And the local table is described.
    let local = check(&report, "local-spec");
    assert_eq!(local["synced"], Value::from(false));
    assert!(local["operations"].as_u64().unwrap() > 100, "{local}");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_credential_anywhere_exits_two_and_never_contacts_the_instance() {
    let api = instance(200).await;
    let env = Env::new();
    let (stdout, stderr, code) = run(env.command(Some(&api.uri()), None, &["--offline"])).await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    assert_eq!(report["healthy"], Value::from(false));
    assert_eq!(report["exit_code"], Value::from(2));

    // The credential check is the blocking one, and it names every way in.
    let credential = check(&report, "credential");
    assert_eq!(credential["status"], Value::from("problem"), "{credential}");
    assert_eq!(credential["exit_code"], Value::from(2));
    let detail = credential["detail"].to_string();
    assert!(detail.contains("otl auth login"), "{detail}");
    assert!(detail.contains("otl auth set-key"), "{detail}");
    // The finding reaches stderr too, since that is what `main` prints.
    assert!(stderr.contains("credential"), "{stderr}");
    // Everything local is still reported rather than abandoned.
    assert_eq!(check(&report, "instance")["status"], Value::from("ok"));
    assert_eq!(hits(&api).await, 0, "nothing may be sent without one");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_instance_exits_seven_from_the_connectivity_check() {
    let env = Env::new();
    let (stdout, stderr, code) = run(env.command(Some(DEAD), Some("test-key"), &[])).await;

    assert_eq!(code, 7, "{stdout}{stderr}");
    let report = parse(&stdout);
    let connectivity = check(&report, "connectivity");
    assert_eq!(connectivity["status"], Value::from("problem"), "{report}");
    assert_eq!(connectivity["exit_code"], Value::from(7));
    assert_eq!(connectivity["reachable"], Value::from(false));
    // Nothing local is blamed for it: the code came from the network check.
    for local in ["configuration", "instance", "credentials", "credential"] {
        assert_ne!(
            check(&report, local)["status"],
            Value::from("problem"),
            "{local} must not be a problem here: {report}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_credential_exits_four() {
    let api = instance(401).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(Some(&api.uri()), Some("test-key"), &[])).await;

    assert_eq!(code, 4, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(
        check(&report, "connectivity")["exit_code"],
        Value::from(4),
        "{report}"
    );
}

/// A credential file other users can read is exit 2 - and doctor must not
/// send anything while the store it would read from is compromised.
///
/// The environment key is exported ON PURPOSE: without it, "nothing was
/// sent" would hold simply because there was nothing to send, and the test
/// would pass even if `doctor` decided to ignore the unusable file. With it,
/// there IS a usable credential and the run still has to refuse to use one.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_over_wide_credential_file_exits_two_before_anything_is_sent() {
    use std::os::unix::fs::PermissionsExt;

    let api = instance(200).await;
    let env = Env::new();
    env.seed_key(&api.uri(), "KEY-SECRET-9c7a");
    let file = env.dir().join("credentials.toml");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), Some("ENV-SECRET-3f1b"), &[])).await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let credentials = check(&report, "credentials");
    assert_eq!(credentials["status"], Value::from("problem"), "{report}");
    assert_eq!(credentials["usable"], Value::from(false));
    // The offending file and its mode are named, with the fix.
    let text = credentials.to_string();
    assert!(text.contains(&file.display().to_string()), "{text}");
    assert!(text.contains("0644"), "{text}");
    assert!(text.contains("chmod 600"), "{text}");
    assert_eq!(hits(&api).await, 0, "a compromised store must send nothing");
    // And no part of the credential is anywhere in the output.
    for secret in ["SECRET-9c7a", "SECRET-3f1b"] {
        assert!(!stdout.contains(secret), "{secret} in stdout: {stdout}");
        assert!(!stderr.contains(secret), "{secret} in stderr: {stderr}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_contacts_neither_the_instance_nor_the_spec_host() {
    let api = instance(200).await;
    let specs = spec_host(document(&[KNOWN_OP])).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(
        Some(&api.uri()),
        Some("test-key"),
        &[
            "--offline",
            "--spec-url",
            &format!("{}{SPEC_PATH}", specs.uri()),
        ],
    ))
    .await;

    assert_eq!(code, 0, "{stdout}");
    let report = parse(&stdout);
    assert_eq!(
        check(&report, "connectivity")["status"],
        Value::from("skipped"),
        "{report}"
    );
    assert_eq!(
        check(&report, "online-spec")["status"],
        Value::from("skipped")
    );
    assert_eq!(check(&report, "online-spec")["checked"], Value::from(false));
    assert_eq!(hits(&api).await, 0, "--offline sent a request");
    assert_eq!(hits(&specs).await, 0, "--offline fetched a document");
}

/// FR23, first half: an operation the online API declares and this build
/// does not is reported by name, with the command that fixes it. The
/// instance is deliberately unreachable, so this also shows the online
/// comparison still runs when the instance check has already failed.
#[tokio::test(flavor = "multi_thread")]
async fn the_online_comparison_names_missing_and_withdrawn_operations() {
    let specs = spec_host(document(&[KNOWN_OP, NEW_OP])).await;
    let env = Env::new();
    let (stdout, _, _) = run(env.command(
        Some(DEAD),
        Some("test-key"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    let report = parse(&stdout);
    let online = check(&report, "online-spec");
    assert_eq!(online["status"], Value::from("warn"), "{online}");
    assert_eq!(online["checked"], Value::from(true));
    assert_eq!(online["missing"], json!([NEW_OP]), "{online}");
    // Everything the built-in table has and this document does not.
    let withdrawn = online["withdrawn"].as_array().unwrap();
    assert!(
        withdrawn.len() > 100 && !withdrawn.contains(&Value::from(KNOWN_OP)),
        "{online}"
    );
    // Named in the human-readable detail as well, with the remedy.
    let detail = online["detail"].to_string();
    assert!(detail.contains(NEW_OP), "{detail}");
    assert!(detail.contains("otl spec sync"), "{detail}");
    assert_eq!(hits(&specs).await, 1, "one document fetch, no more");
}

/// FR23, second half: an operation the online API marks deprecated and this
/// build still offers.
#[tokio::test(flavor = "multi_thread")]
async fn a_deprecated_operation_this_build_still_offers_is_reported() {
    let specs = spec_host(deprecated_document()).await;
    let env = Env::new();
    let (stdout, _, _) = run(env.command(
        Some(DEAD),
        Some("test-key"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    let report = parse(&stdout);
    let online = check(&report, "online-spec");
    assert_eq!(online["deprecated"], json!([KNOWN_OP]), "{online}");
    assert!(
        online["detail"].to_string().contains("deprecated online"),
        "{online}"
    );
    assert_eq!(online["status"], Value::from("warn"));
}

/// A spec host that cannot be reached is a WARNING: the CLI dispatches from
/// its local table, so the environment still works. The exit code must not
/// come from a third-party host.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_spec_source_is_a_warning_and_not_the_exit_code() {
    let api = instance(200).await;
    let env = Env::new();
    let (stdout, _, code) = run(env.command(
        Some(&api.uri()),
        Some("test-key"),
        &["--spec-url", &format!("{DEAD}{SPEC_PATH}")],
    ))
    .await;

    assert_eq!(code, 0, "a spec host must not fail a doctor run: {stdout}");
    let report = parse(&stdout);
    let online = check(&report, "online-spec");
    assert_eq!(online["status"], Value::from("warn"), "{online}");
    assert_eq!(online["checked"], Value::from(false));
    assert_eq!(report["healthy"], Value::from(true));
    // The message talks about the spec source, never about the API key or
    // the instance URL, neither of which is involved in fetching a document.
    let detail = online["detail"].to_string();
    assert!(!detail.contains("OUTLINE_API_KEY"), "{detail}");
    assert!(!detail.contains("OUTLINE_URL"), "{detail}");
}

/// A document whose operation name carries terminal escapes is refused, and
/// nothing it contained reaches either stream as an executable byte.
///
/// Three layers already close this: the compiler rejects the name, the
/// rejection renders it with `Debug` (which escapes control characters as
/// text), and `doctor`'s own rendering scrubs at the sink. This is the
/// regression guard over all three - a change that echoed the name raw
/// would fail here.
#[tokio::test(flavor = "multi_thread")]
async fn a_hostile_document_is_refused_and_nothing_executable_is_printed() {
    // The escapes are written as JSON escapes, so the document the server
    // serves carries real ESC and BEL bytes while this source file does not.
    let hostile = concat!(
        r#"{"openapi":"3.0.0","paths":{"/things\u001b]52;c;cGF3bmVk\u0007.info":"#,
        r#"{"post":{"summary":"x"}}}}"#
    )
    .to_string();
    let specs = spec_host(hostile).await;
    let env = Env::new();
    let (stdout, stderr, code) = run(env.command(
        Some(DEAD),
        Some("test-key"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    // The exit code is the instance's (unreachable), never the document's.
    assert_eq!(code, 7, "{stdout}{stderr}");
    let report = parse(&stdout);
    let online = check(&report, "online-spec");
    assert_eq!(online["status"], Value::from("warn"), "{online}");
    assert_eq!(online["checked"], Value::from(false));
    assert!(
        online["detail"].to_string().contains("cannot be used"),
        "the refusal must be reported: {online}"
    );
    for (stream, text) in [("stdout", &stdout), ("stderr", &stderr)] {
        assert!(
            !text.contains('\u{1b}'),
            "an escape sequence reached {stream}: {text}"
        );
        assert!(!text.contains('\u{7}'), "a BEL reached {stream}: {text}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_report_never_carries_a_credential_or_a_fragment_of_one() {
    let api = instance(200).await;
    let specs = spec_host(document(&[KNOWN_OP])).await;
    let env = Env::new();
    env.seed_key(&api.uri(), "STORED-SECRET-9c7a");
    let (stdout, stderr, code) = run(env.command(
        Some(&api.uri()),
        Some("ENV-SECRET-3f1b"),
        &["--spec-url", &format!("{}{SPEC_PATH}", specs.uri())],
    ))
    .await;

    assert_eq!(code, 0, "{stdout}{stderr}");
    let both = format!("{stdout}\n{stderr}");
    for secret in [
        "STORED-SECRET-9c7a",
        "ENV-SECRET-3f1b",
        "SECRET",
        "9c7a",
        "3f1b",
    ] {
        assert!(
            !both.contains(secret),
            "{secret} appears in doctor output: {both}"
        );
    }
    // The report still SAYS what is stored, which is the point of it.
    let report = parse(&stdout);
    assert!(
        check(&report, "credentials")["profiles_stored"]
            .to_string()
            .contains("api key"),
        "{report}"
    );
}

/// A cache that cannot be used is documented as NOT an error: the CLI
/// discards it and falls back to the spec built into the binary. `doctor`
/// must therefore warn, name the remedy, and still exit 0.
#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_spec_cache_is_a_warning_and_the_built_in_table_is_used() {
    let api = instance(200).await;
    let env = Env::new().with_damaged_cache();
    let (stdout, stderr, code) =
        run(env.command(Some(&api.uri()), Some("test-key"), &["--offline"])).await;

    assert_eq!(code, 0, "a damaged cache must not fail a run: {stdout}");
    let report = parse(&stdout);
    let local = check(&report, "local-spec");
    assert_eq!(local["status"], Value::from("warn"), "{local}");
    // The built-in table is in use, and the report says which.
    assert_eq!(local["synced"], Value::from(false));
    assert!(local["operations"].as_u64().unwrap() > 100, "{local}");
    let detail = local["detail"].to_string();
    assert!(detail.contains("damaged"), "{detail}");
    assert!(
        detail.contains("otl spec sync") || detail.contains("otl spec reset"),
        "the remedy must be named: {detail}"
    );
    // The path of the offending file, so the user can look at it.
    assert!(
        detail.contains("ir-cache.bin"),
        "the cache file must be named: {detail}"
    );
    // The CLI's own one-line warning is on stderr, where diagnostics go.
    assert!(stderr.contains("spec cache"), "{stderr}");
}

/// A config file that cannot be parsed is the FIRST thing to fix, so it is
/// the check that decides the exit code even though later checks fail too.
#[tokio::test(flavor = "multi_thread")]
async fn an_unparsable_config_file_exits_two_from_the_configuration_check() {
    let env = Env::new();
    let bad = env.dir().join("broken.toml");
    std::fs::write(&bad, "default_profile = \n[profiles.work]\n").unwrap();
    let (stdout, stderr, code) = run(env.command(
        None,
        Some("test-key"),
        &["--offline", "--config", &bad.display().to_string()],
    ))
    .await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let configuration = check(&report, "configuration");
    assert_eq!(
        configuration["status"],
        Value::from("problem"),
        "{configuration}"
    );
    assert_eq!(configuration["exit_code"], Value::from(2));
    // It is the FIRST check, so it is the one that decided the exit code
    // even though the instance check failed for the same reason.
    assert_eq!(report["checks"][0]["check"], Value::from("configuration"));
    assert!(
        configuration["detail"].to_string().contains("line 1"),
        "the parse error must locate itself: {configuration}"
    );
    assert!(stderr.contains("configuration"), "{stderr}");
}
