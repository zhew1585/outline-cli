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
    // With NO profile in effect the URL layers are flag > env, and the file
    // has nothing to contribute (only profiles carry URLs).
    let dir = tempfile::tempdir().unwrap();
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let env = EnvLayer {
        url: Some("https://env.example.com".to_string()),
        ..EnvLayer::default()
    };
    let env_wins = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(env_wins.base_url, "https://env.example.com");

    let overrides = Overrides {
        url: Some("https://flag.example.com".to_string()),
        ..Overrides::default()
    };
    let flag_wins = resolve_settings(&overrides, &env, &loaded).unwrap();
    assert_eq!(flag_wins.base_url, "https://flag.example.com");

    // With a profile in effect the file supplies the URL, and the flag still
    // outranks it. `OUTLINE_URL` is not a layer here at all - see
    // `an_env_url_pointing_away_from_the_profile_is_refused`.
    let (_dir, path) = config_file(TWO_PROFILES);
    let file_only = settings(&overrides_for(&path), &EnvLayer::default()).unwrap();
    assert_eq!(file_only.base_url, "https://personal.example.com");
    let mut with_flag = overrides_for(&path);
    with_flag.url = Some("https://flag.example.com".to_string());
    let flag_over_file = settings(&with_flag, &EnvLayer::default()).unwrap();
    assert_eq!(flag_over_file.base_url, "https://flag.example.com");
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
    let env = EnvLayer {
        profile: Some("work".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(
        resolved.profile.as_deref(),
        Some("work"),
        "env profile lost"
    );
    assert_eq!(
        resolved.base_url, "https://flag.example.com",
        "flag URL lost"
    );
    assert_eq!(resolved.auth, AuthMethod::Oauth, "profile auth was lost");
}

#[test]
fn an_env_url_pointing_away_from_the_profile_is_refused() {
    // R2 finding 1: a warning cannot recall a credential that has already
    // been sent, which is the same argument that makes a profile refuse the
    // global API key. So the conflict fails the command instead.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer {
        url: Some("https://elsewhere.example.com".to_string()),
        ..EnvLayer::default()
    };
    let error = settings(&overrides, &env).unwrap_err();
    assert!(
        matches!(error, ConfigError::ConflictingUrl { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("OUTLINE_URL"), "{message}");
    assert!(message.contains("work"), "{message}");
    // Neither URL is printed: a base URL can carry credentials.
    assert!(!message.contains("elsewhere.example.com"), "{message}");
    assert!(!message.contains("work.example.com"), "{message}");

    // An env URL that agrees with the profile is not a conflict.
    let same = EnvLayer {
        url: Some("https://work.example.com".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &same).unwrap();
    assert_eq!(resolved.base_url, "https://work.example.com");

    // `--url` is the deliberate redirect and is exempt.
    let mut redirected = overrides.clone();
    redirected.url = Some("https://elsewhere.example.com".to_string());
    let resolved = settings(&redirected, &env).unwrap();
    assert_eq!(resolved.base_url, "https://elsewhere.example.com");
}

#[test]
fn an_env_url_cannot_supply_the_origin_for_a_profile_that_declares_none() {
    // Same rule from the other side: a profile scopes the credential, so an
    // ambient OUTLINE_URL must not be what decides where it goes. Without a
    // profile `url` and without --url the command fails.
    let (_dir, path) = config_file("[profiles.work]\nauth = \"api-key\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer {
        url: Some("https://ambient.example.com".to_string()),
        ..EnvLayer::default()
    };
    let error = settings(&overrides, &env).unwrap_err();
    assert!(matches!(error, ConfigError::MissingUrl { .. }), "{error:?}");

    // --url still works.
    overrides.url = Some("https://explicit.example.com".to_string());
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url, "https://explicit.example.com");
}

#[test]
fn the_env_url_is_still_the_source_when_no_profile_is_in_effect() {
    // Epic 1 path untouched: the restriction is about profile scope only.
    let dir = tempfile::tempdir().unwrap();
    let env = EnvLayer {
        url: Some("https://env.example.com".to_string()),
        api_key: Some("k".to_string()),
        ..EnvLayer::default()
    };
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.base_url, "https://env.example.com");
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
        ..EnvLayer::default()
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

/// Every way a value in the config file can make parsing fail, each with the
/// same recognizable secret as that value.
///
/// `toml`'s own message text interpolates the offending value for several of
/// these (unknown enum variant, string-vs-map type mismatch, unknown bare
/// key), which is why no parser-produced text may reach the output.
const VALUE_LEAK_CASES: &[&str] = &[
    "[profiles.work]\nauth = \"LEAK-SECRET-VALUE\"\n",
    "[profiles.work]\nauth = \"\"\"LEAK-SECRET-VALUE\"\"\"\n",
    "profiles = \"LEAK-SECRET-VALUE\"\n",
    "default_profile = [\"LEAK-SECRET-VALUE\"]\n",
    "LEAK-SECRET-VALUE = 1\n",
    "[LEAK-SECRET-VALUE]\nurl = \"https://x.example.com\"\n",
    "[profiles.work]\nurl = LEAK-SECRET-VALUE\n",
    "[profiles.work]\nurl = \"LEAK-SECRET-VALUE\n",
    "[profiles.work]\nurl = { inner = \"LEAK-SECRET-VALUE\" }\nauth = 1\n",
    "[profiles.work]\nurl = \"a\"\nurl = \"LEAK-SECRET-VALUE\"\n",
    "[profiles.work]\nextra = \"LEAK-SECRET-VALUE\"\n",
    "[profiles.work.nested]\nkey = \"LEAK-SECRET-VALUE\"\n",
    "[profiles.work]\nurl = \"a\\qLEAK-SECRET-VALUE\"\n",
];

#[test]
fn no_config_file_value_is_ever_echoed_into_a_diagnostic() {
    for body in VALUE_LEAK_CASES {
        let (_dir, path) = config_file(body);
        let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default())
            .expect_err(&format!("expected a failure for {body:?}"));
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(
                !rendered.contains("LEAK-SECRET-VALUE"),
                "value echoed for {body:?}: {rendered}"
            );
        }
        // Still actionable: the failure is located.
        assert!(
            error.to_string().contains("line "),
            "no location for {body:?}: {error}"
        );
    }
}

#[test]
fn a_parse_diagnostic_never_carries_control_characters() {
    // A quoted key can hold ESC; nothing derived from the file may reach
    // stderr raw.
    let (_dir, path) = config_file("[profiles.\"a\\u001b[31mb\"]\nbad = 1\n");
    let error = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap_err();
    let message = error.to_string();
    assert!(!message.contains('\u{1b}'), "ESC in message: {message:?}");
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
    }
    .with_profile_api_key("work", "work-key");
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
    let env = EnvLayer::default().with_profile_api_key("work", "work-key");
    let resolved = settings(&overrides, &env).unwrap();
    let config = Config::from_parts(
        &resolved,
        otl::config::EnvApiKey(&env).token(&resolved).unwrap(),
    );
    assert_eq!(config.base_url, "https://work.example.com");
    assert_eq!(config.api_key, "work-key");
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
        ..EnvLayer::default()
    }
    .with_profile_api_key("work", "profile-secret-key");
    let rendered = format!("{env:?}");
    assert!(!rendered.contains("super-secret-key"), "{rendered}");
    assert!(!rendered.contains("pw-secret"), "{rendered}");
    assert!(!rendered.contains("profile-secret-key"), "{rendered}");
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
    // The profile NAME is redacted too (R2 finding 2): a name is config-file
    // content, and Debug is an unbounded surface. Set-ness is still visible.
    assert!(!rendered.contains("work"), "{rendered}");
    assert!(rendered.contains("***"), "{rendered}");
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

// ---------------------------------------------------------------------------
// Credentials are scoped to the instance they belong to (R1 finding 1).
//
// A profile names an instance, so a profile's request must carry THAT
// instance's key. The global variable is for the no-profile case only, and
// never falls through to a profile: falling through is how one workspace's
// key reaches another workspace's server.
// ---------------------------------------------------------------------------

#[test]
fn a_profile_reads_its_own_api_key_variable() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let env = EnvLayer::default()
        .with_profile_api_key("work", "key-for-work")
        .with_profile_api_key("personal", "key-for-personal");

    for (profile, expected) in [("work", "key-for-work"), ("personal", "key-for-personal")] {
        let mut overrides = overrides_for(&path);
        overrides.profile = Some(profile.to_string());
        let resolved = settings(&overrides, &env).unwrap();
        assert_eq!(
            otl::config::EnvApiKey(&env).token(&resolved).unwrap(),
            expected,
            "{profile} got the wrong key"
        );
    }
}

#[test]
fn the_global_api_key_never_reaches_a_profile() {
    // The regression that matters: with only the global variable exported,
    // `--profile personal` must NOT send it to personal's instance.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("personal".to_string());
    let env = EnvLayer {
        api_key: Some("key-for-work".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &env).unwrap();
    let error = otl::config::EnvApiKey(&env).token(&resolved).unwrap_err();
    assert!(
        matches!(error, ConfigError::MissingProfileApiKey { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("OUTLINE_API_KEY_PERSONAL"), "{message}");
    // The message must not echo the key, and must explain the refusal.
    assert!(!message.contains("key-for-work"), "{message}");
    assert!(message.contains("deliberately not used"), "{message}");
}

#[test]
fn one_profiles_key_is_not_reachable_by_another_profile() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let env = EnvLayer::default().with_profile_api_key("work", "key-for-work");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("personal".to_string());
    let resolved = settings(&overrides, &env).unwrap();
    let error = otl::config::EnvApiKey(&env).token(&resolved).unwrap_err();
    assert!(matches!(error, ConfigError::MissingProfileApiKey { .. }));
    assert!(!error.to_string().contains("key-for-work"));
}

#[test]
fn the_global_api_key_still_serves_the_profile_less_path() {
    // Epic 1 behaviour, unchanged: no profile, global variable, no config.
    let dir = tempfile::tempdir().unwrap();
    let env = EnvLayer {
        url: Some("https://env.example.com".to_string()),
        api_key: Some("global-key".to_string()),
        ..EnvLayer::default()
    };
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.profile, None);
    assert_eq!(
        otl::config::EnvApiKey(&env).token(&resolved).unwrap(),
        "global-key"
    );
}

#[test]
fn profile_names_map_to_predictable_variable_names() {
    for (profile, expected) in [
        ("work", Some("OUTLINE_API_KEY_WORK")),
        ("Personal", Some("OUTLINE_API_KEY_PERSONAL")),
        ("self-hosted", Some("OUTLINE_API_KEY_SELF_HOSTED")),
        ("a.b", Some("OUTLINE_API_KEY_A_B")),
        ("x1", Some("OUTLINE_API_KEY_X1")),
        // No ASCII alphanumeric: cannot name a variable.
        ("工作", None),
        ("-", None),
        ("", None),
    ] {
        assert_eq!(
            otl::config::api_key_var(profile).as_deref(),
            expected,
            "profile {profile:?}"
        );
    }
}

#[test]
fn a_profile_with_no_expressible_variable_name_is_refused_not_defaulted() {
    let (_dir, path) = config_file("[profiles.\"工作\"]\nurl = \"https://x.example.com\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("工作".to_string());
    let env = EnvLayer {
        api_key: Some("global-key".to_string()),
        ..EnvLayer::default()
    };
    let resolved = settings(&overrides, &env).unwrap();
    let error = otl::config::EnvApiKey(&env).token(&resolved).unwrap_err();
    assert!(
        matches!(error, ConfigError::ProfileApiKeyVarUnnameable { .. }),
        "{error:?}"
    );
    assert!(!error.to_string().contains("global-key"));
}

#[test]
fn two_profiles_sharing_one_variable_name_are_refused() {
    // `my-work` and `my.work` both map to OUTLINE_API_KEY_MY_WORK, so the
    // key's instance would be ambiguous.
    let (_dir, path) = config_file(
        "[profiles.\"my-work\"]\nurl = \"https://a.example.com\"\n\
         [profiles.\"my.work\"]\nurl = \"https://b.example.com\"\n",
    );
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("my-work".to_string());
    let error = settings(&overrides, &EnvLayer::default()).unwrap_err();
    assert!(
        matches!(error, ConfigError::AmbiguousProfileApiKeyVar { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("OUTLINE_API_KEY_MY_WORK"), "{message}");

    // A profile that does NOT share a variable resolves normally, even
    // though the colliding pair is still in the same file.
    let (_dir2, path2) = config_file(
        "[profiles.\"my-work\"]\nurl = \"https://a.example.com\"\n\
         [profiles.\"my.work\"]\nurl = \"https://b.example.com\"\n\
         [profiles.solo]\nurl = \"https://c.example.com\"\n",
    );
    let mut solo = overrides_for(&path2);
    solo.profile = Some("solo".to_string());
    let resolved = settings(&solo, &EnvLayer::default()).unwrap();
    assert_eq!(resolved.base_url, "https://c.example.com");
}

#[test]
fn a_blank_profile_api_key_variable_counts_as_unset() {
    // `export OUTLINE_API_KEY_WORK=` must fail like an unset variable, not
    // send an empty bearer token.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default().with_profile_api_key("work", "   ");
    let resolved = settings(&overrides, &env).unwrap();
    let error = otl::config::EnvApiKey(&env).token(&resolved).unwrap_err();
    assert!(matches!(error, ConfigError::MissingProfileApiKey { .. }));
}

// ---------------------------------------------------------------------------
// No configuration type leaks a URL through Debug (R1 finding 2).
// ---------------------------------------------------------------------------

/// Every public configuration type that can hold a base URL, rendered with
/// Debug. A URL's userinfo, path, query and fragment can all carry
/// credentials, so none of them may appear.
#[test]
fn no_configuration_type_leaks_a_url_through_debug() {
    const SECRET_URL: &str = "https://alice:pw-secret@example.com/PATH-SECRET?q=QUERY-SECRET";
    let overrides = Overrides {
        profile: Some("work".to_string()),
        url: Some(SECRET_URL.to_string()),
        config_path: None,
    };
    let env = EnvLayer {
        url: Some(SECRET_URL.to_string()),
        api_key: Some("KEY-SECRET".to_string()),
        ..EnvLayer::default()
    };
    let (_dir, path) = config_file(&format!("[profiles.work]\nurl = \"{SECRET_URL}\"\n"));
    let loaded = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap();
    let profile = loaded.file.profiles.get("work").unwrap();
    let resolved = otl::config::Settings {
        profile: Some("work".to_string()),
        base_url: SECRET_URL.to_string(),
        auth: AuthMethod::ApiKey,
    };
    let config = Config {
        base_url: SECRET_URL.to_string(),
        api_key: "KEY-SECRET".to_string(),
    };

    let rendered = [
        format!("{overrides:?}"),
        format!("{env:?}"),
        format!("{profile:?}"),
        format!("{:?}", loaded.file),
        format!("{loaded:?}"),
        format!("{resolved:?}"),
        format!("{config:?}"),
    ];
    for (index, text) in rendered.iter().enumerate() {
        for secret in [
            "pw-secret",
            "alice",
            "PATH-SECRET",
            "QUERY-SECRET",
            "KEY-SECRET",
        ] {
            assert!(
                !text.contains(secret),
                "type #{index} leaked {secret}: {text}"
            );
        }
        // The origin is the one URL-derived form that is safe to show.
        assert!(
            !text.contains("://") || text.contains("https://example.com"),
            "type #{index} shows an unexpected URL form: {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// Names from the config file cannot inject into a terminal (R1 finding 7).
// ---------------------------------------------------------------------------

/// A profile name carrying an ANSI sequence and a forged diagnostic line.
const HOSTILE_NAME: &str = "\u{1b}[31mRED\u{1b}[0m\nerror: forged";

#[test]
fn control_characters_in_profile_names_never_reach_a_diagnostic() {
    let hostile_toml = "[profiles.\"\\u001b[31mRED\\u001b[0m\\nerror: forged\"]\nurl = \"https://x.example.com\"\n";
    let (_dir, path) = config_file(hostile_toml);

    // 1. Listed as an available profile after an unknown-profile failure.
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("nope".to_string());
    let listed = settings(&overrides, &EnvLayer::default())
        .unwrap_err()
        .to_string();

    // 2. Named as the selected-but-URL-less profile.
    let (_dir2, path2) = config_file(
        "[profiles.\"\\u001b[31mRED\\u001b[0m\\nerror: forged\"]\nauth = \"api-key\"\n",
    );
    let mut selected = overrides_for(&path2);
    selected.profile = Some(HOSTILE_NAME.to_string());
    let named = settings(&selected, &EnvLayer::default())
        .unwrap_err()
        .to_string();

    // 3. Named by a credential-in-config diagnostic.
    let (_dir3, path3) =
        config_file("[profiles.\"\\u001b[31mRED\\u001b[0m\\nerror: forged\"]\napi_key = \"x\"\n");
    let credential = otl::config::load_file(&overrides_for(&path3), &EnvLayer::default())
        .unwrap_err()
        .to_string();

    // 4. Named by the unsupported-auth diagnostic.
    let unsupported = ConfigError::UnsupportedAuthMethod {
        profile: Some(HOSTILE_NAME.to_string()),
        method: AuthMethod::Oauth,
    }
    .to_string();

    for (label, text) in [
        ("available list", listed),
        ("selected profile", named),
        ("credential location", credential),
        ("unsupported auth", unsupported),
    ] {
        assert!(!text.contains('\u{1b}'), "{label} carries ESC: {text:?}");
        // The name's own newline is gone, so its payload can never start a
        // line: a forged diagnostic must not be able to impersonate one.
        assert!(
            text.lines()
                .all(|line| !line.trim_start().starts_with("error:")),
            "{label} carries a forged diagnostic line: {text:?}"
        );
    }
}

#[test]
fn sanitize_name_replaces_control_characters_and_caps_length() {
    let sanitized = otl::config::sanitize_name("a\u{1b}[31mb\nc\t");
    assert!(!sanitized.contains('\u{1b}'), "{sanitized:?}");
    assert!(!sanitized.contains('\n'), "{sanitized:?}");
    assert!(!sanitized.contains('\t'), "{sanitized:?}");
    assert!(sanitized.starts_with('a'), "{sanitized:?}");

    let long = "x".repeat(500);
    let capped = otl::config::sanitize_name(&long);
    assert!(capped.chars().count() < 100, "not capped: {}", capped.len());
    assert!(capped.ends_with("..."), "{capped:?}");
}

#[test]
fn a_huge_profile_list_cannot_flood_stderr() {
    let mut body = String::new();
    for index in 0..200 {
        body.push_str(&format!(
            "[profiles.p{index}]\nurl = \"https://x.example.com\"\n"
        ));
    }
    let (_dir, path) = config_file(&body);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("nope".to_string());
    let message = settings(&overrides, &EnvLayer::default())
        .unwrap_err()
        .to_string();
    assert!(message.contains("more"), "list not capped: {message}");
    assert!(message.len() < 1_000, "message too long: {}", message.len());
}

// ---------------------------------------------------------------------------
// No profile NAME reaches any Debug rendering (R2 finding 2).
//
// A name is config-file content just as much as a URL is: `default_profile =
// "<secret>"` and `[profiles.<secret>]` are both values the user wrote. Debug
// is the unbounded surface (logs, panics, error chains), so names appear only
// in Display, sanitized and capped.
// ---------------------------------------------------------------------------

#[test]
fn no_configuration_type_leaks_a_profile_name_through_debug() {
    const SECRET_NAME: &str = "KEY-SECRET-NAME";
    let (_dir, path) = config_file(&format!(
        "default_profile = \"{SECRET_NAME}\"\n\
         [profiles.{SECRET_NAME}]\nurl = \"https://x.example.com\"\n"
    ));
    let mut overrides = overrides_for(&path);
    overrides.profile = Some(SECRET_NAME.to_string());
    let env = EnvLayer {
        profile: Some(SECRET_NAME.to_string()),
        ..EnvLayer::default()
    };
    let loaded = otl::config::load_file(&overrides, &EnvLayer::default()).unwrap();
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    let profile = loaded.file.profiles.get(SECRET_NAME).unwrap();

    for (label, rendered) in [
        ("Overrides", format!("{overrides:?}")),
        ("EnvLayer", format!("{env:?}")),
        ("Profile", format!("{profile:?}")),
        ("ConfigFile", format!("{:?}", loaded.file)),
        ("LoadedConfig", format!("{loaded:?}")),
        ("Settings", format!("{resolved:?}")),
    ] {
        assert!(
            !rendered.contains(SECRET_NAME),
            "{label} leaked the profile name: {rendered}"
        );
    }
}

#[test]
fn config_error_debug_never_exposes_raw_names_or_paths() {
    // The derived Debug printed `name`, `available` and `path` verbatim,
    // bypassing sanitize_name/sanitize_path. Debug now forwards to Display.
    const SECRET_NAME: &str = "KEY-SECRET-NAME";
    let hostile = "esc\u{1b}[31m-newline\nerror: forged";
    for error in [
        ConfigError::UnknownProfile {
            name: Some(hostile.to_string()),
            path: Some(PathBuf::from(format!("/tmp/{hostile}"))),
            available: vec![hostile.to_string(), SECRET_NAME.to_string()],
        },
        ConfigError::MissingUrl {
            profile: Some(hostile.to_string()),
        },
        ConfigError::ConflictingUrl {
            profile: hostile.to_string(),
        },
        ConfigError::CredentialInConfigFile {
            path: PathBuf::from(format!("/tmp/{hostile}")),
            location: "the top level".to_string(),
        },
    ] {
        let rendered = format!("{error:?}");
        assert!(
            !rendered.contains('\u{1b}'),
            "Debug carries ESC: {rendered:?}"
        );
        assert!(
            rendered
                .lines()
                .all(|line| !line.trim_start().starts_with("error:")),
            "Debug carries a forged diagnostic line: {rendered:?}"
        );
        assert!(
            rendered.starts_with("ConfigError("),
            "Debug is not the Display forward: {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// Config PATHS cannot inject into a terminal either (R2 finding 3).
// ---------------------------------------------------------------------------

#[test]
fn a_hostile_config_path_cannot_inject_into_a_diagnostic() {
    // OSC 8 hyperlink + BEL + a forged diagnostic line, in the path itself.
    let hostile =
        "/missing/\u{1b}]8;;https://evil.example.com\u{7}FORGED\u{1b}]8;;\u{7}\nerror: forged";
    let overrides = Overrides {
        config_path: Some(PathBuf::from(hostile)),
        ..Overrides::default()
    };
    let error = otl::config::load_file(&overrides, &EnvLayer::default()).unwrap_err();
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(!rendered.contains('\u{1b}'), "ESC survived: {rendered:?}");
        assert!(!rendered.contains('\u{7}'), "BEL survived: {rendered:?}");
        assert!(
            rendered
                .lines()
                .all(|line| !line.trim_start().starts_with("error:")),
            "forged diagnostic line: {rendered:?}"
        );
    }
}

#[test]
fn sanitize_path_replaces_control_characters_and_caps_length() {
    let cleaned = otl::config::sanitize_path(Path::new("/a/\u{1b}]8;;x\u{7}b\nc"));
    assert!(!cleaned.contains('\u{1b}'), "{cleaned:?}");
    assert!(!cleaned.contains('\u{7}'), "{cleaned:?}");
    assert!(!cleaned.contains('\n'), "{cleaned:?}");

    let long = "x".repeat(5_000);
    let capped = otl::config::sanitize_path(Path::new(&long));
    assert!(capped.chars().count() < 300, "not capped: {}", capped.len());
    assert!(capped.ends_with("..."), "{capped:?}");
}

// ---------------------------------------------------------------------------
// Environment variable names follow the platform's case rules (R2 finding 6).
// ---------------------------------------------------------------------------

#[test]
fn profile_key_variable_matching_follows_the_platform_case_rule() {
    // Windows: names are case-insensitive, but the environment block keeps
    // whatever case was used to set them, so a scan must fold case or it
    // reports a variable that IS set as missing.
    // POSIX: `outline_api_key_work` is a different variable and must not be
    // accepted as the key for profile `work`.
    for (name, case_insensitive, expected) in [
        ("OUTLINE_API_KEY_WORK", false, Some("WORK")),
        ("OUTLINE_API_KEY_WORK", true, Some("WORK")),
        ("outline_api_key_work", false, None),
        ("outline_api_key_work", true, Some("WORK")),
        ("Outline_Api_Key_Work", true, Some("WORK")),
        ("OUTLINE_API_KEY_SELF_HOSTED", true, Some("SELF_HOSTED")),
        // Not a per-profile variable at all.
        ("OUTLINE_API_KEY", true, None),
        ("OUTLINE_API_KEY", false, None),
        ("OUTLINE_URL", true, None),
        ("PATH", true, None),
    ] {
        assert_eq!(
            otl::config::profile_api_key_suffix(name, case_insensitive).as_deref(),
            expected,
            "{name:?} (case_insensitive={case_insensitive})"
        );
    }
}

#[test]
fn the_derived_suffix_matches_what_the_variable_scan_produces() {
    // The two sides must agree, or a key that is set is never found.
    for profile in ["work", "personal", "self-hosted", "a.b", "X1"] {
        let variable = otl::config::api_key_var(profile).unwrap();
        let scanned = otl::config::profile_api_key_suffix(&variable, false).unwrap();
        assert_eq!(
            Some(scanned.as_str()),
            otl::config::api_key_var_suffix(profile).as_deref(),
            "{profile}"
        );
    }
}

// ---------------------------------------------------------------------------
// A derived variable name is bounded at the source (R2 finding 7).
// ---------------------------------------------------------------------------

#[test]
fn an_overlong_profile_name_has_no_variable_and_makes_no_overlong_diagnostic() {
    // Long enough to blow the 64-character variable-name bound, but within
    // the config file's own size cap (which independently bounds the file).
    let long = "x".repeat(5_000);
    assert_eq!(
        otl::config::api_key_var(&long),
        None,
        "variable was derived"
    );
    let huge = "x".repeat(100_000);
    assert_eq!(otl::config::api_key_var(&huge), None);

    let (_dir, path) = config_file(&format!(
        "default_profile = \"{long}\"\n[profiles.{long}]\nurl = \"https://x.example.com\"\n"
    ));
    let overrides = overrides_for(&path);
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    let error = otl::config::EnvApiKey(&EnvLayer::default())
        .token(&resolved)
        .unwrap_err();
    assert!(
        matches!(error, ConfigError::ProfileApiKeyVarUnnameable { .. }),
        "{error:?}"
    );
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(
            rendered.chars().count() < 600,
            "diagnostic is {} chars",
            rendered.chars().count()
        );
    }
}

#[test]
fn a_missing_profile_key_diagnostic_stays_bounded() {
    // The variable name appears twice in this message, so its length has to
    // be bounded at derivation rather than truncated on the way out.
    let profile = "p".repeat(64);
    let (_dir, path) = config_file(&format!(
        "[profiles.{profile}]\nurl = \"https://x.example.com\"\n"
    ));
    let mut overrides = overrides_for(&path);
    overrides.profile = Some(profile.clone());
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    let error = otl::config::EnvApiKey(&EnvLayer::default())
        .token(&resolved)
        .unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, ConfigError::MissingProfileApiKey { .. }));
    assert!(
        message.chars().count() < 600,
        "diagnostic is {} chars",
        message.chars().count()
    );
}
