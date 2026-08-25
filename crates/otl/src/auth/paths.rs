//! Where credentials live, and which profile they belong to.
//!
//! Paths always go through `directories`, never through hand-built
//! `$HOME/...` strings: Windows has no `~/.config`, and assuming Unix
//! layout is one of this project's explicit anti-patterns.

use std::env;
use std::path::PathBuf;

use directories::ProjectDirs;

use crate::auth::error::StoreError;

/// Environment variable that overrides the whole configuration directory.
///
/// Exists so that tests (and containers with an unusual layout) can point
/// the credential file somewhere else without touching `HOME`.
pub const ENV_CONFIG_DIR: &str = "OUTLINE_CONFIG_DIR";

/// Environment variable naming the active profile.
///
/// Kept here, minimal and local, on purpose: the full configuration and
/// profile system is owned elsewhere, and this module only needs the name
/// under which to file a set of credentials.
pub const ENV_PROFILE: &str = "OUTLINE_PROFILE";

/// Profile used when none is named.
pub const DEFAULT_PROFILE: &str = "default";

/// Directory name under the platform configuration root.
pub const APP_DIR_NAME: &str = "outline-cli";

/// Credential file name. Deliberately separate from `config.toml`: config
/// can be shared or committed, credentials never can.
pub const CREDENTIALS_FILE_NAME: &str = "credentials.toml";

/// Lock file guarding token refresh (advisory lock, same directory).
pub const LOCK_FILE_NAME: &str = "credentials.lock";

/// Permission bits the credential file is created with, and validated
/// against on every read. Unix only.
#[cfg(unix)]
pub const FILE_MODE: u32 = 0o600;

/// Permission bits the credential directory is created with. Unix only.
#[cfg(unix)]
pub const DIR_MODE: u32 = 0o700;

/// Permission bits that must NOT be set on the credential file: anything
/// granting group or other access. Unix only.
#[cfg(unix)]
pub const FORBIDDEN_MODE_BITS: u32 = 0o077;

/// Maximum length of a profile name.
pub const MAX_PROFILE_NAME: usize = 64;

/// Punctuation accepted in a profile name, for error messages.
const PROFILE_NAME_PUNCTUATION: &str = "`-`, `_` and `.`";

/// The directory holding the credential file.
///
/// [`ENV_CONFIG_DIR`] wins when set; otherwise the platform per-user
/// configuration directory (`~/.config/outline-cli` on Linux,
/// `~/Library/Application Support/outline-cli` on macOS,
/// `%APPDATA%\outline-cli` on Windows).
pub fn config_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = non_empty_env(ENV_CONFIG_DIR) {
        return Ok(PathBuf::from(dir));
    }
    ProjectDirs::from("", "", APP_DIR_NAME)
        .map(|dirs| dirs.config_dir().to_path_buf())
        .ok_or(StoreError::NoConfigDir)
}

/// The active profile name, validated for use as a credential-file key.
///
/// The name becomes a TOML table key and part of user-facing messages, so
/// it is restricted to an unambiguous character set rather than escaped.
pub fn active_profile() -> Result<String, StoreError> {
    let name = non_empty_env(ENV_PROFILE).unwrap_or_else(|| DEFAULT_PROFILE.to_string());
    validate_profile_name(&name)?;
    Ok(name)
}

/// Reject profile names that would need escaping or could not be typed
/// back into a command line.
fn validate_profile_name(name: &str) -> Result<(), StoreError> {
    let usable = !name.is_empty()
        && name.len() <= MAX_PROFILE_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if usable {
        return Ok(());
    }
    Err(StoreError::ProfileName {
        name: name.to_string(),
        allowed: PROFILE_NAME_PUNCTUATION,
        max: MAX_PROFILE_NAME,
    })
}

/// Read an environment variable, treating blank values as unset.
fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_profile_name_that_would_need_toml_escaping() {
        for bad in ["a\"b", "a b", "a\nb", "a/b", "[x]", ""] {
            assert!(
                validate_profile_name(bad).is_err(),
                "accepted unusable profile name {bad:?}"
            );
        }
    }

    #[test]
    fn accepts_ordinary_profile_names() {
        for good in ["default", "work", "self-hosted", "acme_2", "a.b"] {
            assert!(
                validate_profile_name(good).is_ok(),
                "rejected usable profile name {good:?}"
            );
        }
    }

    #[test]
    fn rejects_an_overlong_profile_name() {
        let long = "a".repeat(MAX_PROFILE_NAME + 1);
        assert!(validate_profile_name(&long).is_err());
        assert!(validate_profile_name(&long[..MAX_PROFILE_NAME]).is_ok());
    }
}
