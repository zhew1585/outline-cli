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

/// Placeholder shown instead of the API key in Debug output.
const REDACTED: &str = "***";

/// Resolved runtime configuration.
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
    /// the raw env value before validation; anything whose origin cannot
    /// be determined safely is redacted whole.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let origin = engine::base_url_origin(&self.base_url);
        let base_url: &str = origin.as_deref().unwrap_or(REDACTED);
        f.debug_struct("Config")
            .field("base_url_origin", &base_url)
            .field("api_key", &REDACTED)
            .finish()
    }
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

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn debug_output_redacts_api_key() {
        let config = Config {
            base_url: "https://docs.example.com".to_string(),
            api_key: "super-secret-key".to_string(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("super-secret-key"),
            "api key leaked: {rendered}"
        );
        assert!(rendered.contains("***"));
        assert!(rendered.contains("https://docs.example.com"));
    }

    #[test]
    fn debug_output_redacts_base_url_with_userinfo() {
        // Config holds the raw env value before Client::new validation, so
        // a base URL may still embed credentials at this point.
        let config = Config {
            base_url: "http://alice:url-secret-pw@example.com".to_string(),
            api_key: "k".to_string(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("url-secret-pw"),
            "base_url credential leaked: {rendered}"
        );
        assert!(!rendered.contains("alice"), "username leaked: {rendered}");
    }

    #[test]
    fn debug_output_redacts_base_url_with_query_secret() {
        // Credentials can hide outside userinfo too; anything that would
        // not pass Client::new shape checks is redacted whole.
        let config = Config {
            base_url: "https://example.com/?access_token=query-secret".to_string(),
            api_key: "query-secret".to_string(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("query-secret"),
            "query credential leaked: {rendered}"
        );
    }

    #[test]
    fn debug_output_shows_clean_base_url() {
        let config = Config {
            base_url: "https://docs.example.com".to_string(),
            api_key: "k".to_string(),
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("https://docs.example.com"));
    }

    #[test]
    fn debug_output_hides_base_url_path() {
        // A path can carry secrets too (token-in-path auth schemes);
        // Debug shows the origin only.
        let config = Config {
            base_url: "https://example.com/PATH-SECRET-9c7a".to_string(),
            api_key: "PATH-SECRET-9c7a".to_string(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("PATH-SECRET-9c7a"),
            "path secret leaked: {rendered}"
        );
        assert!(rendered.contains("https://example.com"));
    }
}
