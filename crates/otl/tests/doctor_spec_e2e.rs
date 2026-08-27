//! End-to-end `otl doctor`: the SPEC half.
//!
//! Which operation table this binary dispatches from, and how it differs
//! from the online API description: operations that are missing here,
//! operations the document no longer declares, and operations it marks
//! deprecated. The environment half - config, credentials, reachability - is
//! `doctor_e2e.rs`.
//!
//! The rule every case here is really about: a spec finding is a WARNING,
//! because the CLI keeps dispatching from its local table and the
//! environment still works. The one exception is a `--spec-url` this CLI
//! will not fetch, which is the invocation being wrong rather than a third
//! party failing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{json, Value};

mod common;
use common::doctor::{check, deprecated_document, document, hits, instance, parse, run};
use common::doctor::{spec_host, Env, DEAD, KNOWN_OP, NEW_OP, SPEC_PATH};

/// An operation the online API declares and this build
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

/// An operation the online API marks deprecated and this
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

/// A `--spec-url` the fetch channel refuses locally is the INVOCATION being
/// wrong, not a third party failing: exit 2, like `otl spec sync` gives the
/// same mistake. A spec host that is merely unreachable stays a warning
/// (`an_unreachable_spec_source_is_a_warning_and_not_the_exit_code`).
#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_spec_url_is_a_usage_problem_not_a_warning() {
    let api = instance(200).await;
    let env = Env::new();
    let (stdout, stderr, code) = run(env.command(
        Some(&api.uri()),
        Some("test-key"),
        &["--spec-url", "not-a-url"],
    ))
    .await;

    assert_eq!(code, 2, "{stdout}{stderr}");
    let report = parse(&stdout);
    let online = check(&report, "online-spec");
    assert_eq!(online["status"], Value::from("problem"), "{online}");
    assert_eq!(online["exit_code"], Value::from(2));
    assert_eq!(online["checked"], Value::from(false));
    assert!(
        online["summary"]
            .as_str()
            .is_some_and(|text| text.contains("--spec-url")),
        "the offending flag must be named: {online}"
    );
    // Everything else was fine, so this is the finding that decided the code.
    assert_eq!(report["problems"], Value::from(1), "{report}");
    assert!(stderr.contains("online-spec"), "{stderr}");
}
