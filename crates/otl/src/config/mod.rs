//! Runtime configuration: command-line flags over environment variables
//! over a single user config file (TOML, with named profiles).
//!
//! Precedence is **flag > env > config file, per key**: the env supplying a
//! base URL does not discard the selected profile's authentication method.
//! Every layer is captured as plain data ([`Overrides`], [`EnvLayer`],
//! [`ConfigFile`]) so that [`resolve_settings`] is a pure function - tests
//! never mutate the process environment and never read the real user file.
//!
//! Credentials are deliberately NOT part of this file. Resolution stops at
//! the base URL and the authentication *method*; the secret itself comes
//! from a [`TokenSource`], the single interface point where the credential
//! file (Epic 2) plugs in. A credential found in the config file is a hard
//! error: the config file is meant to be shareable and committable, the
//! credential file is not.
//!
//! # Credentials are scoped to their instance
//!
//! A profile names an INSTANCE, so the credential sent to it must belong to
//! that instance. The global `OUTLINE_API_KEY` is therefore used only when
//! no profile is in effect; a profile reads
//! `OUTLINE_API_KEY_<PROFILE>` and refuses to fall back, because falling
//! back would send one workspace's key to another workspace's server. See
//! [`EnvApiKey`].
//!
//! # Nothing from the config file is echoed back
//!
//! Two rules, because a user who wrongly puts a secret in the config file
//! must not see it again in a diagnostic, a log or a Debug rendering:
//!
//! - config-file diagnostics are built only from text this module owns plus
//!   a line number (see `file::parse_reason`); no parser-produced text
//!   reaches the output, since `toml`'s messages interpolate the offending
//!   value for unknown enum variants and type mismatches;
//! - every name that does reach a diagnostic goes through
//!   [`sanitize_name`], because a TOML quoted key can carry ESC and newline
//!   bytes straight into a terminal.

mod error;
mod file;

pub use error::{sanitize_name, ConfigError};
pub use file::{config_dir, default_config_path, load_file, load_from, locate, CONFIG_FILE_NAME};

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::PathBuf;

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

/// How a profile authenticates.
///
/// This is the *method*, not the secret: see [`TokenSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    /// Bearer API key (the Epic 1 path; `OUTLINE_API_KEY` in v1).
    #[default]
    #[serde(alias = "api_key", alias = "apikey")]
    ApiKey,
    /// Browser OAuth 2.0 (`otl auth login`, Epic 2).
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
    /// Manual impl: delegates to [`Profile`]'s redacting Debug and shows
    /// profile names as escaped strings, never raw.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigFile")
            .field("default_profile", &self.default_profile)
            .field("profiles", &self.profiles)
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
            .field("profile", &self.profile)
            .field("url_origin", &redacted_origin(self.url.as_deref()))
            .field("config_path", &self.config_path)
            .finish()
    }
}

/// The environment layer, captured as data.
#[derive(Clone, Default)]
pub struct EnvLayer {
    /// `OUTLINE_PROFILE`.
    pub profile: Option<String>,
    /// `OUTLINE_URL`.
    pub url: Option<String>,
    /// `OUTLINE_API_KEY`: the key used when NO profile is in effect.
    pub api_key: Option<String>,
    /// `OUTLINE_CONFIG`.
    pub config_path: Option<PathBuf>,
    /// Per-profile API keys, keyed by the variable suffix after
    /// [`ENV_API_KEY_PREFIX`] (so `OUTLINE_API_KEY_WORK` is stored under
    /// `WORK`). A profile's key is looked up here and nowhere else.
    pub profile_api_keys: BTreeMap<String, String>,
}

impl fmt::Debug for EnvLayer {
    /// Manual impl: the API key must never appear, and the base URL is
    /// reduced to its origin (userinfo, path, query and fragment can all
    /// carry credentials).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvLayer")
            .field("profile", &self.profile)
            .field("url_origin", &redacted_origin(self.url.as_deref()))
            .field("api_key", &REDACTED)
            .field("config_path", &self.config_path)
            // Count only: the suffixes name profiles, but the values are
            // keys, and a map rendering would print them.
            .field("profile_api_keys", &self.profile_api_keys.len())
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
            profile_api_keys: profile_api_keys_from_process(),
            ..Self::from_values(
                read(ENV_PROFILE).as_deref(),
                read(ENV_URL).as_deref(),
                read(ENV_API_KEY).as_deref(),
                // Not filtered on emptiness: `OUTLINE_CONFIG=` is the
                // documented way to say "read no config file at all"
                // (see [`locate`]).
                env::var_os(ENV_CONFIG).map(PathBuf::from),
            )
        }
    }

    /// The layer with one profile-scoped API key added, applying the same
    /// blank-is-unset rule as the process environment. Test seam.
    pub fn with_profile_api_key(mut self, profile: &str, api_key: &str) -> Self {
        if let (Some(suffix), Some(value)) = (api_key_var_suffix(profile), non_blank(Some(api_key)))
        {
            self.profile_api_keys.insert(suffix, value);
        }
        self
    }

    /// Build the layer from explicit values, applying the blank-is-unset
    /// rule. The seam that lets tests avoid mutating the environment.
    pub fn from_values(
        profile: Option<&str>,
        url: Option<&str>,
        api_key: Option<&str>,
        config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            profile: non_blank(profile),
            url: non_blank(url),
            api_key: non_blank(api_key),
            config_path,
            profile_api_keys: BTreeMap::new(),
        }
    }
}

/// Collect every `OUTLINE_API_KEY_*` variable from the process environment.
///
/// Blank values count as unset, matching every other variable.
fn profile_api_keys_from_process() -> BTreeMap<String, String> {
    env::vars()
        .filter_map(|(name, value)| {
            let suffix = name.strip_prefix(ENV_API_KEY_PREFIX)?;
            let value = non_blank(Some(&value))?;
            (!suffix.is_empty()).then(|| (suffix.to_string(), value))
        })
        .collect()
}

/// The `OUTLINE_API_KEY_*` variable suffix for a profile name.
///
/// ASCII alphanumerics are upper-cased and every other character becomes
/// `_`, so `work` -> `WORK` and `self-hosted` -> `SELF_HOSTED`. `None` when
/// the name has no ASCII alphanumeric at all and therefore cannot be
/// expressed as an environment variable name.
pub fn api_key_var_suffix(profile: &str) -> Option<String> {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    /// The path, or `None` when the platform has no resolvable config
    /// directory (a headless account without a home directory).
    pub path: Option<PathBuf>,
    /// Whether the user named this path (`--config` / `OUTLINE_CONFIG`).
    /// An explicitly named file that is missing is an error; the default
    /// location being empty is not.
    pub explicit: bool,
}

/// A loaded config file and the path it came from (`None` when no file was
/// read).
#[derive(Debug, Clone, Default)]
pub struct LoadedConfig {
    /// The parsed contents, empty when no file was read.
    pub file: ConfigFile,
    /// The file actually read, for error messages.
    pub path: Option<PathBuf>,
}

/// Everything resolved except the secret itself.
#[derive(Clone)]
pub struct Settings {
    /// Named profile in effect, if any.
    pub profile: Option<String>,
    /// Base URL of the Outline instance.
    pub base_url: String,
    /// How to authenticate to it.
    pub auth: AuthMethod,
}

impl fmt::Debug for Settings {
    /// Manual impl: same base-URL redaction rule as [`Config`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("profile", &self.profile)
            .field("base_url_origin", &redacted_origin(Some(&self.base_url)))
            .field("auth", &self.auth)
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

/// The origin (`scheme://host[:port]`) of a URL, or [`REDACTED`] when it
/// cannot be determined safely.
fn redacted_origin(url: Option<&str>) -> String {
    url.and_then(engine::base_url_origin)
        .unwrap_or_else(|| REDACTED.to_string())
}

/// Where the secret comes from.
///
/// The single interface point between profile resolution and credential
/// storage: v1 ships [`EnvApiKey`], and the Epic 2 credential file becomes a
/// second implementation without any change above this line.
pub trait TokenSource {
    /// The bearer token for `settings`, or a readable configuration error.
    fn token(&self, settings: &Settings) -> Result<String, ConfigError>;
}

/// The v1 token source: an API key from the environment.
///
/// A credential belongs to ONE instance, and a profile names an instance, so
/// the two are resolved from the same scope:
///
/// - no profile in effect: the global `OUTLINE_API_KEY` (the Epic 1 path,
///   unchanged);
/// - profile in effect: `OUTLINE_API_KEY_<PROFILE>` and nothing else.
///
/// The second rule deliberately does NOT fall back to the global variable.
/// Falling back is what would send the key for the workspace whose variable
/// happens to be exported to whichever instance the selected profile points
/// at - a silent cross-origin credential disclosure produced by nothing more
/// than `--profile`. Refusing is recoverable (the error names the variable to
/// set); a key already sent to the wrong server is not.
pub struct EnvApiKey<'layer>(pub &'layer EnvLayer);

impl TokenSource for EnvApiKey<'_> {
    fn token(&self, settings: &Settings) -> Result<String, ConfigError> {
        if settings.auth != AuthMethod::ApiKey {
            return Err(ConfigError::UnsupportedAuthMethod {
                profile: settings.profile.clone(),
                method: settings.auth,
            });
        }
        let Some(profile) = settings.profile.as_deref() else {
            return self.0.api_key.clone().ok_or(ConfigError::MissingApiKey);
        };
        let Some(suffix) = api_key_var_suffix(profile) else {
            return Err(ConfigError::ProfileApiKeyVarUnnameable {
                profile: profile.to_string(),
            });
        };
        self.0
            .profile_api_keys
            .get(&suffix)
            .cloned()
            .ok_or_else(|| ConfigError::MissingProfileApiKey {
                profile: profile.to_string(),
                variable: format!("{ENV_API_KEY_PREFIX}{suffix}"),
                global_set: self.0.api_key.is_some(),
            })
    }
}

/// Resolve everything but the secret, applying flag > env > file per key.
pub fn resolve_settings(
    overrides: &Overrides,
    env: &EnvLayer,
    loaded: &LoadedConfig,
) -> Result<Settings, ConfigError> {
    let (requested, from_file) = match overrides.profile.as_deref().or(env.profile.as_deref()) {
        Some(name) => (Some(name), false),
        // A name read from the config file is FILE CONTENT, so it is never
        // echoed back (see the module docs); one the user just typed is not.
        None => (loaded.file.default_profile.as_deref(), true),
    };
    let profile = match requested {
        Some(name) => Some(lookup_profile(name, loaded, from_file)?),
        None => None,
    };
    if let Some((name, _)) = profile {
        check_api_key_var_is_unambiguous(name, loaded)?;
    }
    let base_url = overrides
        .url
        .clone()
        .or_else(|| env.url.clone())
        .or_else(|| profile.and_then(|(_, p)| non_blank(p.url.as_deref())))
        .ok_or_else(|| ConfigError::MissingUrl {
            profile: profile.map(|(name, _)| name.to_string()),
        })?;
    Ok(Settings {
        profile: profile.map(|(name, _)| name.to_string()),
        base_url,
        auth: profile.and_then(|(_, p)| p.auth).unwrap_or_default(),
    })
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

/// Whether an environment base URL points somewhere other than the selected
/// profile does.
///
/// Not an error - precedence is flag > env > file for every key - but it
/// means the profile's credential is about to be sent to an instance the
/// profile did not name, so it is worth saying out loud. `--url` is the
/// deliberate way to redirect a profile and is therefore not reported.
pub fn env_url_shadows_profile<'a>(
    overrides: &Overrides,
    env: &'a EnvLayer,
    loaded: &LoadedConfig,
    settings: &Settings,
) -> Option<&'a str> {
    if overrides.url.is_some() {
        return None;
    }
    let env_url = env.url.as_deref()?;
    let profile = loaded.file.profiles.get(settings.profile.as_deref()?)?;
    let declared = non_blank(profile.url.as_deref())?;
    (declared != env_url).then_some(env_url)
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
            base_url: settings.base_url.clone(),
            api_key,
        }
    }

    /// Full resolution: flags over environment over user config file.
    pub fn load(overrides: &Overrides) -> Result<Self, ConfigError> {
        let env = EnvLayer::from_process();
        let loaded = load_file(overrides, &env)?;
        let settings = resolve_settings(overrides, &env, &loaded)?;
        if env_url_shadows_profile(overrides, &env, &loaded, &settings).is_some() {
            // Diagnostics go to stderr; the origin is not printed, since a
            // base URL can carry credentials in its own path or query.
            crate::stdio::write_diagnostic_line(&format!(
                "warning: {ENV_URL} points at a different instance than profile {:?} \
                 declares, so that profile's credential is being sent there; \
                 unset {ENV_URL}, or pass --url to redirect it deliberately",
                settings
                    .profile
                    .as_deref()
                    .map(sanitize_name)
                    .unwrap_or_default()
            ));
        }
        let api_key = EnvApiKey(&env).token(&settings)?;
        Ok(Self::from_parts(&settings, api_key))
    }
}
