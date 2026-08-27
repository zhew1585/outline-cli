//! Runtime configuration: command-line flags over environment variables
//! over a single user config file (TOML, with named profiles).
//!
//! Precedence is **flag > env > config file, per key**: the env supplying a
//! base URL does not discard the selected profile's authentication method.
//! Every layer is captured as plain data ([`Overrides`], [`EnvLayer`],
//! [`ConfigFile`]) so that [`resolve_settings`] is a pure function.
//!
//! Credentials are NOT part of this file. Resolution stops at the base URL
//! and the authentication *method*; the secret itself comes from a
//! [`TokenSource`]. A credential found in the config file is a hard error:
//! the config file is meant to be shareable and committable, the
//! credential file is not.
//!
//! # Credentials are scoped to their instance
//!
//! A profile names an INSTANCE, so the credential sent to it must belong to
//! that instance. Two rules enforce that, in both directions:
//!
//! - the CREDENTIAL comes from the profile's own scope. The global
//!   `OUTLINE_API_KEY` is used only when no profile is in effect; a profile
//!   reads `OUTLINE_API_KEY_<PROFILE>` and refuses to fall back, because
//!   falling back would send one workspace's key to another workspace's
//!   server. See [`EnvApiKey`].
//! - the credential is only RELEASED once it is bound to the origin the
//!   request will use, via [`release_token`], which refuses to release a
//!   profile's credential to an origin that profile never named.
//!
//! The binding rules, applied only when a profile is in effect:
//!
//! | base URL came from | released? |
//! |--------------------|-----------|
//! | `--url`            | yes - stated in the same command, so the redirect is deliberate |
//! | the profile's `url`| yes - the profile named the origin itself |
//! | `OUTLINE_URL`      | only if its origin matches the profile's declared `url` |
//! | `OUTLINE_URL`, profile declares no `url` | no - there is nothing to bind the credential to |
//!
//! Comparison is by normalized ORIGIN (`scheme://host[:port]`), so a
//! trailing slash, host casing or a default port cannot produce a false
//! conflict, and a path difference cannot hide the fact that it is the same
//! server receiving the credential.
//!
//! The gate lives at the credential-release boundary rather than inside a
//! [`TokenSource`], and is structural: [`TokenSource::fetch`] demands a
//! `BindingChecked`, whose only field is private, so [`release_token`] is
//! the only way to reach any source; [`Settings`] has private fields and
//! no public constructor, so the input the gate decides from can only come
//! out of [`resolve_settings`]; [`EnvLayer`]'s secret fields are private
//! with no accessor.
//!
//! # Nothing from the config file is echoed back
//!
//! A user who wrongly puts a secret in the config file must not see it
//! again in a diagnostic, a log or a Debug rendering:
//!
//! - config-file diagnostics are built only from text this module owns
//!   plus a line number (see `file::parse_reason`), never from
//!   parser-produced text, since `toml`'s messages interpolate the
//!   offending value;
//! - every name that does reach a diagnostic goes through
//!   [`sanitize_name`], because a TOML quoted key can carry ESC and
//!   newline bytes straight into a terminal.

mod credentials;
mod error;
mod file;
mod release;
mod resolved;
mod secret;

pub use credentials::{
    select as select_credential_source, Source as CredentialSource, StoredCredential,
};
pub use error::{sanitize_name, sanitize_path, sanitize_text, ConfigError};
pub use file::{config_dir, default_config_path, load_file, load_from, locate, CONFIG_FILE_NAME};
pub use release::{release_token, BindingChecked, TokenSource};
pub use resolved::{resolve_profile_name, resolve_settings, ProfileSource, Settings, UrlSource};
pub use secret::EnvApiKey;

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{Deserializer, IgnoredAny};
use serde::Deserialize;

/// Environment variable holding the Outline instance base URL.
pub const ENV_URL: &str = "OUTLINE_URL";
/// Environment variable holding the API key when no profile is in effect.
pub const ENV_API_KEY: &str = "OUTLINE_API_KEY";
/// Prefix of the per-profile API key environment variable, completed with
/// the profile name mapped by [`api_key_var_suffix`] (`work` ->
/// `OUTLINE_API_KEY_WORK`).
pub const ENV_API_KEY_PREFIX: &str = "OUTLINE_API_KEY_";
/// Environment variable selecting a named profile.
pub const ENV_PROFILE: &str = "OUTLINE_PROFILE";
/// Environment variable overriding the user config file location.
pub const ENV_CONFIG: &str = "OUTLINE_CONFIG";

/// File name of the credential file, which the auth layer owns. Named here
/// only so that error messages can point at it: no credential is ever read
/// from, or written to, the config file.
pub const CREDENTIALS_FILE_NAME: &str = "credentials.toml";

/// Placeholder shown instead of secrets in Debug output.
const REDACTED: &str = "***";
/// Maximum profile-name length that still maps to an API key variable.
///
/// Bounds the derived variable name, which appears in diagnostics as advice
/// to act on; see [`api_key_var_suffix`].
const MAX_PROFILE_VAR_CHARS: usize = 64;

/// How a profile authenticates.
///
/// This is the *method*, not the secret: see [`TokenSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    /// Bearer API key (`OUTLINE_API_KEY` / the credential file).
    #[default]
    #[serde(alias = "api_key", alias = "apikey")]
    ApiKey,
    /// Browser OAuth 2.0 (`otl auth login`).
    Oauth,
}

impl fmt::Display for AuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ApiKey => "api-key",
            Self::Oauth => "oauth",
        })
    }
}

/// Marker for a config-file key that must never hold a credential.
///
/// Deserializing it discards the value (`IgnoredAny`), so the secret is
/// never materialized, cannot end up in a Debug rendering, and cannot be
/// echoed back in an error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeniedSecret;

impl<'de> Deserialize<'de> for DeniedSecret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer)?;
        Ok(DeniedSecret)
    }
}

/// One named profile: which instance, and how to authenticate to it.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Base URL of the instance (cloud or self-hosted).
    pub url: Option<String>,
    /// Authentication method; defaults to [`AuthMethod::ApiKey`].
    pub auth: Option<AuthMethod>,
    /// Trap only - see [`DeniedSecret`].
    #[serde(default)]
    api_key: Option<DeniedSecret>,
    /// Trap only - see [`DeniedSecret`].
    #[serde(default)]
    token: Option<DeniedSecret>,
}

impl fmt::Debug for Profile {
    /// Manual impl: a config-file URL can embed credentials in its
    /// userinfo, path, query or fragment exactly as an environment one can,
    /// so it is reduced to its origin here too.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("url_origin", &redacted_origin(self.url.as_deref()))
            .field("auth", &self.auth)
            .finish()
    }
}

/// The parsed user config file.
///
/// Unknown keys are rejected rather than ignored: a typo in a config file
/// that silently does nothing is worse than a readable error.
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Profile used when neither `--profile` nor `OUTLINE_PROFILE` selects
    /// one.
    pub default_profile: Option<String>,
    /// Named profiles, keyed by profile name.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Trap only - see [`DeniedSecret`].
    #[serde(default)]
    api_key: Option<DeniedSecret>,
    /// Trap only - see [`DeniedSecret`].
    #[serde(default)]
    token: Option<DeniedSecret>,
}

impl fmt::Debug for ConfigFile {
    /// Manual impl: delegates to [`Profile`]'s redacting Debug, and shows no
    /// profile NAME at all - see `redacted_name`. `default_profile` is a
    /// config-file value like any other, and a table key is one too.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A Vec, not the map: rendering the map would print its keys.
        let profiles: Vec<&Profile> = self.profiles.values().collect();
        f.debug_struct("ConfigFile")
            .field(
                "default_profile",
                &redacted_name(self.default_profile.as_deref()),
            )
            .field("profiles", &profiles)
            .finish()
    }
}

/// Command-line overrides: the highest-precedence layer.
#[derive(Clone, Default)]
pub struct Overrides {
    /// `--profile NAME`.
    pub profile: Option<String>,
    /// `--url URL`.
    pub url: Option<String>,
    /// `--config FILE`.
    pub config_path: Option<PathBuf>,
}

impl fmt::Debug for Overrides {
    /// Manual impl: `--url` carries the same credential-bearing components
    /// as the environment variable, so it is reduced to its origin.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Overrides")
            .field("profile", &redacted_name(self.profile.as_deref()))
            .field("url_origin", &redacted_origin(self.url.as_deref()))
            .field("config_path", &redacted_path(self.config_path.as_deref()))
            .finish()
    }
}

/// The environment layer, captured as data.
///
/// The fields are private and there is no accessor for the secrets: a
/// credential is obtainable only through [`release_token`], which applies
/// the binding check first. The non-secret parts are readable through
/// [`EnvLayer::profile`], [`EnvLayer::url`] and [`EnvLayer::config_path`].
#[derive(Clone, Default)]
pub struct EnvLayer {
    /// `OUTLINE_PROFILE`.
    profile: Option<String>,
    /// `OUTLINE_URL`.
    url: Option<String>,
    /// `OUTLINE_CONFIG`.
    config_path: Option<PathBuf>,
    /// The API keys, in a container this module cannot read out of - see
    /// `secret::EnvKeys`.
    keys: secret::EnvKeys,
}

impl fmt::Debug for EnvLayer {
    /// Manual impl: the API key must never appear, and the base URL is
    /// reduced to its origin (userinfo, path, query and fragment can all
    /// carry credentials).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvLayer")
            .field("profile", &redacted_name(self.profile.as_deref()))
            .field("url_origin", &redacted_origin(self.url.as_deref()))
            .field("api_key", &REDACTED)
            .field("config_path", &redacted_path(self.config_path.as_deref()))
            // Count only: the suffixes name profiles, but the values are
            // keys, and a map rendering would print them.
            .field("profile_api_keys", &self.keys.profile_key_count())
            .finish()
    }
}

impl EnvLayer {
    /// Read the environment layer from the process environment.
    ///
    /// Blank values count as unset: an exported-but-empty variable must not
    /// shadow the config file.
    pub fn from_process() -> Self {
        let read = |name: &str| env::var(name).ok();
        Self {
            keys: secret::EnvKeys::from_process(),
            ..Self::from_values(
                read(ENV_PROFILE).as_deref(),
                read(ENV_URL).as_deref(),
                None,
                // Not filtered on emptiness: `OUTLINE_CONFIG=` is the
                // documented way to say "read no config file at all"
                // (see [`locate`]).
                env::var_os(ENV_CONFIG).map(PathBuf::from),
            )
        }
    }

    /// Build the layer from explicit values, applying the blank-is-unset
    /// rule. The seam that lets tests avoid mutating the environment.
    pub fn from_values(
        profile: Option<&str>,
        url: Option<&str>,
        api_key: Option<&str>,
        config_path: Option<PathBuf>,
    ) -> Self {
        let keys = match api_key {
            Some(api_key) => secret::EnvKeys::default().with_global(api_key),
            None => secret::EnvKeys::default(),
        };
        Self {
            profile: non_blank(profile),
            url: non_blank(url),
            config_path,
            keys,
        }
    }

    /// The selected profile name from `OUTLINE_PROFILE`.
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// The base URL from `OUTLINE_URL`.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// The config-file path from `OUTLINE_CONFIG`.
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// The opaque key container. Only `config::secret` can read out of it.
    pub(super) fn keys(&self) -> &secret::EnvKeys {
        &self.keys
    }

    /// The layer with `OUTLINE_PROFILE` set.
    pub fn with_profile(mut self, profile: &str) -> Self {
        self.profile = non_blank(Some(profile));
        self
    }

    /// The layer with `OUTLINE_URL` set.
    pub fn with_url(mut self, url: &str) -> Self {
        self.url = non_blank(Some(url));
        self
    }

    /// The layer with the global `OUTLINE_API_KEY` set.
    pub fn with_api_key(mut self, api_key: &str) -> Self {
        self.keys = self.keys.with_global(api_key);
        self
    }

    /// The layer with `OUTLINE_CONFIG` set. An empty path means "read no
    /// config file", exactly as the variable does.
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// The layer with one profile-scoped API key added, applying the same
    /// blank-is-unset rule as the process environment. Test seam.
    pub fn with_profile_api_key(mut self, profile: &str, api_key: &str) -> Self {
        self.keys = self.keys.with_profile(profile, api_key);
        self
    }
}

/// Whether environment variable NAMES are case-insensitive on this platform.
///
/// They are on Windows, where the environment block nevertheless preserves
/// whatever case was used to set a variable: `set outline_api_key_work=K`
/// stores that spelling, and `GetEnvironmentVariable` still finds it under
/// any case. `std::env::var` therefore works for the fixed names, but a scan
/// over `std::env::vars` sees the original spelling and must compare
/// case-insensitively or it would miss the variable and report it unset.
/// POSIX names are case-sensitive, where `outline_api_key_work` is a
/// genuinely different variable that must NOT be accepted.
pub(super) const ENV_NAMES_ARE_CASE_INSENSITIVE: bool = cfg!(windows);

/// The profile-key suffix of an environment variable name, or `None` when the
/// name is not a per-profile API key variable.
///
/// `case_insensitive` selects the platform rule (see
/// `ENV_NAMES_ARE_CASE_INSENSITIVE`); it is a parameter rather than a
/// `cfg!` inside the body so that both platform behaviours are testable
/// everywhere. The suffix is upper-cased on the case-insensitive path so it
/// matches what [`api_key_var_suffix`] derives from a profile name.
pub fn profile_api_key_suffix(env_name: &str, case_insensitive: bool) -> Option<String> {
    let suffix = if case_insensitive {
        let upper = env_name.to_ascii_uppercase();
        upper.strip_prefix(ENV_API_KEY_PREFIX)?.to_string()
    } else {
        env_name.strip_prefix(ENV_API_KEY_PREFIX)?.to_string()
    };
    (!suffix.is_empty()).then_some(suffix)
}

/// The `OUTLINE_API_KEY_*` variable suffix for a profile name.
///
/// ASCII alphanumerics are upper-cased and every other character becomes
/// `_`, so `work` -> `WORK` and `self-hosted` -> `SELF_HOSTED`.
///
/// `None` when the name cannot be expressed as an environment variable
/// name: no ASCII alphanumeric at all, or longer than the 64-character cap
/// ([`MAX_PROFILE_VAR_CHARS`]).
pub fn api_key_var_suffix(profile: &str) -> Option<String> {
    if profile.chars().count() > MAX_PROFILE_VAR_CHARS {
        return None;
    }
    let suffix: String = profile
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    suffix
        .chars()
        .any(|c| c.is_ascii_alphanumeric())
        .then_some(suffix)
}

/// The full environment variable name holding a profile's API key.
pub fn api_key_var(profile: &str) -> Option<String> {
    api_key_var_suffix(profile).map(|suffix| format!("{ENV_API_KEY_PREFIX}{suffix}"))
}

/// Trim-and-drop-if-empty, so `export OUTLINE_URL=` behaves as unset.
fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Where the user config file was looked for.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigSource {
    /// The path, or `None` when the platform has no resolvable config
    /// directory (a headless account without a home directory).
    pub path: Option<PathBuf>,
    /// Whether the user named this path (`--config` / `OUTLINE_CONFIG`).
    /// An explicitly named file that is missing is an error; the default
    /// location being empty is not.
    pub explicit: bool,
}

impl fmt::Debug for ConfigSource {
    /// Manual impl: a config PATH is caller-supplied text that can carry a
    /// secret in a directory name, so Debug reports only whether one is
    /// set - see `redacted_path`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigSource")
            .field("path", &redacted_path(self.path.as_deref()))
            .field("explicit", &self.explicit)
            .finish()
    }
}

/// A loaded config file and the path it came from (`None` when no file was
/// read).
#[derive(Clone, Default)]
pub struct LoadedConfig {
    /// The parsed contents, empty when no file was read.
    pub file: ConfigFile,
    /// The file actually read, for error messages.
    pub path: Option<PathBuf>,
}

impl fmt::Debug for LoadedConfig {
    /// Manual impl: delegates to [`ConfigFile`], and reports the path only
    /// as set/unset - see `redacted_path`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedConfig")
            .field("file", &self.file)
            .field("path", &redacted_path(self.path.as_deref()))
            .finish()
    }
}

/// Resolved runtime configuration handed to the request channel.
#[derive(Clone)]
pub struct Config {
    /// Base URL of the Outline instance (e.g. `https://docs.example.com`).
    pub base_url: String,
    /// API key used as a bearer token.
    pub api_key: String,
}

impl fmt::Debug for Config {
    /// Manual impl: the API key must never appear in Debug output, and the
    /// base URL is reduced to its origin (`scheme://host[:port]`) - the
    /// only URL-derived form safe to display, since userinfo, query,
    /// fragment, and even the path may embed credentials. `Config` holds
    /// the raw configured value before validation; anything whose origin
    /// cannot be determined safely is redacted whole.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("base_url_origin", &redacted_origin(Some(&self.base_url)))
            .field("api_key", &REDACTED)
            .finish()
    }
}

/// Whether a profile name is set, without disclosing it.
///
/// Debug output is an unbounded machine surface (logs, panic messages,
/// error chains); a user who put a secret in a profile name must not have
/// it copied there. Names appear only in `Display`, where they are
/// sanitized and length-capped.
fn redacted_name(name: Option<&str>) -> &'static str {
    match name {
        Some(_) => REDACTED,
        None => "<unset>",
    }
}

/// Whether a config-file path is set, without disclosing it.
///
/// A path is caller-supplied text (`--config`, `OUTLINE_CONFIG`) and its
/// directory names can carry secrets, so it gets the same treatment as a
/// profile name: absent from Debug, and shown in `Display` only, where it is
/// sanitized and length-capped.
fn redacted_path(path: Option<&Path>) -> &'static str {
    match path {
        Some(_) => REDACTED,
        None => "<unset>",
    }
}

/// The origin (`scheme://host[:port]`) of a URL, or [`REDACTED`] when it
/// cannot be determined safely.
fn redacted_origin(url: Option<&str>) -> String {
    url.and_then(engine::base_url_origin)
        .unwrap_or_else(|| REDACTED.to_string())
}

/// Refuse a selected profile whose API key variable another profile shares.
///
/// Distinct names can map to one variable (`my-work` and `my.work` both give
/// `OUTLINE_API_KEY_MY_WORK`), which would make it ambiguous WHICH instance
/// that key belongs to - precisely the ambiguity profile-scoped credentials
/// exist to remove. Checked only for the selected profile, so an unrelated
/// collision elsewhere in the file does not block a command.
fn check_api_key_var_is_unambiguous(
    selected: &str,
    loaded: &LoadedConfig,
) -> Result<(), ConfigError> {
    let Some(variable) = api_key_var(selected) else {
        return Ok(()); // Reported by the token source, which knows the method.
    };
    let clash = loaded
        .file
        .profiles
        .keys()
        .filter(|name| name.as_str() != selected)
        .find(|name| api_key_var(name).as_deref() == Some(variable.as_str()));
    match clash {
        Some(other) => Err(ConfigError::AmbiguousProfileApiKeyVar {
            profile: selected.to_string(),
            other: other.clone(),
            variable,
        }),
        None => Ok(()),
    }
}

/// Look up a selected profile, or explain what does exist.
fn lookup_profile<'a>(
    name: &'a str,
    loaded: &'a LoadedConfig,
    from_file: bool,
) -> Result<(&'a str, &'a Profile), ConfigError> {
    match loaded.file.profiles.get_key_value(name) {
        Some((key, profile)) => Ok((key.as_str(), profile)),
        None => Err(ConfigError::UnknownProfile {
            name: (!from_file).then(|| name.to_string()),
            path: loaded.path.clone(),
            available: loaded.file.profiles.keys().cloned().collect(),
        }),
    }
}

impl Config {
    /// Pair resolved settings with a secret from a [`TokenSource`].
    pub fn from_parts(settings: &Settings, api_key: String) -> Self {
        Self {
            base_url: settings.base_url().to_string(),
            api_key,
        }
    }

    /// Pair already-resolved settings with the credential they select.
    ///
    /// Which store holds the key (the credential file or the environment)
    /// is decided by [`select_credential_source`] from the resolved
    /// settings. Either way the secret is obtained through
    /// [`release_token`], so the binding check applies to a stored
    /// credential exactly as it does to an environment variable.
    ///
    /// Settings are passed in rather than resolved here so that a caller
    /// which has already resolved them does not resolve them a second
    /// time: two reads of the same config file can disagree if it changes
    /// in between.
    pub fn release(
        settings: &Settings,
        env: &EnvLayer,
        stored: &StoredCredential<'_>,
    ) -> Result<Self, ConfigError> {
        let api_key = match credentials::select(settings, stored.is_present()) {
            credentials::Source::CredentialFile => release_token(stored, settings)?,
            credentials::Source::Environment => release_token(&EnvApiKey(env), settings)?,
        };
        Ok(Self::from_parts(settings, api_key))
    }

    /// Full resolution: flags over environment over user config file.
    ///
    /// `stored` is whatever the credential file holds for the selected
    /// profile. That file belongs to the `auth` module, which owns its
    /// hygiene, so its contents are handed in rather than read here.
    pub fn load(overrides: &Overrides, stored: &StoredCredential<'_>) -> Result<Self, ConfigError> {
        let env = EnvLayer::from_process();
        let loaded = load_file(overrides, &env)?;
        let settings = resolve_settings(overrides, &env, &loaded)?;
        Self::release(&settings, &env, stored)
    }
}
