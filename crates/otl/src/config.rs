//! Runtime configuration, read from environment variables.
//!
//! Story 1.1 scope: env only. Config file and named profiles come later;
//! precedence will be flag > env > user config file.

use std::env;
use std::fmt;

/// Environment variable holding the Outline instance base URL.
pub const ENV_URL: &str = "OUTLINE_URL";
/// Environment variable holding the API key.
pub const ENV_API_KEY: &str = "OUTLINE_API_KEY";

/// Resolved runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Base URL of the Outline instance (e.g. `https://docs.example.com`).
    pub base_url: String,
    /// API key used as a bearer token.
    pub api_key: String,
}

/// Configuration errors. Always reported before any network request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `OUTLINE_URL` is unset or empty.
    MissingUrl,
    /// `OUTLINE_API_KEY` is unset or empty.
    MissingApiKey,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUrl => write!(
                f,
                "{ENV_URL} is not set.\n\
                 Set it to your Outline instance base URL, for example:\n\
                 \x20 export {ENV_URL}=https://docs.example.com"
            ),
            Self::MissingApiKey => write!(
                f,
                "{ENV_API_KEY} is not set.\n\
                 Create an API key in Outline (Settings -> API) and set it, for example:\n\
                 \x20 export {ENV_API_KEY}=<your-api-key>"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Load configuration from the environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let base_url = non_empty_env(ENV_URL).ok_or(ConfigError::MissingUrl)?;
        let api_key = non_empty_env(ENV_API_KEY).ok_or(ConfigError::MissingApiKey)?;
        Ok(Self { base_url, api_key })
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}
