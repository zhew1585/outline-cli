//! Resolved settings, and the only code that can produce them.
//!
//! # Why this is a leaf module
//!
//! Rust privacy is module-tree wide: a private field is visible to the
//! module that declares it AND to every descendant. Declaring
//! [`Settings`]'s fields private in `config` therefore would NOT stop a
//! sibling such as a future `config::credentials` from building one by
//! hand - and the credential-release gate decides from
//! [`Settings::url_source`], so a hand-built `Settings` claiming
//! [`UrlSource::Flag`] would be handed any profile's key for any origin.
//!
//! Declaring them here, in a module with no children, is what makes
//! [`resolve_settings`] the only constructor: `config`, `config::release`,
//! `config::secret` and anything added beside them are all outside this
//! module's subtree.
//!
//! **This module must stay a leaf.** Adding a submodule to it would hand
//! that submodule the ability to forge resolved state, which is exactly what
//! the gate relies on being impossible; `config_isolation.rs` asserts the
//! property so it cannot regress quietly.

use std::fmt;

use super::{
    check_api_key_var_is_unambiguous, lookup_profile, non_blank, redacted_name, redacted_origin,
    AuthMethod, ConfigError, EnvLayer, LoadedConfig, Overrides,
};

/// Which layer supplied the base URL.
///
/// Recorded because the credential-release gate treats them differently: a
/// `--url` in the same command is a deliberate redirect, while an
/// environment variable is ambient and has to agree with the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlSource {
    /// `--url`.
    Flag,
    /// `OUTLINE_URL`.
    Env,
    /// The selected profile's `url`.
    Profile,
}

/// Everything resolved except the secret itself.
///
/// The fields are private and there is no public constructor: the only way
/// to obtain a `Settings` is [`resolve_settings`]. That is what makes the
/// credential-release gate structural rather than decorative - the gate
/// decides from [`Settings::url_source`], so a caller able to build a
/// `Settings` by hand could simply claim `UrlSource::Flag` and be handed a
/// profile's key for any origin at all. Read the parts through the
/// accessors; produce one only by resolving.
#[derive(Clone)]
pub struct Settings {
    /// Named profile in effect, if any.
    profile: Option<String>,
    /// Base URL of the Outline instance.
    base_url: String,
    /// Which layer supplied `base_url`.
    url_source: UrlSource,
    /// The selected profile's own `url`, when it declared one.
    ///
    /// Kept so that [`release_token`] can compare origins without needing
    /// the config file again.
    profile_url: Option<String>,
    /// How to authenticate to it.
    auth: AuthMethod,
}

impl Settings {
    /// The named profile in effect, if any.
    pub fn profile(&self) -> Option<&str> {
        self.profile.as_deref()
    }

    /// The resolved base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Which layer supplied the base URL.
    pub fn url_source(&self) -> UrlSource {
        self.url_source
    }

    /// The selected profile's own `url`, when it declared one.
    pub fn profile_url(&self) -> Option<&str> {
        self.profile_url.as_deref()
    }

    /// The resolved authentication method.
    pub fn auth(&self) -> AuthMethod {
        self.auth
    }
}

impl fmt::Debug for Settings {
    /// Manual impl: same base-URL redaction rule as [`Config`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("profile", &redacted_name(self.profile.as_deref()))
            .field("base_url_origin", &redacted_origin(Some(&self.base_url)))
            .field("url_source", &self.url_source)
            .field(
                "profile_url_origin",
                &redacted_origin(self.profile_url.as_deref()),
            )
            .field("auth", &self.auth)
            .finish()
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
    // Strict flag > env > file, for this key as for every other. Whether the
    // resolved origin may receive the resolved credential is a separate
    // question, asked once at the credential-release boundary
    // ([`release_token`]) rather than by bending precedence here.
    let profile_url = profile.and_then(|(_, p)| non_blank(p.url.as_deref()));
    let (base_url, url_source) = non_blank(overrides.url.as_deref())
        .map(|url| (url, UrlSource::Flag))
        .or_else(|| env.url.clone().map(|url| (url, UrlSource::Env)))
        .or_else(|| profile_url.clone().map(|url| (url, UrlSource::Profile)))
        .ok_or_else(|| ConfigError::MissingUrl {
            profile: profile.map(|(name, _)| name.to_string()),
        })?;
    Ok(Settings {
        profile: profile.map(|(name, _)| name.to_string()),
        base_url,
        url_source,
        profile_url,
        auth: profile.and_then(|(_, p)| p.auth).unwrap_or_default(),
    })
}
