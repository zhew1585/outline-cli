//! Story 4.1: what a `Debug` rendering of a configuration type may contain.
//!
//! `Debug` is held to a stricter standard than `Display`: it lands in logs,
//! panic messages and error chains, where nothing needs naming, so it carries
//! no URL beyond an origin, no profile name, no path and no error field text
//! at all. `Display` - which has to name the thing the user must fix - is
//! covered in `config_diagnostics.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use otl::config::{
    resolve_settings, AuthMethod, Config, ConfigError, EnvLayer, Overrides, Settings,
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

/// Resolve settings from explicit layers, exactly as the binary does.
fn settings(overrides: &Overrides, env: &EnvLayer) -> Result<Settings, ConfigError> {
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
fn env_layer_debug_redacts_the_api_key() {
    let env = EnvLayer::default()
        .with_api_key("super-secret-key")
        .with_url("https://alice:pw-secret@example.com")
        .with_profile_api_key("work", "profile-secret-key");
    let rendered = format!("{env:?}");
    assert!(!rendered.contains("super-secret-key"), "{rendered}");
    assert!(!rendered.contains("pw-secret"), "{rendered}");
    assert!(!rendered.contains("profile-secret-key"), "{rendered}");
}

#[test]
fn settings_debug_redacts_the_base_url() {
    let (_dir, path) =
        config_file("[profiles.work]\nurl = \"https://alice:pw-secret@example.com/PATH-SECRET\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    let rendered = format!("{resolved:?}");
    assert!(!rendered.contains("pw-secret"), "{rendered}");
    assert!(!rendered.contains("PATH-SECRET"), "{rendered}");
    // The profile NAME is redacted too (R2 finding 2): a name is config-file
    // content, and Debug is an unbounded surface. Set-ness is still visible.
    assert!(!rendered.contains("work"), "{rendered}");
    assert!(rendered.contains("***"), "{rendered}");
}

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
    let env = EnvLayer::default()
        .with_url(SECRET_URL)
        .with_api_key("KEY-SECRET");
    let (_dir, path) = config_file(&format!("[profiles.work]\nurl = \"{SECRET_URL}\"\n"));
    let loaded = otl::config::load_file(&overrides_for(&path), &EnvLayer::default()).unwrap();
    let profile = loaded.file.profiles.get("work").unwrap();
    let mut selected = overrides_for(&path);
    selected.profile = Some("work".to_string());
    let resolved = settings(&selected, &EnvLayer::default()).unwrap();
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

#[test]
fn no_configuration_type_leaks_a_profile_name_through_debug() {
    const SECRET_NAME: &str = "KEY-SECRET-NAME";
    let (_dir, path) = config_file(&format!(
        "default_profile = \"{SECRET_NAME}\"\n\
         [profiles.{SECRET_NAME}]\nurl = \"https://x.example.com\"\n"
    ));
    let mut overrides = overrides_for(&path);
    overrides.profile = Some(SECRET_NAME.to_string());
    let env = EnvLayer::default().with_profile(SECRET_NAME);
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
fn no_configuration_type_leaks_a_config_path_through_debug() {
    const SECRET: &str = "KEY-SECRET-DIR";
    let dir = tempfile::tempdir().unwrap();
    let secret_dir = dir.path().join(SECRET);
    std::fs::create_dir(&secret_dir).unwrap();
    let path = secret_dir.join("config.toml");
    std::fs::write(&path, TWO_PROFILES).unwrap();

    let overrides = Overrides {
        config_path: Some(path.clone()),
        ..Overrides::default()
    };
    let env = EnvLayer::default().with_config_path(path.clone());
    let source = otl::config::locate(&overrides, &env);
    let loaded = otl::config::load_from(&source).unwrap();

    for (label, rendered) in [
        ("Overrides", format!("{overrides:?}")),
        ("EnvLayer", format!("{env:?}")),
        ("ConfigSource", format!("{source:?}")),
        ("LoadedConfig", format!("{loaded:?}")),
    ] {
        assert!(
            !rendered.contains(SECRET),
            "{label} leaked the config path: {rendered}"
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
            source: otl::config::ProfileSource::Flag,
        },
        ConfigError::CredentialInConfigFile {
            path: PathBuf::from(format!("/tmp/{hostile}")),
            location: "the top level".to_string(),
        },
    ] {
        let rendered = format!("{error:?}");
        // R3 finding 2: this is the assertion the previous version was
        // missing. The secret WAS in `available`, and forwarding Debug to
        // Display printed it.
        assert!(
            !rendered.contains(SECRET_NAME),
            "Debug leaked a profile name: {rendered}"
        );
        assert!(
            !rendered.contains(hostile),
            "Debug leaked raw field text: {rendered:?}"
        );
        assert!(
            !rendered.contains("tmp"),
            "Debug leaked a path: {rendered:?}"
        );
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
            rendered.starts_with("ConfigError::"),
            "Debug is not the structural rendering: {rendered}"
        );
    }
}

#[test]
fn config_error_debug_carries_no_field_text_for_any_variant() {
    // Every variant, each with a recognizable secret in every string field.
    const SECRET: &str = "KEY-SECRET-FIELD";
    let secret = SECRET.to_string();
    let path = PathBuf::from(format!("/tmp/{SECRET}/config.toml"));
    for error in [
        ConfigError::MissingUrl {
            profile: Some(secret.clone()),
        },
        ConfigError::MissingApiKey,
        ConfigError::MissingProfileApiKey {
            profile: secret.clone(),
            variable: secret.clone(),
            global_set: true,
            source: otl::config::ProfileSource::Flag,
        },
        ConfigError::ProfileApiKeyVarUnnameable {
            profile: secret.clone(),
        },
        ConfigError::UnboundProfileCredential {
            profile: secret.clone(),
        },
        ConfigError::ConflictingUrl {
            profile: secret.clone(),
            source: otl::config::ProfileSource::Flag,
        },
        ConfigError::InvalidProfileUrl {
            profile: secret.clone(),
        },
        ConfigError::AmbiguousProfileApiKeyVar {
            profile: secret.clone(),
            other: secret.clone(),
            variable: secret.clone(),
        },
        ConfigError::UnknownProfile {
            name: Some(secret.clone()),
            path: Some(path.clone()),
            available: vec![secret.clone()],
        },
        ConfigError::ConfigFileUnreadable {
            path: path.clone(),
            reason: secret.clone(),
        },
        ConfigError::MalformedConfigFile {
            path: path.clone(),
            reason: secret.clone(),
        },
        ConfigError::CredentialInConfigFile {
            path: path.clone(),
            location: secret.clone(),
        },
        ConfigError::UnsupportedAuthMethod {
            profile: Some(secret.clone()),
            method: AuthMethod::Oauth,
        },
    ] {
        let rendered = format!("{error:?}");
        assert!(
            !rendered.contains(SECRET),
            "Debug leaked a field: {rendered}"
        );
        assert!(
            rendered.starts_with("ConfigError::"),
            "unexpected Debug shape: {rendered}"
        );
    }
}
