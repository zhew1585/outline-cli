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
//! `Debug` renders NO field content at all - only the variant name and
//! non-textual scalars - because `Display` has to name the profile the
//! user must fix, and a name must not reach the unbounded surface that
//! lands in logs, panics and error chains.
//!
//! `Display` is bounded for ANY construction of these variants, not
//! just the ones this crate builds: every string field is passed through a
//! sanitizer with a length cap on the way out, because the variants are
//! public and their fields can hold arbitrary text.

use std::fmt;
use std::path::{Path, PathBuf};

use super::{
    AuthMethod, ProfileSource, CONFIG_FILE_NAME, CREDENTIALS_FILE_NAME, ENV_API_KEY,
    ENV_API_KEY_PREFIX, ENV_URL, MAX_PROFILE_VAR_CHARS,
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
/// Maximum number of characters kept from any other free-form text field.
const MAX_TEXT_CHARS: usize = 300;

/// Whether a character must not be forwarded into a diagnostic.
///
/// Every category from [`crate::text`], with no exceptions and no
/// per-category distinction: a diagnostic is the one surface where making
/// tampering visible matters more than preserving the text, so each one is
/// replaced with a visible marker rather than dropped or kept. A category
/// added later is covered automatically, because this asks whether the
/// character is classified at all.
fn is_unsafe_in_diagnostic(c: char) -> bool {
    crate::text::hazard(c).is_some()
}

/// Replace every unsafe character and cap the length.
fn scrub(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if is_unsafe_in_diagnostic(c) {
                CONTROL_PLACEHOLDER
            } else {
                c
            }
        })
        .take(max_chars)
        .collect();
    if text.chars().count() > max_chars {
        format!("{cleaned}...")
    } else {
        cleaned
    }
}

/// Make a name from the config file or the command line safe to print in a
/// diagnostic.
///
/// A TOML quoted key and a `--profile` argument are both caller-controlled
/// text, and can carry ESC, BEL, bidi overrides or zero-width characters -
/// enough to recolour the terminal, retitle its window, reverse the visual
/// order of the rest of the message, or forge additional diagnostic lines on
/// stderr. Applied even when stderr is not a terminal, because the consumer
/// may be one. The result is also length-capped, so a name cannot flood the
/// output.
pub fn sanitize_name(name: &str) -> String {
    scrub(name, MAX_NAME_CHARS)
}

/// Make free-form text safe to print in a diagnostic.
///
/// Applies to the string fields of this crate's own error variants too: they
/// are public, so their contents are not guaranteed to be anything.
pub fn sanitize_text(text: &str) -> String {
    scrub(text, MAX_TEXT_CHARS)
}

/// Make a filesystem path safe to print in a diagnostic.
///
/// `Path::display()` is lossy for non-UTF-8 but passes control bytes through
/// unchanged, and a path is caller-controlled (`--config`,
/// `OUTLINE_CONFIG`). Without this, `--config` carrying an OSC 8 sequence
/// plants a clickable hyperlink in the user's terminal, and an embedded
/// newline lets it forge a second `error:` line.
pub fn sanitize_path(path: &Path) -> String {
    scrub(&path.display().to_string(), MAX_PATH_CHARS)
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
        /// How the profile was selected, so the advice names an action the
        /// user can actually take.
        source: ProfileSource,
    },
    /// A profile name cannot be expressed as an environment variable name.
    ProfileApiKeyVarUnnameable {
        /// The profile in effect.
        profile: String,
    },
    /// A profile is in effect, the base URL came from `OUTLINE_URL`, and the
    /// profile declares no `url` of its own - so there is nothing to bind
    /// the profile's credential to.
    UnboundProfileCredential {
        /// The profile in effect.
        profile: String,
    },
    /// The selected profile's own `url` is not a usable base URL, so no
    /// binding between its credential and an instance can be established.
    InvalidProfileUrl {
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
        /// How the profile was selected, so the advice names an action the
        /// user can actually take.
        source: ProfileSource,
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
    /// The credential file holds nothing for the selected profile under the
    /// resolved authentication method.
    ///
    /// Distinct from [`Self::MissingApiKey`]: that one means "no variable is
    /// exported", this one means "the file that `otl auth` writes has no
    /// entry here", and the remedy is a different command.
    MissingStoredCredential {
        /// Selected profile, when one is in effect.
        profile: Option<String>,
        /// Which authentication method was resolved.
        method: AuthMethod,
        /// Absolute path of the credential file.
        path: PathBuf,
    },

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
            Self::MissingApiKey => write_missing_api_key(f),
            Self::MissingProfileApiKey {
                profile,
                variable,
                global_set,
                source,
            } => write_missing_profile_key(f, profile, variable, *global_set, *source),
            Self::ProfileApiKeyVarUnnameable { profile } => write_unnameable_var(f, profile),
            Self::ConflictingUrl { profile, source } => write_conflicting_url(f, profile, *source),
            Self::InvalidProfileUrl { profile } => write_invalid_profile_url(f, profile),
            Self::UnboundProfileCredential { profile } => write_unbound_credential(f, profile),
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
            Self::ConfigFileUnreadable { path, reason } => {
                write_file_problem(f, "cannot read the user config file", path, reason)
            }
            Self::MalformedConfigFile { path, reason } => write_malformed_file(f, path, reason),
            Self::CredentialInConfigFile { path, location } => {
                write_credential_in_file(f, path, location)
            }
            Self::MissingStoredCredential {
                profile,
                method,
                path,
            } => write_missing_stored(f, profile.as_deref(), *method, path),
            Self::UnsupportedAuthMethod { profile, method } => {
                write_unsupported_auth(f, profile.as_deref(), *method)
            }
        }
    }
}

/// "There is no credential to use."
///
/// Names all THREE ways to supply one, not just the variable: two of them
/// (`otl auth login`, `otl auth set-key`) put the credential in the
/// owner-only credential file, which is the better place for it, and a user
/// told only about the variable would export a long-lived key into their
/// shell environment for want of knowing the alternative.
fn write_missing_api_key(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
        f,
        "no credentials are available.\n\
         Sign in with a browser:\n\
         \x20 otl auth login\n\
         or store an API key (Settings -> API in Outline):\n\
         \x20 otl auth set-key\n\
         or, for CI, set {ENV_API_KEY} in the environment:\n\
         \x20 export {ENV_API_KEY}=<your-api-key>"
    )
}

fn write_file_problem(
    f: &mut fmt::Formatter<'_>,
    what: &str,
    path: &Path,
    reason: &str,
) -> fmt::Result {
    write!(
        f,
        "{what} {}: {}",
        sanitize_path(path),
        sanitize_text(reason)
    )
}

fn write_malformed_file(f: &mut fmt::Formatter<'_>, path: &Path, reason: &str) -> fmt::Result {
    write!(
        f,
        "the user config file {} is not valid: {}",
        sanitize_path(path),
        sanitize_text(reason)
    )
}

fn write_credential_in_file(
    f: &mut fmt::Formatter<'_>,
    path: &Path,
    location: &str,
) -> fmt::Result {
    write!(
        f,
        "the user config file {} sets {CREDENTIAL_KEYS} at {}.\n\
         Credentials never live in the config file (which is meant to be \
         shareable); they belong in {CREDENTIALS_FILE_NAME} beside it, or in \
         {ENV_API_KEY}. Remove the key and re-run.",
        sanitize_path(path),
        sanitize_text(location)
    )
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

fn write_conflicting_url(
    f: &mut fmt::Formatter<'_>,
    profile: &str,
    source: ProfileSource,
) -> fmt::Result {
    // Neither URL is printed: a base URL can carry credentials in its
    // userinfo, path or query. The way out is phrased for the layer that
    // actually selected the profile - "drop --profile" is not an action a
    // user with `default_profile` in their config file can take.
    write!(
        f,
        "{ENV_URL} names a different instance than profile {:?} declares, so \
         that profile's API key was not sent: a credential that has gone to \
         the wrong server cannot be recalled.\n\
         Unset {ENV_URL}, {} to use {ENV_URL} on its own, or pass --url to \
         redirect this profile deliberately.",
        sanitize_name(profile),
        source.how_to_drop()
    )
}

fn write_invalid_profile_url(f: &mut fmt::Formatter<'_>, profile: &str) -> fmt::Result {
    write!(
        f,
        "profile {:?} declares a `url` that is not a usable base URL, so its \
         API key cannot be tied to an instance, and was not sent.\n\
         Fix that profile's `url` in {CONFIG_FILE_NAME} (an absolute \
         http/https URL with a host and no credentials).",
        sanitize_name(profile)
    )
}

fn write_unbound_credential(f: &mut fmt::Formatter<'_>, profile: &str) -> fmt::Result {
    write!(
        f,
        "profile {:?} declares no `url`, so its API key cannot be tied to the \
         instance {ENV_URL} names, and was not sent.\n\
         Add `url = \"...\"` under that profile in {CONFIG_FILE_NAME} so the \
         key has an instance, or pass --url to direct this run deliberately.",
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
        "profiles {:?} and {:?} both map to {}, so it is ambiguous which \
         instance that API key belongs to.\n\
         Rename one of them so each profile has its own variable.",
        sanitize_name(profile),
        sanitize_name(other),
        sanitize_name(variable)
    )
}

fn write_missing_url(f: &mut fmt::Formatter<'_>, profile: Option<&str>) -> fmt::Result {
    if let Some(profile) = profile {
        // Sanitized, and quoted rather than interpolated into a TOML header,
        // so a name carrying control bytes cannot reach the terminal raw.
        let profile = sanitize_name(profile);
        write!(
            f,
            // Not "or set OUTLINE_URL": with a profile in effect an
            // environment URL resolves but cannot be bound to that
            // profile's credential, so recommending it would recommend the
            // next error.
            "profile {profile:?} has no base URL.\n\
             Add `url = \"https://docs.example.com\"` under that profile in \
             {CONFIG_FILE_NAME}, or pass --url for a one-off."
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
    source: ProfileSource,
) -> fmt::Result {
    let variable = sanitize_name(variable);
    write!(
        f,
        "profile {:?} has no credentials: {variable} is not set, and \
         nothing is stored for it.\n\
         Sign in, or store a key, for THAT instance (both honour the \
         selected profile):\n\
         \x20 otl auth login\n\
         \x20 otl auth set-key\n\
         or set the variable, for CI:\n\
         \x20 export {variable}=<key-for-this-instance>",
        sanitize_name(profile)
    )?;
    if global_set {
        write!(
            f,
            "\n{ENV_API_KEY} is set but is deliberately not used with a \
             profile: sending it would give one workspace's key to another \
             workspace's server. To use it, {}.",
            source.how_to_drop()
        )?;
    }
    Ok(())
}

/// "The credential file has nothing for this profile."
///
/// Names the file so the user can see where it looked, and the command that
/// puts something there. The path is scrubbed like any other foreign text:
/// it can come from `OUTLINE_CONFIG_DIR`.
fn write_missing_stored(
    f: &mut fmt::Formatter<'_>,
    profile: Option<&str>,
    method: AuthMethod,
    path: &Path,
) -> fmt::Result {
    let subject = match profile {
        Some(name) => format!("profile {:?}", sanitize_name(name)),
        None => "the default profile".to_string(),
    };
    let remedy = match method {
        AuthMethod::Oauth => "otl auth login",
        AuthMethod::ApiKey => "otl auth set-key",
    };
    write!(
        f,
        "no `{method}` credential is stored for {subject} in {}.\n\
         Run `{remedy}` to store one.",
        sanitize_path(path)
    )
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

impl ConfigError {
    /// The variant name, which is the bulk of this error's Debug rendering.
    fn variant(&self) -> &'static str {
        match self {
            Self::MissingUrl { .. } => "MissingUrl",
            Self::MissingApiKey => "MissingApiKey",
            Self::MissingProfileApiKey { .. } => "MissingProfileApiKey",
            Self::ProfileApiKeyVarUnnameable { .. } => "ProfileApiKeyVarUnnameable",
            Self::UnboundProfileCredential { .. } => "UnboundProfileCredential",
            Self::ConflictingUrl { .. } => "ConflictingUrl",
            Self::InvalidProfileUrl { .. } => "InvalidProfileUrl",
            Self::AmbiguousProfileApiKeyVar { .. } => "AmbiguousProfileApiKeyVar",
            Self::UnknownProfile { .. } => "UnknownProfile",
            Self::ConfigFileUnreadable { .. } => "ConfigFileUnreadable",
            Self::MalformedConfigFile { .. } => "MalformedConfigFile",
            Self::CredentialInConfigFile { .. } => "CredentialInConfigFile",
            Self::MissingStoredCredential { .. } => "MissingStoredCredential",
            Self::UnsupportedAuthMethod { .. } => "UnsupportedAuthMethod",
        }
    }
}

impl fmt::Debug for ConfigError {
    /// Manual impl: the variant name and non-textual scalars only.
    ///
    /// Neither the derived Debug nor a forward to `Display` is safe here.
    /// The derived one prints raw names, paths and the whole `available`
    /// list; `Display` prints sanitized ones, but it MUST name the profile
    /// the user has to fix, and Debug is the surface that ends up in logs,
    /// panic messages and error chains, where nothing needs naming. So Debug
    /// carries no field text at all: what remains identifies the failure
    /// without disclosing anything about the configuration.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConfigError::{}", self.variant())?;
        match self {
            Self::MissingProfileApiKey { global_set, .. } => {
                write!(f, " {{ global_key_set: {global_set} }}")
            }
            Self::MissingStoredCredential { method, .. } => f
                .debug_struct(self.variant())
                .field("method", method)
                .finish_non_exhaustive(),
            Self::UnsupportedAuthMethod { method, .. } => {
                write!(f, " {{ method: {method} }}")
            }
            Self::UnknownProfile { available, .. } => {
                write!(f, " {{ defined_profiles: {} }}", available.len())
            }
            _ => Ok(()),
        }
    }
}

impl std::error::Error for ConfigError {}
