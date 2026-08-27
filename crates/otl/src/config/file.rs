//! Locating, reading and parsing the user config file.
//!
//! The reading side is bounded (a config file is a few hundred bytes, and a
//! path can point at a device or a pipe) and the parsing side never lets
//! parser-produced text into a diagnostic: see [`parse_reason`].

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    sanitize_name, ConfigError, ConfigFile, ConfigSource, EnvLayer, LoadedConfig, Overrides,
};

/// Application directory name under the platform config root.
const APP_DIR_NAME: &str = "outline-cli";
/// File name of the user config file inside the config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Maximum accepted size of the user config file. A config file is a few
/// hundred bytes; anything larger is a mistake (or a device/pipe), and it
/// is read into memory, so the read is bounded.
const MAX_CONFIG_FILE_BYTES: u64 = 64 * 1024;
/// The complete config-file schema, as static text. Included in parse
/// diagnostics instead of the parser's own wording, which would quote the
/// offending value.
const SCHEMA_HINT: &str = "valid keys are `default_profile` and `profiles` at the top level, \
     and `url` and `auth` (`api-key` or `oauth`) inside a profile";

/// The default config directory for `otl`.
///
/// This is `~/.config/outline-cli` on Linux and macOS, and
/// `%APPDATA%\outline-cli\config` on Windows. `None` when no home directory
/// can be determined.
pub fn config_dir() -> Option<PathBuf> {
    crate::user_dirs::config_dir(APP_DIR_NAME)
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
///
/// Every name that reaches a message goes through [`sanitize_name`]: a TOML
/// quoted key can carry ESC or newline bytes.
fn validate(file: &ConfigFile, path: &Path) -> Result<(), ConfigError> {
    let credential = |location: String| ConfigError::CredentialInConfigFile {
        path: path.to_path_buf(),
        location,
    };
    if file.api_key.is_some() || file.token.is_some() {
        return Err(credential("the top level".to_string()));
    }
    for (name, profile) in &file.profiles {
        if profile.api_key.is_some() || profile.token.is_some() {
            return Err(credential(format!("profile {:?}", sanitize_name(name))));
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

/// A description of a TOML error built entirely from text this module owns,
/// plus a line number.
///
/// No parser-produced text reaches the output. Both `Display` and
/// `message()` interpolate content from the file: `Display` renders an
/// annotated source snippet, and `message()` embeds the offending VALUE for
/// an unknown enum variant (`unknown variant \`<secret>\``), for a type
/// mismatch (`invalid type: string "<secret>"`) and for an unknown bare key.
/// A user who wrongly put a credential in the config file must not see it
/// again in a diagnostic, so the parser message is used only to CLASSIFY the
/// failure, and the classification falls back to the generic bucket for any
/// wording this function does not recognise.
///
/// The line number is enough to act on, and the schema is restated in full
/// as static text, which is more useful than naming the offending key.
fn parse_reason(error: &toml::de::Error, raw: &str) -> String {
    let location = match error.span() {
        Some(span) => format!("line {}: ", line_at(raw, span.start)),
        None => String::new(),
    };
    let (kind, schema_related) = classify_parse_error(error.message());
    let hint = if schema_related {
        format!("; {SCHEMA_HINT}")
    } else {
        String::new()
    };
    format!("{location}{kind}{hint}")
}

/// Map a parser message to one of our own value-free descriptions.
///
/// Prefix matching on the parser's wording, which is why an unrecognised
/// message must fall through to the generic description rather than being
/// forwarded: forwarding is what would leak a value.
fn classify_parse_error(message: &str) -> (&'static str, bool) {
    for (prefix, description) in [
        ("unknown field", "unknown key"),
        (
            "unknown variant",
            "a key was given a value outside its fixed set of choices",
        ),
        ("invalid type", "a key was given a value of the wrong type"),
        ("missing field", "a required key is missing"),
        ("duplicate key", "a key is defined twice"),
    ] {
        if message.starts_with(prefix) {
            return (description, true);
        }
    }
    ("the file is not valid TOML", false)
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
