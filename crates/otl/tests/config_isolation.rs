//! Story 4.1: the credential-release boundary is structural.
//!
//! The gate ([`otl::config::release_token`]) is only as strong as the state
//! it decides from, so three things have to be impossible to forge:
//!
//! - a `Settings` claiming a `UrlSource` the layers never produced;
//! - a read of the API key that does not pass through the gate;
//! - a `BindingChecked` minted without running the check.
//!
//! Rust's privacy rules make that a question about MODULE LAYOUT, not about
//! the `pub` keyword: a private field is visible to the declaring module and
//! to every descendant of it. Fields declared private in `config` would
//! still be reachable from `config::anything_added_later`. The security
//! state therefore lives in leaf modules - `config::resolved` owns
//! `Settings`, `config::secret` owns the keys, `config::release` owns the
//! proof token - and none of them is an ancestor of the others.
//!
//! These tests check that from both sides: an external crate (what a library
//! consumer can do) and a sibling module inside `config` (what the Epic 2
//! credential source would be able to do).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use otl::config::{
    release_token, resolve_settings, ConfigError, ConfigSource, EnvLayer, Overrides, Settings,
    UrlSource,
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

/// A default (non-explicit) config location pointing at a file that does not
/// exist - the shape of a fresh machine.
fn absent_default_source(dir: &TempDir) -> ConfigSource {
    ConfigSource {
        path: Some(dir.path().join("config.toml")),
        explicit: false,
    }
}

/// Release a credential the way the binary does: through the shared gate,
/// never by calling a `TokenSource` directly (which the type system forbids).
fn release(env: &EnvLayer, resolved: &Settings) -> Result<String, ConfigError> {
    release_token(&otl::config::EnvApiKey(env), resolved)
}

const TWO_PROFILES: &str = r#"
default_profile = "personal"

[profiles.work]
url = "https://work.example.com"
auth = "api-key"

[profiles.personal]
url = "https://personal.example.com"
"#;

/// A stand-in for a future credential source (the Epic 2 credential file).
/// It would happily hand out a secret for any settings at all.
struct AlwaysYields(&'static str);

impl otl::config::TokenSource for AlwaysYields {
    fn fetch(
        &self,
        _settings: &Settings,
        _checked: &otl::config::BindingChecked,
    ) -> Result<String, ConfigError> {
        Ok(self.0.to_string())
    }
}

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
            release(&env, &resolved).unwrap(),
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
    let env = EnvLayer::default().with_api_key("key-for-work");
    let resolved = settings(&overrides, &env).unwrap();
    let error = release(&env, &resolved).unwrap_err();
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
    let error = release(&env, &resolved).unwrap_err();
    assert!(matches!(error, ConfigError::MissingProfileApiKey { .. }));
    assert!(!error.to_string().contains("key-for-work"));
}

#[test]
fn the_global_api_key_still_serves_the_profile_less_path() {
    // Epic 1 behaviour, unchanged: no profile, global variable, no config.
    let dir = tempfile::tempdir().unwrap();
    let env = EnvLayer::default()
        .with_url("https://env.example.com")
        .with_api_key("global-key");
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.profile(), None);
    assert_eq!(release(&env, &resolved).unwrap(), "global-key");
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
    let env = EnvLayer::default().with_api_key("global-key");
    let resolved = settings(&overrides, &env).unwrap();
    let error = release(&env, &resolved).unwrap_err();
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
    assert_eq!(resolved.base_url(), "https://c.example.com");
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
    let error = release(&env, &resolved).unwrap_err();
    assert!(matches!(error, ConfigError::MissingProfileApiKey { .. }));
}

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

#[test]
fn the_binding_gate_applies_to_every_token_source() {
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let conflicting = EnvLayer::default().with_url("https://elsewhere.example.com");
    let resolved = settings(&overrides, &conflicting).unwrap();

    // A source that never refuses anything is still refused by the gate.
    let error = release_token(&AlwaysYields("secret-from-a-file"), &resolved).unwrap_err();
    assert!(
        matches!(error, ConfigError::ConflictingUrl { .. }),
        "the gate is inside EnvApiKey rather than at the boundary: {error:?}"
    );
    assert!(!error.to_string().contains("secret-from-a-file"));

    // And it releases when the binding holds.
    let ok = settings(&overrides, &EnvLayer::default()).unwrap();
    assert_eq!(
        release_token(&AlwaysYields("secret-from-a-file"), &ok).unwrap(),
        "secret-from-a-file"
    );
}

#[test]
fn no_profile_means_no_binding_question() {
    // The global credential and the global URL variable are one scope.
    let dir = tempfile::tempdir().unwrap();
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let env = EnvLayer::default()
        .with_url("https://env.example.com")
        .with_api_key("global-key");
    let resolved = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();
    assert_eq!(resolved.url_source(), UrlSource::Env);
    assert_eq!(release(&env, &resolved).unwrap(), "global-key");
}

#[test]
fn the_gate_still_governs_an_honestly_resolved_flag_redirect() {
    // The counterpart to the compile-fail cases: a genuine `--url` (the only
    // way to obtain `UrlSource::Flag`) is still allowed, so the fix closed the
    // forgery without closing the documented escape hatch.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    overrides.url = Some("https://elsewhere.example.com".to_string());
    let env = EnvLayer::default().with_profile_api_key("work", "key-for-work");
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.url_source(), UrlSource::Flag);
    assert_eq!(release(&env, &resolved).unwrap(), "key-for-work");
}

#[test]
fn an_unusable_env_url_is_left_to_the_request_channel() {
    // Nothing can be sent to it, so there is no credential exposure to
    // prevent, and the request channel gives the precise message.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("not-a-url")
        .with_profile_api_key("work", "key-for-work");
    let resolved = settings(&overrides, &env).unwrap();
    assert_eq!(resolved.base_url(), "not-a-url");
    assert!(
        release(&env, &resolved).is_ok(),
        "an unusable URL was reported as a cross-instance conflict"
    );
}

#[test]
fn an_unusable_profile_url_is_named_as_the_problem() {
    // Here the binding genuinely cannot be established, and the profile's own
    // configuration is what needs fixing - so it must not be reported as
    // OUTLINE_URL naming a different instance.
    let (_dir, path) = config_file("[profiles.work]\nurl = \"not-a-url\"\n");
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://real.example.com")
        .with_profile_api_key("work", "key-for-work");
    let resolved = settings(&overrides, &env).unwrap();
    let error = release(&env, &resolved).unwrap_err();
    assert!(
        matches!(error, ConfigError::InvalidProfileUrl { .. }),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("work"), "{message}");
    assert!(!message.contains("different instance"), "{message}");
    assert!(!message.contains("key-for-work"), "{message}");
}

/// Whether a compiler failure is about reachability rather than, say, a typo
/// in the probe.
///
/// "No such field" counts: a field that has been moved into a leaf module is
/// not merely private to the caller, it is not part of the type it used to be
/// on. Either way the attack does not compile, which is the property.
fn is_privacy_rejection(stderr: &str) -> bool {
    [
        "private", // the general wording
        "E0451",   // private field in a struct literal
        "E0616",   // private field access
        "E0603",   // private module / item
        "E0609",   // no such field (moved into a leaf)
        "E0560",   // struct has no such field
        "no field",
        "not a tuple struct", // BindingChecked's private field
    ]
    .iter()
    .any(|marker| stderr.contains(marker))
}

/// The config module sources, as a compilable standalone crate in a temp dir.
///
/// `mod.rs` becomes a child module of a generated `lib.rs`, so the module
/// tree - and therefore every privacy relationship inside it - is identical
/// to the real one.
fn config_tree_copy() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config");
    let mut copied = 0;
    for entry in std::fs::read_dir(&source).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            std::fs::copy(&path, config.join(path.file_name().unwrap())).unwrap();
            copied += 1;
        }
    }
    assert!(copied >= 5, "expected the config module to have leaf files");
    std::fs::write(dir.path().join("lib.rs"), "pub mod config;\n").unwrap();
    dir
}

/// Compile the copied tree; returns the compiler's stderr on failure.
fn compile_config_tree(dir: &TempDir) -> Option<String> {
    let deps = Path::new(env!("CARGO_BIN_EXE_otl"))
        .parent()
        .unwrap()
        .join("deps");
    let newest = |prefix: &str| -> PathBuf {
        std::fs::read_dir(&deps)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".rlib"))
            })
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
            .unwrap_or_else(|| panic!("no {prefix}*.rlib in {}", deps.display()))
    };
    let mut command =
        std::process::Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()));
    command
        .arg(dir.path().join("lib.rs"))
        .arg("--crate-type=lib")
        .arg("--edition=2021")
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("-o")
        .arg(dir.path().join("probe.rlib"));
    for (name, prefix) in [
        ("engine", "libengine-"),
        ("serde", "libserde-"),
        ("toml", "libtoml-"),
        ("directories", "libdirectories-"),
    ] {
        command
            .arg("--extern")
            .arg(format!("{name}={}", newest(prefix).display()));
    }
    let output = command.output().unwrap();
    (!output.status.success()).then(|| String::from_utf8_lossy(&output.stderr).to_string())
}

/// Add a sibling module inside `config` containing `body`.
fn with_attacker_module(dir: &TempDir, body: &str) {
    let mod_rs = dir.path().join("config/mod.rs");
    let mut source = std::fs::read_to_string(&mod_rs).unwrap();
    source.insert_str(source.find("mod error;").unwrap(), "mod attacker;\n");
    std::fs::write(&mod_rs, source).unwrap();
    std::fs::write(dir.path().join("config/attacker.rs"), body).unwrap();
}

/// What a module added inside `config` must not be able to do, and why.
const INTERNAL_ATTACKS: &[(&str, &str)] = &[
    (
        "forge a Settings claiming a Flag url_source",
        r#"
        use super::{AuthMethod, Settings, UrlSource};
        pub fn forge() -> Settings {
            Settings {
                profile: Some("work".to_string()),
                base_url: "https://attacker.example.com".to_string(),
                url_source: UrlSource::Flag,
                profile_url: None,
                auth: AuthMethod::ApiKey,
            }
        }
        "#,
    ),
    (
        "read the global API key out of the layer",
        r#"
        use super::EnvLayer;
        pub fn steal(env: &EnvLayer) -> Option<String> {
            env.keys().global.clone()
        }
        "#,
    ),
    (
        "read a per-profile API key out of the layer",
        r#"
        use super::EnvLayer;
        pub fn steal(env: &EnvLayer) -> Option<String> {
            env.keys().per_profile.get("WORK").cloned()
        }
        "#,
    ),
    (
        "mint a BindingChecked without running the check",
        r#"
        use super::BindingChecked;
        pub fn forge() -> BindingChecked {
            BindingChecked(())
        }
        "#,
    ),
];

#[test]
fn a_module_added_inside_config_cannot_forge_the_gates_state() {
    // Permission probe first: the unmodified tree must compile, or every
    // case below would "pass" for the wrong reason.
    let clean = config_tree_copy();
    if let Some(stderr) = compile_config_tree(&clean) {
        panic!("the probe harness is broken; the config tree did not compile:\n{stderr}");
    }

    for (what, body) in INTERNAL_ATTACKS {
        let dir = config_tree_copy();
        with_attacker_module(&dir, body);
        let stderr = compile_config_tree(&dir)
            .unwrap_or_else(|| panic!("A MODULE INSIDE config CAN STILL {what}"));
        assert!(
            is_privacy_rejection(&stderr),
            "{what}: rejected for the wrong reason:\n{stderr}"
        );
    }
}

#[test]
fn the_security_state_lives_in_leaf_modules() {
    // The whole argument rests on `resolved`, `secret` and `release` having
    // no descendants: a submodule of any of them would inherit exactly the
    // access the tests above prove a sibling does not have. This is cheap to
    // assert and cannot be spotted by reading a diff of the leaf itself.
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config");
    for leaf in ["resolved.rs", "secret.rs", "release.rs"] {
        let source = std::fs::read_to_string(config.join(leaf)).unwrap();
        for (number, line) in source.lines().enumerate() {
            let declares_module = line.trim_start().starts_with("mod ")
                || line.trim_start().starts_with("pub mod ")
                || line.trim_start().starts_with("pub(super) mod ")
                || line.trim_start().starts_with("pub(crate) mod ");
            assert!(
                !declares_module,
                "{leaf}:{} declares a submodule; the credential gate relies on \
                 this module having no descendants",
                number + 1
            );
        }
    }

    // And the state must be declared in those leaves, not in `config` itself,
    // where every sibling could reach it.
    let mod_rs = std::fs::read_to_string(config.join("mod.rs")).unwrap();
    for (item, leaf) in [
        ("pub struct Settings", "resolved.rs"),
        ("pub enum UrlSource", "resolved.rs"),
        ("pub struct EnvKeys", "secret.rs"),
        ("pub struct BindingChecked", "release.rs"),
    ] {
        assert!(
            !mod_rs.contains(item),
            "{item} is declared in config/mod.rs; it belongs in {leaf}"
        );
        let source = std::fs::read_to_string(config.join(leaf)).unwrap();
        assert!(source.contains(item), "{item} is not declared in {leaf}");
    }
}
