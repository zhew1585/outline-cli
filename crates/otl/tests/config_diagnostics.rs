//! Story 4.1: what a configuration diagnostic may contain.
//!
//! Two rules, tested from every direction the reviews have found:
//!
//! - **no value from the config file is ever echoed**, because a user who
//!   wrongly puts a credential there must not see it again in a message, a
//!   log or a Debug rendering;
//! - **every name, path and free-form field is sanitized and bounded**,
//!   because a TOML quoted key and a `--config` argument can both carry
//!   control, bidi and zero-width characters into a terminal.
//!
//! `Debug` is held to the stricter standard than `Display`: `Display` has to
//! name the profile the user must fix, while `Debug` lands in logs, panic
//! messages and error chains, where nothing needs naming.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use otl::config::{
    release_token, resolve_settings, AuthMethod, Config, ConfigError, EnvLayer, Overrides, Settings,
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

/// Release a credential the way the binary does: through the shared gate,
/// never by calling a `TokenSource` directly (which the type system forbids).
fn release(env: &EnvLayer, resolved: &Settings) -> Result<String, ConfigError> {
    release_token(&otl::config::EnvApiKey(env), resolved)
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

/// A profile name carrying an ANSI sequence and a forged diagnostic line.
const HOSTILE_NAME: &str = "\u{1b}[31mRED\u{1b}[0m\nerror: forged";

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
        },
        ConfigError::ProfileApiKeyVarUnnameable {
            profile: secret.clone(),
        },
        ConfigError::UnboundProfileCredential {
            profile: secret.clone(),
        },
        ConfigError::ConflictingUrl {
            profile: secret.clone(),
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

#[test]
fn display_is_bounded_and_inert_for_any_construction_of_a_public_variant() {
    // The variants are public, so their string fields are not guaranteed to
    // be anything. Display must stay bounded and inert regardless.
    let hostile = format!(
        "{}\u{1b}[31m\u{202e}\n{}",
        "z".repeat(5_000),
        "y".repeat(5_000)
    );
    for error in [
        ConfigError::MalformedConfigFile {
            path: PathBuf::from(hostile.clone()),
            reason: hostile.clone(),
        },
        ConfigError::ConfigFileUnreadable {
            path: PathBuf::from(hostile.clone()),
            reason: hostile.clone(),
        },
        ConfigError::CredentialInConfigFile {
            path: PathBuf::from(hostile.clone()),
            location: hostile.clone(),
        },
        ConfigError::MissingProfileApiKey {
            profile: hostile.clone(),
            variable: hostile.clone(),
            global_set: false,
        },
    ] {
        let shown = error.to_string();
        assert!(
            shown.chars().count() < 1_500,
            "Display is unbounded ({} chars)",
            shown.chars().count()
        );
        assert!(!shown.contains('\u{1b}'), "ESC survived: {shown:?}");
        assert!(!shown.contains('\u{202e}'), "bidi survived: {shown:?}");
        assert!(shown.lines().count() <= 6, "forged lines: {shown:?}");
    }
}

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

#[test]
fn bidi_and_invisible_characters_are_scrubbed_from_names_and_paths() {
    // U+202E is not `char::is_control()`, but it reverses the visual order of
    // everything after it - enough to make a path or a reason read as
    // something else - and a truncated string can leave the state open.
    for hostile in [
        "safe\u{202e}txt",
        "a\u{200f}b",
        "a\u{2066}b\u{2069}c",
        "a\u{200b}b",
        "a\u{feff}b",
        "a\u{00ad}b",
    ] {
        let name = otl::config::sanitize_name(hostile);
        let path = otl::config::sanitize_path(Path::new(hostile));
        let text = otl::config::sanitize_text(hostile);
        for (label, cleaned) in [("name", name), ("path", path), ("text", text)] {
            for bad in hostile.chars().filter(|c| {
                matches!(*c as u32, 0x061c | 0x200b..=0x200f | 0x202a..=0x202e
                    | 0x2060..=0x2069 | 0x00ad | 0x180e | 0xfeff)
            }) {
                assert!(
                    !cleaned.contains(bad),
                    "{label} kept U+{:04X} from {hostile:?}: {cleaned:?}",
                    bad as u32
                );
            }
        }
    }
}

#[test]
fn a_bidi_config_path_reaches_a_diagnostic_inert() {
    let hostile = "/missing/safe\u{202e}txt";
    let overrides = Overrides {
        config_path: Some(PathBuf::from(hostile)),
        ..Overrides::default()
    };
    let error = otl::config::load_file(&overrides, &EnvLayer::default()).unwrap_err();
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(
            !rendered.contains('\u{202e}'),
            "bidi override survived: {rendered:?}"
        );
    }
}

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
    let error = release(&EnvLayer::default(), &resolved).unwrap_err();
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
    let error = release(&EnvLayer::default(), &resolved).unwrap_err();
    let message = error.to_string();
    assert!(matches!(error, ConfigError::MissingProfileApiKey { .. }));
    assert!(
        message.chars().count() < 600,
        "diagnostic is {} chars",
        message.chars().count()
    );
}
