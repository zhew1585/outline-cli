//! Releasing a credential that came out of the credential file.
//!
//! The credential FILE is read by the `auth` module, which owns its hygiene:
//! the owner-only open, the descriptor checks, the atomic write, the lock.
//! This module owns the other half - deciding whether what was read may be
//! sent to the instance that was resolved - and it does that by being an
//! ordinary [`TokenSource`], so the binding check in
//! [`crate::config::release_token`] applies to a stored credential exactly
//! as it does to an environment variable.
//!
//! # Why the file is not read here
//!
//! Reading it here would make `config` depend on `auth`, and `config` is
//! the lower layer: `auth` resolves configuration, not the other way
//! round. It would also drag the whole authentication stack into
//! `config_isolation.rs`'s permission probe, which compiles this module
//! tree on its own against an explicit list of `--extern` crates - a list
//! that would then have to name every dependency `auth` has. A source that
//! is handed its secret keeps this module to `std` plus `config`'s own
//! types, and keeps that harness honest.
//!
//! That probe scans for `crate::` anywhere in the config sources, doc
//! comments included, so `auth` is named in prose here rather than through
//! an intra-doc link. A link would not be a compile-time dependency, but it
//! would look like one to the probe and drag the whole authentication stack
//! into it.
//!
//! Being handed the secret is not a weaker position than [`EnvApiKey`]
//! is in: that type also holds the value before `fetch` is called. The gate
//! has never been about preventing a READ, only about approving a RELEASE.
//!
//! [`EnvApiKey`]: crate::config::EnvApiKey

use std::path::Path;

use super::release::{BindingChecked, TokenSource};
use super::resolved::Settings;
use super::{AuthMethod, ConfigError};

/// A credential read out of the credential file, awaiting release.
///
/// Holds a borrowed secret and the path it came from. The path is for the
/// diagnostic when there is nothing to release; the secret is never part of
/// any message.
pub struct StoredCredential<'a> {
    /// What the file holds for the selected profile under the resolved
    /// authentication method, if anything.
    token: Option<&'a str>,
    /// Absolute path of the credential file, for diagnostics only.
    path: &'a Path,
}

impl<'a> StoredCredential<'a> {
    /// A stored credential, or the absence of one, for `path`.
    pub fn new(token: Option<&'a str>, path: &'a Path) -> Self {
        Self { token, path }
    }

    /// Whether there is anything here to release.
    pub fn is_present(&self) -> bool {
        self.token.is_some()
    }
}

impl TokenSource for StoredCredential<'_> {
    /// Release the stored credential, or explain that there is none.
    ///
    /// Reached only after the binding check has passed for these settings,
    /// so a profile's stored credential cannot be released for an origin
    /// the gate refused - the same protection `OUTLINE_API_KEY_<PROFILE>`
    /// gets, obtained by being a `TokenSource` rather than by repeating the
    /// check here.
    fn fetch(&self, checked: &BindingChecked<'_>) -> Result<String, ConfigError> {
        let settings = checked.settings();
        self.token
            .map(str::to_string)
            .ok_or_else(|| ConfigError::MissingStoredCredential {
                profile: settings.profile().map(str::to_string),
                method: settings.auth(),
                path: self.path.to_path_buf(),
            })
    }
}

/// Which store a credential should be released from for these settings.
///
/// Stated as a type rather than left implicit at the call site: the choice
/// decides which secret goes on the wire, and a reader should not have to
/// reconstruct it from an `if`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `OUTLINE_API_KEY` / `OUTLINE_API_KEY_<PROFILE>`.
    Environment,
    /// The credential file, written by `otl auth login` / `set-key`.
    CredentialFile,
}

/// Pick the source for `settings`, given whether the file has anything.
///
/// Two rules, in this order:
///
/// 1. `auth = "oauth"` can only ever come from the credential file. An
///    environment variable cannot hold a renewable session.
/// 2. Otherwise the credential file wins when it has something, and the
///    environment is the fallback. A stored key was put there deliberately
///    by `otl auth set-key`; an exported variable is often left over from
///    another shell, and it is the less protected of the two.
///
/// # Relationship to the credential file's own precedence
///
/// This function decides FILE vs ENVIRONMENT and nothing else. Which
/// credential inside the file is used - a renewable OAuth session or a
/// stored API key - is decided by `auth`, which is the layer that can see
/// the file. The two cannot contradict each other, because a session is
/// never a candidate here: `Config` holds one fixed key, and a session that
/// rotates on every refresh is not that. `auth` therefore settles the
/// session case before it ever asks for a `Config`, and passes only a
/// stored API key in as `file_has_credential`.
pub fn select(settings: &Settings, file_has_credential: bool) -> Source {
    match settings.auth() {
        AuthMethod::Oauth => Source::CredentialFile,
        AuthMethod::ApiKey if file_has_credential => Source::CredentialFile,
        AuthMethod::ApiKey => Source::Environment,
    }
}

// Behaviour tests live in `crates/otl/tests/config_credentials.rs`. They
// need a real `Settings`, and the only way to get one is `resolve_settings`
// - `Settings` has no constructor on purpose, and adding a test-only one
// here would be exactly the widening `config_isolation.rs` guards against.
