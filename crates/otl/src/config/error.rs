//! Configuration errors and the rules for what a diagnostic may contain.
//!
//! Two invariants hold for every message in this module:
//!
//! - no value from the config file is echoed, because a user who wrongly put
//!   a credential there must not see it again in a diagnostic, a log or a
//!   Debug rendering;
//! - every NAME or PATH that is echoed goes through [`sanitize_name`] or
//!   [`sanitize_path`] first, because a TOML quoted key and a `--config`
//!   argument can both carry ESC, BEL and newline bytes straight into a
//!   terminal, where they forge hyperlinks, retitle the window or fake an
//!   additional `error:` line.
//!
//! `Debug` deliberately does not render these fields at all: it forwards to
//! `Display`, which is the only rendering that has passed both rules.

use std::fmt;
use std::path::{Path, PathBuf};

use super::{
    AuthMethod, CONFIG_FILE_NAME, CREDENTIALS_FILE_NAME, ENV_API_KEY, ENV_API_KEY_PREFIX, ENV_URL,
    MAX_PROFILE_VAR_CHARS,
};

/// Maximum number of characters kept from any name echoed in a diagnostic.
const MAX_NAME_CHARS: usize = 64;
/// Replacement for a control character in a name echoed in a diagnostic.
const CONTROL_PLACEHOLDER: char = '\u{fffd}';
/// Config-file keys that would hold a credential. Trapped structurally, so
/// the value is never even deserialized.
const CREDENTIAL_KEYS: &str = "`api_key` / `token`";
/// Maximum number of profile names listed in one diagnostic.
const MAX_LISTED_PROFILES: usize = 20;
/// Maximum number of characters kept from a path echoed in a diagnostic.
///
/// Larger than [`MAX_NAME_CHARS`]: a legitimate path is much longer than a
/// legitimate profile name, and the point here is a bound, not brevity.
const MAX_PATH_CHARS: usize = 200;

/// or forge additional diagnostic lines on stderr. Applied even when stderr
/// is not a terminal, because the consumer may be one.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() {
                CONTROL_PLACEHOLDER
            } else {
                c
            }
        })
        .take(MAX_NAME_CHARS)
        .collect();
    if name.chars().count() > MAX_NAME_CHARS {
        format!("{cleaned}...")
    } else {
        cleaned
    }
}

/// Make a filesystem path safe to print in a diagnostic.
///
/// `Path::display()` is lossy for non-UTF-8 but passes control bytes through
/// unchanged, and a path is caller-controlled (`--config`,
/// `OUTLINE_CONFIG`). Without this, `--config` carrying an OSC 8 sequence
/// plants a clickable hyperlink in the user's terminal, and an embedded
/// newline lets it forge a second `error:` line.
pub fn sanitize_path(path: &Path) -> String {
    let shown = path.display().to_string();
    let cleaned: String = shown
        .chars()
        .map(|c| {
            if c.is_control() {
                CONTROL_PLACEHOLDER
            } else {
                c
            }
        })
        .take(MAX_PATH_CHARS)
        .collect();
    if shown.chars().count() > MAX_PATH_CHARS {
        format!("{cleaned}...")
    } else {
        cleaned
    }
}

/// Configuration errors. Always reported before any network request.
#[derive(Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// No base URL from any layer.
    MissingUrl {
        /// The profile in effect, when one was selected.
        profile: Option<String>,
    },
    /// `OUTLINE_API_KEY` is unset or empty, with no profile in effect.
    MissingApiKey,
    /// A profile is in effect but its own API key variable is unset.
    MissingProfileApiKey {
        /// The profile in effect.
        profile: String,
        /// The variable that must hold its key.
        variable: String,
        /// Whether the global `OUTLINE_API_KEY` is set - deliberately not
        /// used here, and worth saying so.
        global_set: bool,
    },
    /// A profile name cannot be expressed as an environment variable name.
    ProfileApiKeyVarUnnameable {
        /// The profile in effect.
        profile: String,
    },
    /// `OUTLINE_URL` disagrees with the selected profile's own base URL.
    ///
    /// Refused rather than resolved: the profile decides which credential is
    /// sent, so it must also decide where. Neither answer is safe to pick
    /// silently - honouring the variable sends the profile's key to a server
    /// the profile never named, and ignoring it discards configuration
    /// without saying so.
    ConflictingUrl {
        /// The profile in effect.
        profile: String,
    },
    /// Two profiles map to the same API key variable.
    AmbiguousProfileApiKeyVar {
        /// The selected profile.
        profile: String,
        /// The other profile sharing its variable.
        other: String,
        /// The shared variable name.
        variable: String,
    },
    /// A profile was selected that the config file does not define.
    UnknownProfile {
        /// The requested name, when the user supplied it directly. `None`
        /// when it came from the config file's `default_profile`, whose
        /// value is file content and therefore never echoed.
        name: Option<String>,
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
            Self::MissingProfileApiKey {
                profile,
                variable,
                global_set,
            } => write_missing_profile_key(f, profile, variable, *global_set),
            Self::ProfileApiKeyVarUnnameable { profile } => write_unnameable_var(f, profile),
            Self::ConflictingUrl { profile } => write_conflicting_url(f, profile),
            Self::AmbiguousProfileApiKeyVar {
                profile,
                other,
                variable,
            } => write_ambiguous_var(f, profile, other, variable),
            Self::UnknownProfile {
                name,
                path,
                available,
            } => write_unknown_profile(f, name.as_deref(), path.as_deref(), available),
            Self::ConfigFileUnreadable { path, reason } => write!(
                f,
                "cannot read the user config file {}: {reason}",
                sanitize_path(path)
            ),
            Self::MalformedConfigFile { path, reason } => write!(
                f,
                "the user config file {} is not valid: {reason}",
                sanitize_path(path)
            ),
            Self::CredentialInConfigFile { path, location } => write!(
                f,
                "the user config file {} sets {CREDENTIAL_KEYS} at {location}.\n\
                 Credentials never live in the config file (which is meant to be \
                 shareable); they belong in {CREDENTIALS_FILE_NAME} beside it, or in \
                 {ENV_API_KEY}. Remove the key and re-run.",
                sanitize_path(path)
            ),
            Self::UnsupportedAuthMethod { profile, method } => {
                write_unsupported_auth(f, profile.as_deref(), *method)
            }
        }
    }
}

fn write_unnameable_var(f: &mut fmt::Formatter<'_>, profile: &str) -> fmt::Result {
    write!(
        f,
        "profile {:?} has no {ENV_API_KEY_PREFIX}* variable to read its API \
         key from: a profile name must contain at least one ASCII letter or \
         digit and be at most {MAX_PROFILE_VAR_CHARS} characters.\n\
         Rename the profile, or drop --profile and use --url with \
         {ENV_API_KEY}.",
        sanitize_name(profile)
    )
}

fn write_conflicting_url(f: &mut fmt::Formatter<'_>, profile: &str) -> fmt::Result {
    // Neither URL is printed: a base URL can carry credentials in its
    // userinfo, path or query.
    write!(
        f,
        "{ENV_URL} points at a different instance than profile {:?} declares, \
         so it is unclear where that profile's API key should be sent - and \
         sending it to the wrong instance cannot be undone.\n\
         Unset {ENV_URL}, drop --profile to use {ENV_URL} on its own, or pass \
         --url to redirect this profile deliberately.",
        sanitize_name(profile)
    )
}

fn write_ambiguous_var(
    f: &mut fmt::Formatter<'_>,
    profile: &str,
    other: &str,
    variable: &str,
) -> fmt::Result {
    write!(
        f,
        "profiles {:?} and {:?} both map to {variable}, so it is ambiguous \
         which instance that API key belongs to.\n\
         Rename one of them so each profile has its own variable.",
        sanitize_name(profile),
        sanitize_name(other)
    )
}

fn write_missing_url(f: &mut fmt::Formatter<'_>, profile: Option<&str>) -> fmt::Result {
    if let Some(profile) = profile {
        // Sanitized, and quoted rather than interpolated into a TOML header,
        // so a name carrying control bytes cannot reach the terminal raw.
        let profile = sanitize_name(profile);
        write!(
            f,
            "profile {profile:?} has no base URL, and neither --url nor \
             {ENV_URL} supplied one.\n\
             Add `url = \"https://docs.example.com\"` under that profile in \
             {CONFIG_FILE_NAME}, or set {ENV_URL}."
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
    name: Option<&str>,
    path: Option<&Path>,
    available: &[String],
) -> fmt::Result {
    match name {
        Some(name) => write!(f, "unknown profile {:?}: ", sanitize_name(name))?,
        // The name came from the config file's `default_profile`; its value
        // is file content, which is never echoed.
        None => write!(
            f,
            "the config file's `default_profile` names a profile it does not define: "
        )?,
    }
    match path {
        Some(path) if available.is_empty() => write!(
            f,
            "the user config file {} defines no profiles",
            sanitize_path(path)
        ),
        Some(path) => write!(
            f,
            "the user config file {} defines {}",
            sanitize_path(path),
            profile_list(available)
        ),
        None => write!(
            f,
            "there is no user config file to define it. Create one (see \
             {CONFIG_FILE_NAME} in your config directory) or drop --profile"
        ),
    }
}

/// Render the defined profile names for a diagnostic: each sanitized and
/// quoted, and the list itself capped so a config file with hundreds of
/// profiles cannot flood stderr.
fn profile_list(available: &[String]) -> String {
    let mut listed: Vec<String> = available
        .iter()
        .take(MAX_LISTED_PROFILES)
        .map(|name| format!("{:?}", sanitize_name(name)))
        .collect();
    if available.len() > MAX_LISTED_PROFILES {
        listed.push(format!(
            "and {} more",
            available.len() - MAX_LISTED_PROFILES
        ));
    }
    listed.join(", ")
}

fn write_missing_profile_key(
    f: &mut fmt::Formatter<'_>,
    profile: &str,
    variable: &str,
    global_set: bool,
) -> fmt::Result {
    write!(
        f,
        "profile {:?} has no API key: {variable} is not set.\n\
         Set it to the key for THAT instance, for example:\n\
         \x20 export {variable}=<key-for-this-instance>",
        sanitize_name(profile)
    )?;
    if global_set {
        write!(
            f,
            "\n{ENV_API_KEY} is set but is deliberately not used with --profile: \
             sending it would give one workspace's key to another workspace's \
             server. Drop --profile to use it."
        )?;
    }
    Ok(())
}

fn write_unsupported_auth(
    f: &mut fmt::Formatter<'_>,
    profile: Option<&str>,
    method: AuthMethod,
) -> fmt::Result {
    let subject = match profile {
        Some(name) => format!("profile {:?}", sanitize_name(name)),
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

impl fmt::Debug for ConfigError {
    /// Manual impl: forwards to `Display`.
    ///
    /// The derived Debug would print the raw fields, which is exactly what
    /// the two rules in the module docs forbid: profile names and paths
    /// unsanitized, and (via `available`) a whole list of them. `Display` is
    /// the only rendering that has passed both, so Debug reuses it rather
    /// than opening a second, unchecked surface - Debug is the one that ends
    /// up in logs, panic messages and error chains.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConfigError({self})")
    }
}

impl std::error::Error for ConfigError {}
