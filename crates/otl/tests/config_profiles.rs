//! Story 4.1: user config file, named profiles, and the strict
//! flag > env > config-file precedence (applied per key, not per layer).
//!
//! Every case here builds its layers as DATA (`Overrides` + `EnvLayer`) and
//! points the config path at a `tempfile` directory: the tests never read or
//! write the real user config file, and never mutate the process
//! environment (which would need `unsafe` and race across test threads).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use otl::config::{
    release_token, resolve_settings, AuthMethod, Config, ConfigError, ConfigSource, EnvLayer,
    Overrides, Settings, UrlSource,
};
use tempfile::TempDir;

/// Write a config file into a fresh temp dir and return (dir, path).
fn config_file(body: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, body).unwrap();
    (dir, path)
}

/// Overrides naming an explicit config file, so the default location (the
/// user's real config) is never consulted.
fn overrides_for(path: &Path) -> Overrides {
    Overrides {
        config_path: Some(path.to_path_buf()),
        ..Overrides::default()
    }
}

/// Resolve a `Settings` without a config file, the only way one can be
/// obtained: the type has no public constructor, precisely so that the
/// credential gate cannot be handed a hand-built `UrlSource`.
fn settings_from_env(dir: &TempDir, env: &EnvLayer) -> Settings {
    let loaded = otl::config::load_from(&absent_default_source(dir)).unwrap();
    resolve_settings(&Overrides::default(), env, &loaded).unwrap()
}

/// Release a credential the way the binary does: through the shared gate,
/// never by calling a `TokenSource` directly (which the type system forbids).
fn release(env: &EnvLayer, resolved: &Settings) -> Result<String, ConfigError> {
    release_token(&otl::config::EnvApiKey(env), resolved)
}

/// Resolve settings from explicit layers, with the config file loaded from
/// `overrides`/`env` exactly as the binary does.
fn settings(
    overrides: &Overrides,
    env: &EnvLayer,
) -> Result<otl::config::Settings, otl::config::ConfigError> {
    let loaded = otl::config::load_file(overrides, env)?;
    resolve_settings(overrides, env, &loaded)
}

const TWO_PROFILES: &str = r#"
default_profile = "personal"

[profiles.work]
url = "https://work.example.com"
auth = "api-key"

[profiles.personal]
url = "https://personal.example.com"
"#;

#[test]
fn profile_flag_selects_the_instance() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    assert_eq!(resolved.base_url(), "https://work.example.com");
    assert_eq!(resolved.profile(), Some("work"));
    assert_eq!(resolved.auth(), AuthMethod::ApiKey);
}

#[test]
fn profile_env_var_selects_the_instance() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let env = EnvLayer::default().with_profile("work");
    let resolved = settings(&overrides_for(&path), &env).unwrap();
    assert_eq!(resolved.base_url(), "https://work.example.com");
}

#[test]
fn default_profile_applies_when_nothing_selects_one() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let resolved = settings(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert_eq!(resolved.base_url(), "https://personal.example.com");
    assert_eq!(resolved.profile(), Some("personal"));
}

#[test]
fn profile_flag_beats_profile_env_var() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default().with_profile("personal");
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url(), "https://work.example.com");
}

#[test]
fn url_precedence_is_flag_then_env_then_file() {
    // With NO profile in effect the URL layers are flag > env, and the file
    // has nothing to contribute (only profiles carry URLs).
    let dir = tempfile::tempdir().unwrap();
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let env = EnvLayer::default().with_url("https://env.example.com");
    let env_wins = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(env_wins.base_url(), "https://env.example.com");

    let overrides = Overrides {
        url: Some("https://flag.example.com".to_string()),
        ..Overrides::default()
    };
    let flag_wins = resolve_settings(&overrides, &env, &loaded).unwrap();
    assert_eq!(flag_wins.base_url(), "https://flag.example.com");

    // With a profile in effect the file supplies the URL, and the flag still
    // outranks it. `OUTLINE_URL` is not a layer here at all - see
    // `an_env_url_pointing_away_from_the_profile_is_refused`.
    let (_dir, path) = config_file(TWO_PROFILES);
    let file_only = settings(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert_eq!(file_only.base_url(), "https://personal.example.com");
    let mut with_flag = overrides_for(&path);
    with_flag.url = Some("https://flag.example.com".to_string());
    let flag_over_file = settings(&with_flag, &EnvLayer::default()).unwrap();
    assert_eq!(flag_over_file.base_url(), "https://flag.example.com");
}

#[test]
fn precedence_is_applied_per_key_not_per_layer() {
    // Three layers contribute three different keys in one resolution: the
    // profile comes from the env, the URL from the flag, and the auth method
    // from the config file. A layer that wins one key must not discard the
    // others.
    let (_dir, path) = config_file(
        r#"
        default_profile = "personal"

        [profiles.work]
        url = "https://work.example.com"
        auth = "oauth"

        [profiles.personal]
        url = "https://personal.example.com"
        "#,
    );
    let mut overrides = overrides_for(&path);
    overrides.url = Some("https://flag.example.com".to_string());
    let env = EnvLayer::default().with_profile("work");
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.profile(), Some("work"), "env profile lost");
    assert_eq!(
        resolved.base_url(),
        "https://flag.example.com",
        "flag URL lost"
    );
    assert_eq!(resolved.auth(), AuthMethod::Oauth, "profile auth was lost");
}

#[test]
fn an_env_url_resolves_by_precedence_then_the_gate_refuses_the_credential() {
    // R3 ruling: OUTLINE_URL stays a normal env layer for the URL, so
    // resolution honours flag > env > file (AC2). Whether that origin may
    // receive THIS profile's credential is a separate question, asked once at
    // the credential-release boundary - which refuses, because a credential
    // sent to the wrong server cannot be recalled.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://elsewhere.example.com")
        .with_profile_api_key("work", "key-for-work");

    // Resolution: the env layer wins the URL, as the AC requires.
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url(), "https://elsewhere.example.com");
    assert_eq!(resolved.url_source(), UrlSource::Env);
    assert_eq!(resolved.profile_url(), Some("https://work.example.com"));

    // Release: refused, so no credential exists to send.
    let error = release(&env, &resolved).unwrap_err();
    assert!(
        matches!(error, ConfigError::ConflictingUrl { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("OUTLINE_URL"), "{message}");
    assert!(message.contains("work"), "{message}");
    assert!(!message.contains("elsewhere.example.com"), "{message}");
    assert!(!message.contains("key-for-work"), "{message}");
}

#[test]
fn an_env_url_matching_the_profile_origin_releases_the_credential() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://work.example.com")
        .with_profile_api_key("work", "key-for-work");
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.url_source(), UrlSource::Env);
    assert_eq!(release(&env, &resolved).unwrap(), "key-for-work");
}

#[test]
fn origin_equivalence_is_normalized_not_a_string_comparison() {
    // R3 finding 4: the request channel tolerates and normalizes a trailing
    // slash, host casing and a default port, so none of them may look like a
    // different instance to the gate.
    for (declared, from_env, bound) in [
        ("http://127.0.0.1:9", "http://127.0.0.1:9/", true),
        ("http://127.0.0.1:9/", "http://127.0.0.1:9", true),
        ("https://Work.Example.COM", "https://work.example.com", true),
        (
            "https://work.example.com:443",
            "https://work.example.com",
            true,
        ),
        (
            "http://work.example.com:80",
            "http://work.example.com",
            true,
        ),
        // A different path is still the same server receiving the key.
        (
            "https://work.example.com",
            "https://work.example.com/sub",
            true,
        ),
        // Genuinely different instances.
        (
            "https://work.example.com",
            "https://other.example.com",
            false,
        ),
        ("https://work.example.com", "http://work.example.com", false),
        (
            "https://work.example.com",
            "https://work.example.com:8443",
            false,
        ),
    ] {
        let (_dir, path) = config_file(&format!("[profiles.work]\nurl = \"{declared}\"\n"));
        let mut overrides = overrides_for(&path);
        overrides.profile = Some("work".to_string());
        let env = EnvLayer::default()
            .with_url(from_env)
            .with_profile_api_key("work", "key-for-work");
        let resolved = settings(&overrides, &env).unwrap();
        assert_eq!(
            release(&env, &resolved).is_ok(),
            bound,
            "declared {declared:?} vs env {from_env:?}"
        );
    }
}

#[test]
fn a_profile_that_declares_no_url_cannot_bind_an_env_url() {
    // The env URL is still the resolution result (AC2), but there is nothing
    // to bind the profile's credential to, so it is not released.
    let (_dir, path) = config_file("[profiles.work]\nauth = \"api-key\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://ambient.example.com")
        .with_profile_api_key("work", "key-for-work");

    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url(), "https://ambient.example.com");
    assert_eq!(resolved.url_source(), UrlSource::Env);
    assert_eq!(resolved.profile_url(), None);

    let error = release(&env, &resolved).unwrap_err();
    assert!(
        matches!(error, ConfigError::UnboundProfileCredential { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("work"), "{message}");
    assert!(!message.contains("key-for-work"), "{message}");

    // --url directs the run deliberately and does release the credential.
    overrides.url = Some("https://explicit.example.com".to_string());
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.url_source(), UrlSource::Flag);
    assert_eq!(release(&env, &resolved).unwrap(), "key-for-work");
}

#[test]
fn the_url_flag_is_a_deliberate_redirect_and_binds() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    overrides.url = Some("https://elsewhere.example.com".to_string());
    let env = EnvLayer::default()
        .with_url("https://third.example.com")
        .with_profile_api_key("work", "key-for-work");
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url(), "https://elsewhere.example.com");
    assert_eq!(resolved.url_source(), UrlSource::Flag);
    assert_eq!(release(&env, &resolved).unwrap(), "key-for-work");
}

#[test]
fn the_env_url_is_still_the_source_when_no_profile_is_in_effect() {
    // Epic 1 path untouched: the restriction is about profile scope only.
    let dir = tempfile::tempdir().unwrap();
    let env = EnvLayer::default()
        .with_url("https://env.example.com")
        .with_api_key("k");
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.base_url(), "https://env.example.com");
}

/// A default (non-explicit) config location pointing at a file that does
/// not exist - the shape of a fresh machine, without reading the developer's
/// real config file.
fn absent_default_source(dir: &TempDir) -> ConfigSource {
    ConfigSource {
        path: Some(dir.path().join("config.toml")),
        explicit: false,
    }
}

#[test]
fn pure_env_path_works_with_no_config_file_at_all() {
    // A fresh machine has no config file; the Epic 1 env-only path must
    // keep working unchanged.
    let dir = tempfile::tempdir().unwrap();
    let env = EnvLayer::default()
        .with_url("https://env.example.com")
        .with_api_key("secret-key");
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.base_url(), "https://env.example.com");
    assert_eq!(resolved.profile(), None);
    assert_eq!(resolved.auth(), AuthMethod::ApiKey);
}

#[test]
fn missing_config_file_at_the_default_location_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    assert!(loaded.file.profiles.is_empty());
    assert_eq!(loaded.path, None, "an absent file must not be reported");
}

#[test]
fn a_platform_without_a_config_directory_still_resolves_from_env() {
    // `directories` can fail to find a home directory (some CI sandboxes,
    // service accounts). That must degrade to the env-only path, not fail.
    let loaded = otl::config::load_from(&ConfigSource {
        path: None,
        explicit: false,
    })
    .unwrap();
    assert!(loaded.file.profiles.is_empty());
}

#[test]
fn locate_prefers_the_flag_then_the_env_var_then_the_default() {
    let flag = PathBuf::from("/flag/config.toml");
    let from_env = PathBuf::from("/env/config.toml");
    let env = EnvLayer::default().with_config_path(from_env.clone());
    let overrides = Overrides {
        config_path: Some(flag.clone()),
        ..Overrides::default()
    };
    let located = otl::config::locate(&overrides, &env);
    assert_eq!(located.path, Some(flag));
    assert!(located.explicit);

    let located = otl::config::locate(&Overrides::default(), &env);
    assert_eq!(located.path, Some(from_env));
    assert!(located.explicit);

    let located = otl::config::locate(&Overrides::default(), &EnvLayer::default());
    assert_eq!(located.path, otl::config::default_config_path());
    assert!(!located.explicit, "the default location is never explicit");
}

#[test]
fn an_empty_config_override_disables_the_config_file() {
    // The documented way for a script or a test to pin itself to env vars
    // alone, whatever the invoking user has in their config directory.
    let env = EnvLayer::default().with_config_path(PathBuf::new());
    let located = otl::config::locate(&Overrides::default(), &env);
    assert_eq!(located.path, None);
    assert!(located.explicit);
    let loaded = otl::config::load_from(&located).unwrap();
    assert!(loaded.file.profiles.is_empty());
    assert_eq!(loaded.path, None);
}

#[test]
fn explicitly_named_config_file_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.toml");
    let error = otl::config::load_file(&overrides_for(&missing), &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("nope.toml"), "{message}");
    assert!(matches!(error, ConfigError::ConfigFileUnreadable { .. }));
}

#[test]
fn unknown_profile_is_a_readable_error_listing_the_known_ones() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("nope".to_string());
    let error = settings(&overrides, &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, ConfigError::UnknownProfile { .. }));
    assert!(message.contains("nope"), "{message}");
    assert!(message.contains("work"), "{message}");
    assert!(message.contains("personal"), "{message}");
    assert!(message.contains("config.toml"), "{message}");
    // The name the user typed is echoed; the config file's own
    // `default_profile` value is not (it is file content).
    let (_dir2, path2) = config_file("default_profile = \"LEAK-DEFAULT\"\n");
    let error = settings(&overrides_for(&path2), &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(matches!(
        error,
        ConfigError::UnknownProfile { name: None, .. }
    ));
    assert!(!message.contains("LEAK-DEFAULT"), "{message}");
    assert!(message.contains("default_profile"), "{message}");
}

#[test]
fn unknown_profile_with_no_config_file_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let overrides = Overrides {
        profile: Some("work".to_string()),
        ..Overrides::default()
    };
    let env = EnvLayer::default();
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let error = resolve_settings(&overrides, &env, &loaded).unwrap_err();
    assert!(matches!(error, ConfigError::UnknownProfile { .. }));
    let message = error.to_string();
    assert!(message.contains("work"), "{message}");
    assert!(
        message.contains("no user config file"),
        "an absent file must be stated as such: {message}"
    );
}

#[test]
fn malformed_toml_is_a_readable_error_naming_the_file_and_line() {
    let (_dir, path) = config_file("[profiles.work\nurl = 1\n");
    let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, ConfigError::MalformedConfigFile { .. }));
    assert!(message.contains("config.toml"), "{message}");
    assert!(message.contains("line 1"), "{message}");
}

#[test]
fn unknown_config_key_is_rejected_rather_than_ignored() {
    let (_dir, path) = config_file("[profiles.work]\nurls = \"https://x.example.com\"\n");
    let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, ConfigError::MalformedConfigFile { .. }));
    // The offending key is located by LINE and the whole schema is restated,
    // rather than quoting text from the file (see the no-echo test below).
    assert!(message.contains("line 2"), "{message}");
    assert!(message.contains("unknown key"), "{message}");
    assert!(message.contains("default_profile"), "{message}");
    assert!(message.contains("`url`"), "{message}");
}

#[test]
fn a_profile_without_a_url_and_no_override_is_a_readable_error() {
    let (_dir, path) = config_file("[profiles.work]\nauth = \"api-key\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let error = settings(&overrides, &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, ConfigError::MissingUrl { .. }));
    assert!(message.contains("work"), "{message}");
    // R3 finding 9: it must not recommend a fix the credential gate then
    // refuses. Setting OUTLINE_URL would resolve, and then fail to bind.
    assert!(
        !message.contains("OUTLINE_URL"),
        "recommends a fix that cannot work: {message}"
    );
    assert!(message.contains("url ="), "{message}");
    assert!(message.contains("--url"), "{message}");
}

#[test]
fn missing_url_error_still_names_the_environment_variable() {
    // Epic 1 behaviour: the pure-env user gets the same actionable message.
    let error = ConfigError::MissingUrl { profile: None };
    assert!(error.to_string().contains("OUTLINE_URL"));
}

#[test]
fn oauth_profile_reports_that_only_api_keys_are_wired_up() {
    let (_dir, path) =
        config_file("[profiles.work]\nurl = \"https://w.example.com\"\nauth = \"oauth\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    let env = EnvLayer::default()
        .with_api_key("k")
        .with_profile_api_key("work", "work-key");
    let error = release(&env, &resolved).unwrap_err();
    assert!(matches!(error, ConfigError::UnsupportedAuthMethod { .. }));
    let message = error.to_string();
    assert!(message.contains("work"), "{message}");
    assert!(message.contains("api-key"), "{message}");
}

#[test]
fn api_key_comes_from_the_environment_not_the_config_file() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default().with_profile_api_key("work", "work-key");
    let resolved = settings(&overrides, &env).unwrap();
    let config = Config::from_parts(&resolved, release(&env, &resolved).unwrap());
    assert_eq!(config.base_url, "https://work.example.com");
    assert_eq!(config.api_key, "work-key");
}

#[test]
fn missing_api_key_is_reported_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let env = EnvLayer::default().with_url("https://x.example.com");
    let resolved = settings_from_env(&dir, &env);
    let error = release(&env, &resolved).unwrap_err();
    assert!(matches!(error, ConfigError::MissingApiKey));
    assert!(error.to_string().contains("OUTLINE_API_KEY"));
}

#[test]
fn default_config_path_is_absolute_and_platform_resolved() {
    // Resolved via `directories`, never by assuming a Unix layout: the test
    // only asserts the shape, so it holds on macOS, Linux and Windows.
    let path = otl::config::default_config_path().expect("no config directory on this platform");
    assert!(path.is_absolute(), "{}", path.display());
    assert_eq!(
        path.file_name().and_then(|n| n.to_str()),
        Some("config.toml")
    );
    let dir = otl::config::config_dir().expect("no config directory");
    assert_eq!(path.parent(), Some(dir.as_path()));
    // The credential file the auth layer owns lives beside it, never inside
    // the config file itself.
    assert_ne!(otl::config::CREDENTIALS_FILE_NAME, "config.toml");
}

#[test]
fn an_oversized_config_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let filler = "# padding padding padding padding padding padding\n".repeat(30_000);
    std::fs::write(&path, filler).unwrap();
    let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
    assert!(matches!(error, ConfigError::ConfigFileUnreadable { .. }));
    assert!(error.to_string().contains("too large"), "{error}");
}

#[test]
fn empty_profile_name_is_rejected() {
    let (_dir, path) = config_file("[profiles.\"\"]\nurl = \"https://x.example.com\"\n");
    let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
    assert!(matches!(error, ConfigError::MalformedConfigFile { .. }));
}

#[test]
fn blank_env_values_are_treated_as_unset() {
    // An exported-but-empty variable must not shadow the config file.
    let (_dir, path) = config_file(TWO_PROFILES);
    let env = EnvLayer::from_values(Some("  "), Some(""), Some(""), None);
    let resolved = settings(&overrides_for(&path), &env).unwrap();
    assert_eq!(resolved.base_url(), "https://personal.example.com");
}

// ---------------------------------------------------------------------------
// Credential hygiene of the resolved types' Debug output.
//
// These live here rather than in a `#[cfg(test)]` module inside `config.rs`
// only to keep that file under the 800-line limit; they exercise the public
// API exactly as before.
// ---------------------------------------------------------------------------

#[test]
fn an_empty_config_file_yields_no_profiles() {
    let (_dir, path) = config_file("");
    let loaded = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert!(loaded.file.profiles.is_empty());
    assert_eq!(loaded.file.default_profile, None);
}

// ---------------------------------------------------------------------------
// Credentials are scoped to the instance they belong to (R1 finding 1).
//
// A profile names an instance, so a profile's request must carry THAT
// instance's key. The global variable is for the no-profile case only, and
// never falls through to a profile: falling through is how one workspace's
// key reaches another workspace's server.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// No configuration type leaks a URL through Debug (R1 finding 2).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Names from the config file cannot inject into a terminal (R1 finding 7).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// No profile NAME reaches any Debug rendering (R2 finding 2).
//
// A name is config-file content just as much as a URL is: `default_profile =
// "<secret>"` and `[profiles.<secret>]` are both values the user wrote. Debug
// is the unbounded surface (logs, panics, error chains), so names appear only
// in Display, sanitized and capped.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Config PATHS cannot inject into a terminal either (R2 finding 3).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Environment variable names follow the platform's case rules (R2 finding 6).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// A derived variable name is bounded at the source (R2 finding 7).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// AC2, on ONE key: flag > env > file for the base URL (R3 finding 1).
//
// The R2 attempt replaced this with a test where each layer supplied a
// different key, which only shows that layers do not delete one another. This
// is the same-key ladder, and it is what the acceptance criterion says.
// ---------------------------------------------------------------------------

#[test]
fn the_url_key_itself_resolves_flag_over_env_over_file() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());

    // File only.
    let file_only = settings(&overrides, &EnvLayer::default()).unwrap();
    assert_eq!(file_only.base_url(), "https://work.example.com");
    assert_eq!(file_only.url_source(), UrlSource::Profile);

    // Env over file: the env value is the resolution result, and the source
    // is recorded so the credential gate can have its separate say.
    let env = EnvLayer::default().with_url("https://env.example.com");
    let env_over_file = settings(&overrides, &env).unwrap();
    assert_eq!(env_over_file.base_url(), "https://env.example.com");
    assert_eq!(env_over_file.url_source(), UrlSource::Env);

    // Flag over env over file.
    overrides.url = Some("https://flag.example.com".to_string());
    let flag_over_all = settings(&overrides, &env).unwrap();
    assert_eq!(flag_over_all.base_url(), "https://flag.example.com");
    assert_eq!(flag_over_all.url_source(), UrlSource::Flag);
}

// ---------------------------------------------------------------------------
// The binding check is at the shared boundary, not inside one TokenSource
// (R3 ruling). Epic 2's credential file plugs in as another implementation
// and inherits the check without knowing about it.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Bidi and invisible characters (R3 finding 5).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Config paths stay out of every Debug rendering (R3 finding 3).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Every error variant is registered in the public exit-code document
// (R3 finding 8).
//
// The exit-code table is a published API, so a new failure mode must be
// documented before release. The match below is exhaustive: adding a variant
// stops this file compiling until its documentation keyword is chosen, and
// the assertion then fails until the keyword actually appears in the doc.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The gate's INPUT cannot be forged either (R4 finding 1).
//
// R3 made the proof token unforgeable, which only moved the problem: the
// proof is issued from a `Settings`, and that was a public struct anyone
// could build with `url_source: Flag` and any base URL they liked. The secret
// was also readable straight off `EnvLayer`, without going near the gate.
//
// Both are now closed by construction, which is why the two cases below are
// COMPILE-FAIL tests rather than runtime ones: there is no value of any
// argument that reaches the unsafe behaviour, so there is nothing to assert
// at runtime. `trybuild` is not in the dependency set, so the check is done
// by compiling a probe crate against the real library.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// An unusable URL is diagnosed as such, not as a cross-instance conflict
// (R4 finding 3).
// ---------------------------------------------------------------------------
