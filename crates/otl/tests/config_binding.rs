//! Which credential is released, and to which instance.
//!
//! The behaviour half of the credential gate: per-profile key scoping, the
//! binding rules for each `UrlSource`, and the proof's tie to the settings it
//! approved. The compile-time proofs that this cannot be circumvented live in
//! `config_isolation.rs`.

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

/// A stand-in for the credential-file credential source.
/// It would happily hand out a secret for any settings at all.
struct AlwaysYields(&'static str);

impl otl::config::TokenSource for AlwaysYields {
    fn fetch(&self, _checked: &otl::config::BindingChecked<'_>) -> Result<String, ConfigError> {
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
    // No profile, global variable, no config file.
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

/// A source that delegates to another one - the shape a chaining or
/// fallback credential source has, and the shape the laundering attack took.
struct Delegating<'a>(&'a EnvLayer);

impl otl::config::TokenSource for Delegating<'_> {
    fn fetch(&self, checked: &otl::config::BindingChecked<'_>) -> Result<String, ConfigError> {
        // The delegate can only be handed the approved settings: they come
        // out of the proof, not from anywhere this source chose.
        otl::config::EnvApiKey(self.0).fetch(checked)
    }
}

#[test]
fn a_proof_cannot_be_used_against_settings_it_did_not_approve() {
    // `target` is refused by the gate (env URL names another instance);
    // `benign` has no profile and passes trivially. Before the fix, a source
    // could obtain a proof for `benign` and spend it on `target`.
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let env = EnvLayer::default()
        .with_url("https://attacker.example.com")
        .with_api_key("global-key")
        .with_profile_api_key("work", "key-for-work");
    let target = settings(&overrides, &env).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let loaded = otl::config::load_from(&absent_default_source(&dir)).unwrap();
    let benign = resolve_settings(&Overrides::default(), &env, &loaded).unwrap();

    // The gate refuses the profile outright.
    assert!(matches!(
        release_token(&otl::config::EnvApiKey(&env), &target),
        Err(ConfigError::ConflictingUrl { .. })
    ));

    // A delegating source releasing under `benign` can only ever produce
    // `benign`'s credential - the global key - never the profile's.
    let laundered = release_token(&Delegating(&env), &benign).unwrap();
    assert_eq!(
        laundered, "global-key",
        "a proof issued for one Settings released another's credential"
    );
    assert_ne!(
        laundered, "key-for-work",
        "LEAKED the refused profile's key"
    );
}

#[test]
fn a_source_sees_exactly_the_settings_the_gate_approved() {
    // The positive half: what `fetch` reads out of the proof is the value
    // `release_token` was called with, so a source cannot be confused about
    // which instance it is being asked for.
    struct Echo;
    impl otl::config::TokenSource for Echo {
        fn fetch(&self, checked: &otl::config::BindingChecked<'_>) -> Result<String, ConfigError> {
            Ok(checked.settings().base_url().to_string())
        }
    }
    let (_dir, path) = config_file(TWO_PROFILES);
    let mut overrides = overrides_for(&path);
    overrides.profile = Some("work".to_string());
    let resolved = settings(&overrides, &EnvLayer::default()).unwrap();
    assert_eq!(
        release_token(&Echo, &resolved).unwrap(),
        "https://work.example.com"
    );
}
