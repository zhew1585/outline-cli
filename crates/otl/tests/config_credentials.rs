//! Story 2.x integration: WHICH store a credential is released from, and
//! whether the credential file is subject to the same gate as the
//! environment.
//!
//! `config_binding.rs` covers the gate itself with the environment source.
//! This file covers the second source Epic 2 added - a credential read out of
//! the credential file - and the selection rule that decides between them.
//! The two questions are kept apart on purpose: a selection bug sends the
//! wrong secret, a gate bug sends it to the wrong instance, and only one of
//! those is caught by asserting on the value that comes back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use otl::config::{
    resolve_settings, AuthMethod, Config, ConfigError, CredentialSource, EnvLayer, Overrides,
    Settings, StoredCredential,
};
use tempfile::TempDir;

/// The path a diagnostic should name. Never read - the file is `auth`'s.
const CREDENTIAL_PATH: &str = "/tmp/does-not-need-to-exist/credentials.toml";

/// A secret that must only ever come back from a release that was approved.
const STORED: &str = "stored-key-4f21";

/// A secret in the environment, distinguishable from the stored one.
const FROM_ENV: &str = "env-key-9c07";

/// Write a config file into a fresh temp dir and return (dir, path).
fn config_file(body: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, body).unwrap();
    (dir, path)
}

/// Overrides naming an explicit config file, so the user's real config is
/// never consulted.
fn overrides_for(path: &Path) -> Overrides {
    Overrides {
        config_path: Some(path.to_path_buf()),
        ..Overrides::default()
    }
}

/// Resolve settings from explicit layers, exactly as the binary does.
fn settings(overrides: &Overrides, env: &EnvLayer) -> Settings {
    let loaded = otl::config::load_file(overrides, env).unwrap();
    resolve_settings(overrides, env, &loaded).unwrap()
}

/// Settings for a single instance with no profile in effect - the Epic 1
/// shape, which the gate lets through unconditionally because there is no
/// scoping question to ask.
fn plain_settings(env: &EnvLayer) -> (TempDir, Settings) {
    let (dir, path) = config_file("");
    let env = env.clone().with_url("https://docs.example.com");
    let resolved = settings(&overrides_for(&path), &env);
    (dir, resolved)
}

/// Settings for a profile that selects OAuth and names its own instance.
///
/// `auth` is a per-profile key, so an OAuth setting always comes with a
/// profile - and the profile names the url, which is the binding the gate
/// approves on.
fn oauth_settings() -> (TempDir, Settings) {
    let (dir, path) =
        config_file("[profiles.work]\nurl = \"https://work.example.com\"\nauth = \"oauth\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let resolved = settings(&overrides, &EnvLayer::default());
    (dir, resolved)
}

/// A stored credential, or the absence of one, at [`CREDENTIAL_PATH`].
fn stored(token: Option<&str>) -> StoredCredential<'_> {
    StoredCredential::new(token, Path::new(CREDENTIAL_PATH))
}

// ---------------------------------------------------------------------------
// Selection: which store, decided from the settings and nothing else.
// ---------------------------------------------------------------------------

#[test]
fn oauth_can_only_come_from_the_credential_file() {
    // An environment variable cannot hold a renewable session, so `oauth`
    // must not silently fall back to one - a user who configured a browser
    // login and has a stale OUTLINE_API_KEY exported would otherwise be
    // authenticating as something other than what they asked for.
    let (_dir, resolved) = oauth_settings();
    assert_eq!(resolved.auth(), AuthMethod::Oauth);
    for present in [true, false] {
        assert_eq!(
            otl::config::select_credential_source(&resolved, present),
            CredentialSource::CredentialFile,
            "oauth fell back to the environment (file has credential: {present})"
        );
    }
}

#[test]
fn a_stored_key_outranks_one_in_the_environment() {
    // `otl auth set-key` is a deliberate act and stores the key owner-only.
    // An exported variable is often left over from another shell and is
    // readable by every process the user starts.
    let (_dir, resolved) = plain_settings(&EnvLayer::default());
    assert_eq!(
        otl::config::select_credential_source(&resolved, true),
        CredentialSource::CredentialFile
    );
}

#[test]
fn the_environment_is_the_fallback_when_nothing_is_stored() {
    let (_dir, resolved) = plain_settings(&EnvLayer::default());
    assert_eq!(
        otl::config::select_credential_source(&resolved, false),
        CredentialSource::Environment
    );
}

// ---------------------------------------------------------------------------
// Release: the selected store's secret, and only after the gate approved it.
// ---------------------------------------------------------------------------

#[test]
fn the_selected_store_is_the_one_that_supplies_the_secret() {
    // Both stores hold something DIFFERENT, so the value that comes back
    // says which one was consulted. Asserting only "a key came back" would
    // pass with the selection rule inverted.
    let env = EnvLayer::default().with_api_key(FROM_ENV);
    let (_dir, resolved) = plain_settings(&env);

    let from_file = Config::release(&resolved, &env, &stored(Some(STORED))).unwrap();
    assert_eq!(from_file.api_key, STORED);

    let from_env = Config::release(&resolved, &env, &stored(None)).unwrap();
    assert_eq!(from_env.api_key, FROM_ENV);
}

#[test]
fn an_empty_credential_file_under_oauth_names_the_command_that_fills_it() {
    // The remedy is a different command from the environment's, so the
    // diagnostic has to be a different one too.
    let (_dir, resolved) = oauth_settings();
    let error = Config::release(&resolved, &EnvLayer::default(), &stored(None)).unwrap_err();
    assert!(
        matches!(error, ConfigError::MissingStoredCredential { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("otl auth login"), "{message}");
    assert!(message.contains("credentials.toml"), "{message}");
}

#[test]
fn a_stored_credential_is_not_released_to_another_profiles_instance() {
    // THE POINT OF THE SEAM. The gate refuses an environment key here (see
    // `config_binding.rs`); it must refuse a stored one identically, because
    // it is `release_token` that decides and not the source. If this ever
    // returns `Ok`, the credential file has acquired a way around the gate.
    let (_dir, path) = config_file("[profiles.work]\nurl = \"https://work.example.com\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://elsewhere.example.com")
        .with_profile_api_key("work", FROM_ENV);
    let resolved = settings(&overrides, &env);

    let error = Config::release(&resolved, &env, &stored(Some(STORED))).unwrap_err();
    assert!(
        matches!(error, ConfigError::ConflictingUrl { .. }),
        "a stored credential was released for an origin the profile never \
         named: {error:?}"
    );
    // And the secret is not in the diagnostic either.
    assert!(!error.to_string().contains(STORED));
    assert!(!format!("{error:?}").contains(STORED));
}

#[test]
fn a_stored_credential_is_not_released_when_the_profile_named_no_instance() {
    // Same rule, the other refusal: a profile scopes the credential but
    // declares no url, so an ambient OUTLINE_URL would be choosing where the
    // stored secret goes.
    let (_dir, path) = config_file("[profiles.work]\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://elsewhere.example.com")
        .with_profile_api_key("work", FROM_ENV);
    let resolved = settings(&overrides, &env);

    let error = Config::release(&resolved, &env, &stored(Some(STORED))).unwrap_err();
    assert!(
        matches!(error, ConfigError::UnboundProfileCredential { .. }),
        "{error:?}"
    );
    assert!(!error.to_string().contains(STORED));
}

#[test]
fn a_profile_that_named_this_instance_does_get_its_stored_credential() {
    // Guard against over-refusing: the whole mechanism is worthless if the
    // legitimate case is blocked, and a test suite of refusals alone cannot
    // tell "refuses correctly" from "refuses always".
    let (_dir, path) = config_file("[profiles.work]\nurl = \"https://work.example.com\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://work.example.com")
        .with_profile_api_key("work", FROM_ENV);
    let resolved = settings(&overrides, &env);

    let released = Config::release(&resolved, &env, &stored(Some(STORED))).unwrap();
    assert_eq!(released.api_key, STORED);
    assert_eq!(released.base_url, "https://work.example.com");
}

#[test]
fn a_released_config_never_shows_the_credential_in_debug_output() {
    let (_dir, resolved) = plain_settings(&EnvLayer::default());
    let released = Config::release(&resolved, &EnvLayer::default(), &stored(Some(STORED))).unwrap();
    let debug = format!("{released:?}");
    assert!(!debug.contains(STORED), "{debug}");
}
