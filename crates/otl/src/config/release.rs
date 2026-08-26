//! The credential-release boundary.
//!
//! Resolution ([`super::resolve_settings`]) answers "what did the layers
//! say?" with strict flag > env > file for every key, including the base URL.
//! This module answers the separate question of whether the resolved ORIGIN
//! is one the resolved CREDENTIAL belongs to, and it is the only place a
//! secret is obtained.
//!
//! The check is only as strong as what it decides from, so the inputs are
//! locked down in their own leaf modules: `config::resolved` owns
//! [`Settings`] and is the only module that can build one, and
//! `config::secret` owns the key storage and is the only module that can read
//! a key out. An unforgeable proof token issued from forgeable state would
//! prove nothing, and neither module's privacy would hold if the state lived
//! in `config` itself, where every sibling could reach it.
//!
//! **This module must stay a leaf**: a submodule of it could mint
//! [`BindingChecked`] without running the check. `config_isolation.rs`
//! asserts that.
//!
//! The separation matters because the two questions have different answers.
//! Bending precedence to make a profile's URL win would break the published
//! configuration model for every user; refusing to release a credential to an
//! instance its profile never named breaks nothing and is the only outcome
//! that cannot be undone if it is wrong.

use super::{ConfigError, Settings, UrlSource};

/// Proof that the credential-binding check has run FOR THESE SETTINGS.
///
/// The proof carries the settings it approved, and that is the whole point:
/// an unforgeable token saying only "a check ran" can be spent on a
/// different proposition than the one that was checked. A source handed such
/// a bare token could pass it, together with settings the gate had just
/// refused, to another source's `fetch` - laundering one legitimate approval
/// into a credential for an instance the gate said no to. Binding the two
/// together makes that unrepresentable: `fetch` receives no settings
/// argument at all, only this, and reads the approved ones out of it.
///
/// The field is private, so the token cannot be built outside this module,
/// and the lifetime ties it to the settings it borrows.
#[derive(Debug)]
pub struct BindingChecked<'settings>(&'settings Settings);

impl<'settings> BindingChecked<'settings> {
    /// The settings this check approved - the only ones a
    /// [`TokenSource`] may serve.
    pub fn settings(&self) -> &'settings Settings {
        self.0
    }
}

/// Where the secret comes from.
///
/// The single interface point between profile resolution and credential
/// storage: v1 ships `secret::EnvApiKey`, and the Epic 2 credential file becomes a
/// second implementation without any change above this line.
pub trait TokenSource {
    /// The bearer token for the approved settings, or a readable
    /// configuration error.
    ///
    /// The settings come out of `checked` rather than as a separate
    /// argument, so an implementation cannot serve any settings other than
    /// the ones the gate approved - including by delegating to another
    /// source. Not callable directly: see [`BindingChecked`] and
    /// [`release_token`].
    fn fetch(&self, checked: &BindingChecked<'_>) -> Result<String, ConfigError>;
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
    source.fetch(&checked)
}

/// Refuse to release a profile-scoped credential to an origin the profile
/// never named.
///
/// No profile in effect means no scoping question: the global credential and
/// the global URL variable belong to the same (single) scope.
fn check_credential_binding(settings: &Settings) -> Result<BindingChecked<'_>, ConfigError> {
    let Some(profile) = settings.profile() else {
        return Ok(BindingChecked(settings));
    };
    match settings.url_source() {
        // Stated in the same command as --profile: a deliberate redirect.
        UrlSource::Flag => Ok(BindingChecked(settings)),
        // The profile named this origin itself.
        UrlSource::Profile => Ok(BindingChecked(settings)),
        UrlSource::Env => check_env_url_binding(profile, settings),
    }
}

/// Decide whether an environment-supplied base URL is the profile's own
/// instance.
///
/// Three outcomes rather than two, because "the origins differ" and "an
/// origin could not be determined" are different problems and deserve
/// different diagnostics:
///
/// - the RESOLVED URL has no determinable origin: nothing can be sent to it,
///   so there is no credential exposure to prevent. The request channel
///   rejects it with a precise "invalid base URL" message, which is more
///   useful than this layer guessing;
/// - the resolved URL is usable but the profile's declared `url` is not: the
///   binding cannot be established at all, and the profile's own
///   configuration is what needs fixing;
/// - both parse: compare normalized origins.
fn check_env_url_binding<'settings>(
    profile: &str,
    settings: &'settings Settings,
) -> Result<BindingChecked<'settings>, ConfigError> {
    // Nothing to bind to: the profile scopes the credential but named no
    // instance, so an ambient variable would be deciding where it goes.
    let Some(declared) = settings.profile_url() else {
        return Err(ConfigError::UnboundProfileCredential {
            profile: profile.to_string(),
        });
    };
    let Some(resolved_origin) = engine::base_url_origin(settings.base_url()) else {
        return Ok(BindingChecked(settings));
    };
    let Some(declared_origin) = engine::base_url_origin(declared) else {
        return Err(ConfigError::InvalidProfileUrl {
            profile: profile.to_string(),
        });
    };
    if resolved_origin == declared_origin {
        Ok(BindingChecked(settings))
    } else {
        Err(ConfigError::ConflictingUrl {
            profile: profile.to_string(),
            source: settings.profile_source(),
        })
    }
}
