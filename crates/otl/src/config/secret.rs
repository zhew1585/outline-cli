//! The credential container, and the only code that can read out of it.
//!
//! Rust privacy is module-tree wide, so declaring the key fields private in
//! `config` would leave them readable by `config` itself and by every
//! module beside this one. The credential-release gate would then be
//! decorative.
//!
//! [`EnvKeys`] therefore declares its fields here, in a module with no
//! children. `config` can CONSTRUCT one (from the process environment, or
//! from explicit values in a test) and can ask how many entries it holds,
//! but has no way to read a key out. The only read path is
//! [`EnvApiKey::fetch`], which cannot be called without the proof token
//! the gate issues.
//!
//! **This module must stay a leaf**; `config_isolation.rs` asserts it.

use std::collections::BTreeMap;
use std::env;

use super::release::{BindingChecked, TokenSource};
use super::{
    api_key_var_suffix, non_blank, profile_api_key_suffix, AuthMethod, ConfigError, EnvLayer,
    ENV_API_KEY, ENV_API_KEY_PREFIX, ENV_NAMES_ARE_CASE_INSENSITIVE,
};

/// API keys from the environment: the global one and the per-profile ones.
///
/// There is no accessor. The values leave this type only through
/// [`EnvApiKey::fetch`].
#[derive(Clone, Default)]
pub struct EnvKeys {
    /// `OUTLINE_API_KEY`: the key used when NO profile is in effect.
    global: Option<String>,
    /// Per-profile keys, keyed by the variable suffix after
    /// [`ENV_API_KEY_PREFIX`] (so `OUTLINE_API_KEY_WORK` is stored under
    /// `WORK`).
    per_profile: BTreeMap<String, String>,
}

impl EnvKeys {
    /// Collect every API key variable from the process environment.
    ///
    /// Blank values count as unset, matching every other variable.
    pub(super) fn from_process() -> Self {
        let per_profile = env::vars()
            .filter_map(|(name, value)| {
                let suffix = profile_api_key_suffix(&name, ENV_NAMES_ARE_CASE_INSENSITIVE)?;
                let value = non_blank(Some(&value))?;
                Some((suffix, value))
            })
            .collect();
        Self {
            global: env::var(ENV_API_KEY).ok().and_then(|v| non_blank(Some(&v))),
            per_profile,
        }
    }

    /// Store the global key. Write-only, like every method here.
    pub(super) fn with_global(mut self, api_key: &str) -> Self {
        self.global = non_blank(Some(api_key));
        self
    }

    /// Store one profile's key, applying the same blank-is-unset rule as the
    /// process environment.
    pub(super) fn with_profile(mut self, profile: &str, api_key: &str) -> Self {
        if let (Some(suffix), Some(value)) = (api_key_var_suffix(profile), non_blank(Some(api_key)))
        {
            self.per_profile.insert(suffix, value);
        }
        self
    }

    /// How many per-profile keys are held. The only thing about the contents
    /// that leaves this module, and it is a count, not a value.
    pub(super) fn profile_key_count(&self) -> usize {
        self.per_profile.len()
    }
}

/// The environment token source: an API key from the environment.
///
/// A credential belongs to ONE instance, and a profile names an instance, so
/// the two are resolved from the same scope:
///
/// - no profile in effect: the global `OUTLINE_API_KEY`;
/// - profile in effect: `OUTLINE_API_KEY_<PROFILE>` and nothing else.
///
/// The second rule does NOT fall back to the global variable: falling back
/// would send one workspace's key to whichever instance the selected
/// profile points at.
pub struct EnvApiKey<'layer>(pub &'layer EnvLayer);

impl TokenSource for EnvApiKey<'_> {
    fn fetch(&self, checked: &BindingChecked<'_>) -> Result<String, ConfigError> {
        let settings = checked.settings();
        let keys = self.0.keys();
        if settings.auth() != AuthMethod::ApiKey {
            return Err(ConfigError::UnsupportedAuthMethod {
                profile: settings.profile().map(str::to_string),
                method: settings.auth(),
            });
        }
        let Some(profile) = settings.profile() else {
            return keys.global.clone().ok_or(ConfigError::MissingApiKey);
        };
        let Some(suffix) = api_key_var_suffix(profile) else {
            return Err(ConfigError::ProfileApiKeyVarUnnameable {
                profile: profile.to_string(),
            });
        };
        keys.per_profile
            .get(&suffix)
            .cloned()
            .ok_or_else(|| ConfigError::MissingProfileApiKey {
                profile: profile.to_string(),
                variable: format!("{ENV_API_KEY_PREFIX}{suffix}"),
                global_set: keys.global.is_some(),
                source: settings.profile_source(),
            })
    }
}
