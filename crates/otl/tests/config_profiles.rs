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
    resolve_settings, AuthMethod, Config, ConfigError, ConfigSource, EnvLayer, Overrides,
    TokenSource,
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
    assert_eq!(resolved.base_url, "https://work.example.com");
    assert_eq!(resolved.profile.as_deref(), Some("work"));
    assert_eq!(resolved.auth, AuthMethod::ApiKey);
}

#[test]
fn profile_env_var_selects_the_instance() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let env = EnvLayer {
        profile: Some("work".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides_for(&path), &env).unwrap();
    assert_eq!(resolved.base_url, "https://work.example.com");
}

#[test]
fn default_profile_applies_when_nothing_selects_one() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let resolved = settings(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert_eq!(resolved.base_url, "https://personal.example.com");
    assert_eq!(resolved.profile.as_deref(), Some("personal"));
}

#[test]
fn profile_flag_beats_profile_env_var() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer {
        profile: Some("personal".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url, "https://work.example.com");
}

#[test]
fn url_precedence_is_flag_then_env_then_file() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let file_only = settings(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert_eq!(file_only.base_url, "https://personal.example.com");

    let env = EnvLayer {
        url: Some("https://env.example.com".to_string()),
        ..EnvLayer::default()
    };
    let env_wins = settings(&overrides_for(&path), &env).unwrap();
    assert_eq!(env_wins.base_url, "https://env.example.com");

    let mut overrides = overrides_for(&path);
    overrides.url = Some("https://flag.example.com".to_string());
    let flag_wins = settings(&overrides, &env).unwrap();
    assert_eq!(flag_wins.base_url, "https://flag.example.com");
}

#[test]
fn precedence_is_applied_per_key_not_per_layer() {
    // The env supplies only the URL; the profile's auth method must survive
    // instead of the whole config-file layer being discarded.
    let (_dir, path) = config_file(
        r#"
        [profiles.work]
        url = "https://work.example.com"
        auth = "oauth"
        "#,
    );
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer {
        url: Some("https://env.example.com".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url, "https://env.example.com");
    assert_eq!(resolved.auth, AuthMethod::Oauth, "profile auth was lost");
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
    let env = EnvLayer {
        url: Some("https://env.example.com".to_string()),
        api_key: Some("secret-key".to_string()),
        config_path: None,
        profile: None,
    };
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.base_url, "https://env.example.com");
    assert_eq!(resolved.profile, None);
    assert_eq!(resolved.auth, AuthMethod::ApiKey);
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
    let env = EnvLayer {
        config_path: Some(from_env.clone()),
        ..EnvLayer::default()
    };
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
    let env = EnvLayer {
        config_path: Some(PathBuf::new()),
        ..EnvLayer::default()
    };
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
    assert!(message.contains("urls"), "typo not named: {message}");
}

#[test]
fn a_credential_in_the_config_file_is_refused_and_never_echoed() {
    for body in [
        "api_key = \"leaked-secret-value\"\n",
        "[profiles.work]\napi_key = \"leaked-secret-value\"\n",
        "[profiles.work]\ntoken = \"leaked-secret-value\"\n",
    ] {
        let (_dir, path) = config_file(body);
        let error =
            otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
        let message = error.to_string();
        assert!(
            matches!(error, ConfigError::CredentialInConfigFile { .. }),
            "{body:?} -> {error:?}"
        );
        assert!(
            !message.contains("leaked-secret-value"),
            "secret echoed: {message}"
        );
        assert!(!format!("{error:?}").contains("leaked-secret-value"));
        assert!(message.contains("credentials.toml"), "{message}");
    }
}

#[test]
fn a_syntax_error_next_to_a_secret_does_not_echo_the_secret() {
    // An unterminated string is reported by location and kind only: the
    // config file is the wrong place for a secret, but a user who put one
    // there must not see it again in an error message, its Debug, or logs.
    let (_dir, path) = config_file("[profiles.work]\napi_key = \"unterminated-secret\n");
    let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(
            !rendered.contains("unterminated-secret"),
            "secret echoed: {rendered}"
        );
    }
}

#[test]
fn a_profile_without_a_url_and_no_override_is_a_readable_error() {
    let (_dir, path) = config_file("[profiles.work]\nauth = \"api-key\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let error = settings(&overrides, &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("work"), "{message}");
    assert!(message.contains("OUTLINE_URL"), "{message}");
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
    let env = EnvLayer {
        api_key: Some("k".to_string()),
        ..EnvLayer::default()
    };
    let error = otl::config::EnvApiKey(&env).token(&resolved).unwrap_err();
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
    let env = EnvLayer {
        api_key: Some("env-key".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &env).unwrap();
    let config = Config::from_parts(
        &resolved,
        otl::config::EnvApiKey(&env).token(&resolved).unwrap(),
    );
    assert_eq!(config.base_url, "https://work.example.com");
    assert_eq!(config.api_key, "env-key");
}

#[test]
fn missing_api_key_is_reported_before_any_request() {
    let resolved = otl::config::Settings {
        profile: None,
        base_url: "https://x.example.com".to_string(),
        auth: AuthMethod::ApiKey,
    };
    let error = otl::config::EnvApiKey(&EnvLayer::default())
        .token(&resolved)
        .unwrap_err();
    assert!(matches!(error, ConfigError::MissingApiKey));
    assert!(error.to_string().contains("OUTLINE_API_KEY"));
}

#[test]
fn env_layer_debug_redacts_the_api_key() {
    let env = EnvLayer {
        api_key: Some("super-secret-key".to_string()),
        url: Some("https://alice:pw-secret@example.com".to_string()),
        profile: None,
        config_path: None,
    };
    let rendered = format!("{env:?}");
    assert!(!rendered.contains("super-secret-key"), "{rendered}");
    assert!(!rendered.contains("pw-secret"), "{rendered}");
}

#[test]
fn settings_debug_redacts_the_base_url() {
    let resolved = otl::config::Settings {
        profile: Some("work".to_string()),
        base_url: "https://alice:pw-secret@example.com/PATH-SECRET".to_string(),
        auth: AuthMethod::ApiKey,
    };
    let rendered = format!("{resolved:?}");
    assert!(!rendered.contains("pw-secret"), "{rendered}");
    assert!(!rendered.contains("PATH-SECRET"), "{rendered}");
    assert!(rendered.contains("work"));
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
    assert_eq!(resolved.base_url, "https://personal.example.com");
}

// ---------------------------------------------------------------------------
// Credential hygiene of the resolved types' Debug output.
//
// These live here rather than in a `#[cfg(test)]` module inside `config.rs`
// only to keep that file under the 800-line limit; they exercise the public
// API exactly as before.
// ---------------------------------------------------------------------------

#[test]
fn debug_output_redacts_api_key() {
    let config = Config {
        base_url: "https://docs.example.com".to_string(),
        api_key: "super-secret-key".to_string(),
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("super-secret-key"),
        "api key leaked: {rendered}"
    );
    assert!(rendered.contains("***"));
    assert!(rendered.contains("https://docs.example.com"));
}

#[test]
fn debug_output_redacts_base_url_with_userinfo() {
    // Config holds the raw configured value before Client::new validation,
    // so a base URL may still embed credentials here.
    let config = Config {
        base_url: "http://alice:url-secret-pw@example.com".to_string(),
        api_key: "k".to_string(),
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("url-secret-pw"),
        "base_url credential leaked: {rendered}"
    );
    assert!(!rendered.contains("alice"), "username leaked: {rendered}");
}

#[test]
fn debug_output_redacts_base_url_with_query_secret() {
    // Credentials can hide outside userinfo too; anything that would not
    // pass Client::new shape checks is redacted whole.
    let config = Config {
        base_url: "https://example.com/?access_token=query-secret".to_string(),
        api_key: "query-secret".to_string(),
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("query-secret"),
        "query credential leaked: {rendered}"
    );
}

#[test]
fn debug_output_shows_clean_base_url() {
    let config = Config {
        base_url: "https://docs.example.com".to_string(),
        api_key: "k".to_string(),
    };
    let rendered = format!("{config:?}");
    assert!(rendered.contains("https://docs.example.com"));
}

#[test]
fn debug_output_hides_base_url_path() {
    // A path can carry secrets too (token-in-path auth schemes); Debug
    // shows the origin only.
    let config = Config {
        base_url: "https://example.com/PATH-SECRET-9c7a".to_string(),
        api_key: "PATH-SECRET-9c7a".to_string(),
    };
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains("PATH-SECRET-9c7a"),
        "path secret leaked: {rendered}"
    );
    assert!(rendered.contains("https://example.com"));
}

#[test]
fn an_empty_config_file_yields_no_profiles() {
    let (_dir, path) = config_file("");
    let loaded = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert!(loaded.file.profiles.is_empty());
    assert_eq!(loaded.file.default_profile, None);
}
