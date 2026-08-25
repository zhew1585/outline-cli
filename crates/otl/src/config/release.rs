//! The credential-release boundary.
//!
//! Resolution ([`super::resolve_settings`]) answers "what did the layers
//! say?" with strict flag > env > file for every key, including the base URL.
//! This module answers the separate question of whether the resolved ORIGIN
//! is one the resolved CREDENTIAL belongs to, and it is the only place a
//! secret is obtained.
//!
//! The separation matters because the two questions have different answers.
//! Bending precedence to make a profile's URL win would break the published
//! configuration model for every user; refusing to release a credential to an
//! instance its profile never named breaks nothing and is the only outcome
//! that cannot be undone if it is wrong.

use super::{
    api_key_var_suffix, AuthMethod, ConfigError, EnvLayer, Settings, UrlSource, ENV_API_KEY_PREFIX,
};

/// Proof that the credential-binding check has run.
///
/// Its only field is private, so this type cannot be constructed outside
/// this module. Since [`TokenSource::fetch`] demands one, a credential
/// cannot be obtained except through [`release_token`] - the check is not
/// something an implementation has to remember, and a source added later
/// (the Epic 2 credential file) cannot bypass it.
#[derive(Debug)]
pub struct BindingChecked(());

/// Where the secret comes from.
///
/// The single interface point between profile resolution and credential
/// storage: v1 ships [`EnvApiKey`], and the Epic 2 credential file becomes a
/// second implementation without any change above this line.
pub trait TokenSource {
    /// The bearer token for `settings`, or a readable configuration error.
    ///
    /// Not callable directly: see [`BindingChecked`] and [`release_token`].
    fn fetch(&self, settings: &Settings, checked: &BindingChecked) -> Result<String, ConfigError>;
}

/// The credential-release boundary: the one place a secret is obtained.
///
/// Resolution has already applied flag > env > file for every key. This gate
/// asks the separate question of whether the resolved ORIGIN is one the
/// resolved CREDENTIAL belongs to, and refuses to release the secret when it
/// is not. It is deliberately outside [`TokenSource`], so every present and
/// future source is covered by construction rather than by convention.
pub fn release_token(
    source: &impl TokenSource,
    settings: &Settings,
) -> Result<String, ConfigError> {
    let checked = check_credential_binding(settings)?;
    source.fetch(settings, &checked)
}

/// Refuse to release a profile-scoped credential to an origin the profile
/// never named.
///
/// No profile in effect means no scoping question: the global credential and
/// the global URL variable belong to the same (single) scope.
fn check_credential_binding(settings: &Settings) -> Result<BindingChecked, ConfigError> {
    let Some(profile) = settings.profile.as_deref() else {
        return Ok(BindingChecked(()));
    };
    match settings.url_source {
        // Stated in the same command as --profile: a deliberate redirect.
        UrlSource::Flag => Ok(BindingChecked(())),
        // The profile named this origin itself.
        UrlSource::Profile => Ok(BindingChecked(())),
        UrlSource::Env => match settings.profile_url.as_deref() {
            Some(declared) if same_origin(declared, &settings.base_url) => Ok(BindingChecked(())),
            Some(_) => Err(ConfigError::ConflictingUrl {
                profile: profile.to_string(),
            }),
            // Nothing to bind to: the profile scopes the credential but
            // named no instance, so an ambient variable would be deciding
            // where that credential goes.
            None => Err(ConfigError::UnboundProfileCredential {
                profile: profile.to_string(),
            }),
        },
    }
}

/// Whether two base URLs name the same server.
///
/// Compared as normalized origins (`scheme://host[:port]`), so a trailing
/// slash, host casing and a default port are all equivalent, matching what
/// the request channel itself tolerates. A URL whose origin cannot be
/// determined is never "the same" as anything: it will fail validation in
/// the request channel with a clearer message, and guessing here would be
/// guessing about a credential's destination.
fn same_origin(left: &str, right: &str) -> bool {
    match (
        engine::base_url_origin(left),
        engine::base_url_origin(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
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
    fn fetch(&self, settings: &Settings, _checked: &BindingChecked) -> Result<String, ConfigError> {
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
