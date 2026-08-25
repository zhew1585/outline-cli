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

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::de::{Deserializer, IgnoredAny};
use serde::Deserialize;

/// Environment variable holding the Outline instance base URL.
pub const ENV_URL: &str = "OUTLINE_URL";
/// Environment variable holding the API key.
pub const ENV_API_KEY: &str = "OUTLINE_API_KEY";
/// Environment variable selecting a named profile.
pub const ENV_PROFILE: &str = "OUTLINE_PROFILE";
/// Environment variable overriding the user config file location.
pub const ENV_CONFIG: &str = "OUTLINE_CONFIG";

/// File name of the user config file inside the config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// File name of the credential file, which the auth layer owns. Named here
/// only so that error messages can point at it: no credential is ever read
/// from, or written to, the config file.
pub const CREDENTIALS_FILE_NAME: &str = "credentials.toml";

/// Application directory name under the platform config root.
const APP_DIR_NAME: &str = "outline-cli";
/// Placeholder shown instead of secrets in Debug output.
const REDACTED: &str = "***";
/// Maximum accepted size of the user config file. A config file is a few
/// hundred bytes; anything larger is a mistake (or a device/pipe), and it
/// is read into memory, so the read is bounded.
const MAX_CONFIG_FILE_BYTES: u64 = 64 * 1024;
/// Maximum number of characters kept from a TOML parse diagnostic.
const MAX_PARSE_REASON_CHARS: usize = 200;
/// Config-file keys that would hold a credential. Trapped structurally, so
/// the value is never even deserialized.
const CREDENTIAL_KEYS: &str = "`api_key` / `token`";

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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

/// The parsed user config file.
///
/// Unknown keys are rejected rather than ignored: a typo in a config file
/// that silently does nothing is worse than a readable error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
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

/// Command-line overrides: the highest-precedence layer.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// `--profile NAME`.
    pub profile: Option<String>,
    /// `--url URL`.
    pub url: Option<String>,
    /// `--config FILE`.
    pub config_path: Option<PathBuf>,
}

/// The environment layer, captured as data.
#[derive(Clone, Default)]
pub struct EnvLayer {
    /// `OUTLINE_PROFILE`.
    pub profile: Option<String>,
    /// `OUTLINE_URL`.
    pub url: Option<String>,
    /// `OUTLINE_API_KEY`.
    pub api_key: Option<String>,
    /// `OUTLINE_CONFIG`.
    pub config_path: Option<PathBuf>,
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
        Self::from_values(
            read(ENV_PROFILE).as_deref(),
            read(ENV_URL).as_deref(),
            read(ENV_API_KEY).as_deref(),
            // Not filtered on emptiness: `OUTLINE_CONFIG=` is the documented
            // way to say "read no config file at all" (see [`locate`]).
            env::var_os(ENV_CONFIG).map(PathBuf::from),
        )
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
        }
    }
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

/// The v1 token source: the API key from `OUTLINE_API_KEY`.
pub struct EnvApiKey<'layer>(pub &'layer EnvLayer);

impl TokenSource for EnvApiKey<'_> {
    fn token(&self, settings: &Settings) -> Result<String, ConfigError> {
        match settings.auth {
            AuthMethod::ApiKey => self.0.api_key.clone().ok_or(ConfigError::MissingApiKey),
            AuthMethod::Oauth => Err(ConfigError::UnsupportedAuthMethod {
                profile: settings.profile.clone(),
                method: settings.auth,
            }),
        }
    }
}

/// Configuration errors. Always reported before any network request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// No base URL from any layer.
    MissingUrl {
        /// The profile in effect, when one was selected.
        profile: Option<String>,
    },
    /// `OUTLINE_API_KEY` is unset or empty.
    MissingApiKey,
    /// A profile was selected that the config file does not define.
    UnknownProfile {
        /// The requested name.
        name: String,
        /// The config file consulted, if any was read.
        path: Option<PathBuf>,
        /// The profiles that do exist.
        available: Vec<String>,
    },
    /// The config file could not be read (missing when named explicitly,
    /// unreadable, or too large).
    ConfigFileUnreadable {
        /// The path that was tried.
        path: PathBuf,
        /// Value-free reason.
        reason: String,
    },
    /// The config file is not valid TOML, or does not match the schema.
    MalformedConfigFile {
        /// The path that was parsed.
        path: PathBuf,
        /// Value-free reason (location and kind only).
        reason: String,
    },
    /// The config file holds something that looks like a credential.
    CredentialInConfigFile {
        /// The path that was parsed.
        path: PathBuf,
        /// Where in the file (`top level` or a profile name).
        location: String,
    },
    /// The resolved authentication method has no implementation yet.
    UnsupportedAuthMethod {
        /// The profile in effect, when one was selected.
        profile: Option<String>,
        /// The method that was asked for.
        method: AuthMethod,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUrl { profile } => write_missing_url(f, profile.as_deref()),
            Self::MissingApiKey => write!(
                f,
                "{ENV_API_KEY} is not set.\n\
                 Create an API key in Outline (Settings -> API) and set it, for example:\n\
                 \x20 export {ENV_API_KEY}=<your-api-key>"
            ),
            Self::UnknownProfile {
                name,
                path,
                available,
            } => write_unknown_profile(f, name, path.as_deref(), available),
            Self::ConfigFileUnreadable { path, reason } => write!(
                f,
                "cannot read the user config file {}: {reason}",
                path.display()
            ),
            Self::MalformedConfigFile { path, reason } => write!(
                f,
                "the user config file {} is not valid: {reason}",
                path.display()
            ),
            Self::CredentialInConfigFile { path, location } => write!(
                f,
                "the user config file {} sets {CREDENTIAL_KEYS} at {location}.\n\
                 Credentials never live in the config file (which is meant to be \
                 shareable); they belong in {CREDENTIALS_FILE_NAME} beside it, or in \
                 {ENV_API_KEY}. Remove the key and re-run.",
                path.display()
            ),
            Self::UnsupportedAuthMethod { profile, method } => {
                write_unsupported_auth(f, profile.as_deref(), *method)
            }
        }
    }
}

fn write_missing_url(f: &mut fmt::Formatter<'_>, profile: Option<&str>) -> fmt::Result {
    if let Some(profile) = profile {
        write!(
            f,
            "profile {profile:?} has no base URL, and neither --url nor \
             {ENV_URL} supplied one.\n\
             Add `url = \"https://docs.example.com\"` under \
             [profiles.{profile}] in {CONFIG_FILE_NAME}, or set {ENV_URL}."
        )
    } else {
        write!(
            f,
            "{ENV_URL} is not set.\n\
             Set it to your Outline instance base URL, for example:\n\
             \x20 export {ENV_URL}=https://docs.example.com\n\
             Or define a profile in {CONFIG_FILE_NAME} and select it with --profile."
        )
    }
}

fn write_unknown_profile(
    f: &mut fmt::Formatter<'_>,
    name: &str,
    path: Option<&Path>,
    available: &[String],
) -> fmt::Result {
    write!(f, "unknown profile {name:?}: ")?;
    match path {
        Some(path) if available.is_empty() => write!(
            f,
            "the user config file {} defines no profiles",
            path.display()
        ),
        Some(path) => write!(
            f,
            "the user config file {} defines {}",
            path.display(),
            available.join(", ")
        ),
        None => write!(
            f,
            "there is no user config file to define it. Create one (see \
             {CONFIG_FILE_NAME} in your config directory) or drop --profile"
        ),
    }
}

fn write_unsupported_auth(
    f: &mut fmt::Formatter<'_>,
    profile: Option<&str>,
    method: AuthMethod,
) -> fmt::Result {
    let subject = match profile {
        Some(name) => format!("profile {name:?}"),
        None => "the configuration".to_string(),
    };
    write!(
        f,
        "{subject} selects `{method}` authentication, which this build cannot \
         use yet.\n\
         Set `auth = \"{}\"` for it (or remove the key) and provide the key \
         via {ENV_API_KEY}.",
        AuthMethod::ApiKey
    )
}

impl std::error::Error for ConfigError {}

/// The platform config directory for `otl`, resolved via `directories`.
///
/// Never assume a Unix layout: this is
/// `~/.config/outline-cli` on Linux, `~/Library/Application
/// Support/outline-cli` on macOS and `%APPDATA%\outline-cli\config` on
/// Windows. `None` when no home directory can be determined.
pub fn config_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", APP_DIR_NAME).map(|dirs| dirs.config_dir().to_path_buf())
}

/// Default path of the user config file.
pub fn default_config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(CONFIG_FILE_NAME))
}

/// Resolve which config file to read: `--config`, then `OUTLINE_CONFIG`,
/// then the platform default.
///
/// An EMPTY value (`--config ""`, `OUTLINE_CONFIG=`) explicitly disables the
/// config file, which is how a script or a test pins itself to environment
/// variables alone regardless of what the invoking user has configured.
pub fn locate(overrides: &Overrides, env: &EnvLayer) -> ConfigSource {
    let named = overrides
        .config_path
        .clone()
        .or_else(|| env.config_path.clone());
    match named {
        Some(path) if path.as_os_str().is_empty() => ConfigSource {
            path: None,
            explicit: true,
        },
        Some(path) => ConfigSource {
            path: Some(path),
            explicit: true,
        },
        None => ConfigSource {
            path: default_config_path(),
            explicit: false,
        },
    }
}

/// Locate and load the user config file.
pub fn load_file(overrides: &Overrides, env: &EnvLayer) -> Result<LoadedConfig, ConfigError> {
    load_from(&locate(overrides, env))
}

/// Load the config file at `source`.
///
/// An absent file at the DEFAULT location yields an empty config: the
/// env-only path must keep working on a fresh machine. A file the user named
/// explicitly must exist.
pub fn load_from(source: &ConfigSource) -> Result<LoadedConfig, ConfigError> {
    let Some(path) = source.path.as_deref() else {
        return Ok(LoadedConfig::default());
    };
    let raw = match read_capped(path) {
        Ok(raw) => raw,
        Err(error) if !source.explicit && error.is_not_found() => {
            return Ok(LoadedConfig::default())
        }
        Err(error) => return Err(error.into_config_error(path)),
    };
    let file = parse_config(&raw, path)?;
    Ok(LoadedConfig {
        file,
        path: Some(path.to_path_buf()),
    })
}

/// Parse and validate config-file text.
fn parse_config(raw: &str, path: &Path) -> Result<ConfigFile, ConfigError> {
    let file: ConfigFile =
        toml::from_str(raw).map_err(|error| ConfigError::MalformedConfigFile {
            path: path.to_path_buf(),
            reason: parse_reason(&error, raw),
        })?;
    validate(&file, path)?;
    Ok(file)
}

/// Reject credentials and unusable profile names.
fn validate(file: &ConfigFile, path: &Path) -> Result<(), ConfigError> {
    let credential = |location: &str| ConfigError::CredentialInConfigFile {
        path: path.to_path_buf(),
        location: location.to_string(),
    };
    if file.api_key.is_some() || file.token.is_some() {
        return Err(credential("the top level"));
    }
    for (name, profile) in &file.profiles {
        if profile.api_key.is_some() || profile.token.is_some() {
            return Err(credential(&format!("profile {name:?}")));
        }
        if name.trim().is_empty() {
            return Err(ConfigError::MalformedConfigFile {
                path: path.to_path_buf(),
                reason: "a profile name must not be empty".to_string(),
            });
        }
    }
    Ok(())
}

/// A value-free description of a TOML error: its location and its kind.
///
/// The error's `Display` renders an annotated source SNIPPET, and a user who
/// wrongly put a secret in the config file must not see it echoed back in a
/// message, a log or a Debug rendering. `message()` carries the parser's own
/// wording only, and the location is computed from the span ourselves.
fn parse_reason(error: &toml::de::Error, raw: &str) -> String {
    let location = match error.span() {
        Some(span) => format!("line {}: ", line_at(raw, span.start)),
        None => String::new(),
    };
    let reason: String = error
        .message()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_PARSE_REASON_CHARS)
        .collect();
    format!("{location}{reason}")
}

/// 1-based line number of a byte offset inside `raw`.
fn line_at(raw: &str, offset: usize) -> usize {
    let end = offset.min(raw.len());
    1 + raw[..end].matches('\n').count()
}

/// Read a file, refusing anything over [`MAX_CONFIG_FILE_BYTES`].
fn read_capped(path: &Path) -> Result<String, ReadError> {
    let file = File::open(path).map_err(ReadError::Io)?;
    let metadata = file.metadata().map_err(ReadError::Io)?;
    if metadata.is_file() && metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(ReadError::TooLarge);
    }
    let mut raw = String::new();
    let read = file
        .take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(ReadError::Io)?;
    if read as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(ReadError::TooLarge);
    }
    Ok(raw)
}

/// Why a config-file read failed.
enum ReadError {
    Io(std::io::Error),
    TooLarge,
}

impl ReadError {
    fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
    }

    /// Value-free rendering: the OS error KIND, never its message (which can
    /// embed the path or system locale text).
    fn into_config_error(self, path: &Path) -> ConfigError {
        let reason = match self {
            Self::Io(error) => error.kind().to_string(),
            Self::TooLarge => {
                format!("the file is too large (the limit is {MAX_CONFIG_FILE_BYTES} bytes)")
            }
        };
        ConfigError::ConfigFileUnreadable {
            path: path.to_path_buf(),
            reason,
        }
    }
}

/// Resolve everything but the secret, applying flag > env > file per key.
pub fn resolve_settings(
    overrides: &Overrides,
    env: &EnvLayer,
    loaded: &LoadedConfig,
) -> Result<Settings, ConfigError> {
    let requested = overrides
        .profile
        .as_deref()
        .or(env.profile.as_deref())
        .or(loaded.file.default_profile.as_deref());
    let profile = match requested {
        Some(name) => Some(lookup_profile(name, loaded)?),
        None => None,
    };
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

/// Look up a selected profile, or explain what does exist.
fn lookup_profile<'a>(
    name: &'a str,
    loaded: &'a LoadedConfig,
) -> Result<(&'a str, &'a Profile), ConfigError> {
    match loaded.file.profiles.get_key_value(name) {
        Some((key, profile)) => Ok((key.as_str(), profile)),
        None => Err(ConfigError::UnknownProfile {
            name: name.to_string(),
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
        let api_key = EnvApiKey(&env).token(&settings)?;
        Ok(Self::from_parts(&settings, api_key))
    }
}
