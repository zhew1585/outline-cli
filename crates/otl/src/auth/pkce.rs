//! PKCE (RFC 7636) and the CSRF `state`.
//!
//! `otl` registers as a PUBLIC client: there is no client secret to prove
//! who redeems an authorization code, so PKCE is the only thing that binds
//! the code to the process that asked for it. `S256` only - `plain` puts
//! the verifier in the authorization request, which defeats the point.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::fmt;

use crate::auth::error::OAuthError;

/// Bytes of entropy in a code verifier.
///
/// 32 bytes base64url-encode to 43 characters, exactly RFC 7636's minimum
/// verifier length, with 256 bits of entropy behind it.
pub const VERIFIER_BYTES: usize = 32;

/// Bytes of entropy in the `state` parameter.
pub const STATE_BYTES: usize = 16;

/// A PKCE verifier and the challenge derived from it.
///
/// The verifier is a credential (it redeems the authorization code), so
/// this type has a hand-written `Debug`.
pub struct Pkce {
    /// Secret sent only to the token endpoint.
    verifier: String,
    /// SHA-256 of the verifier, base64url without padding - the value that
    /// travels through the browser.
    challenge: String,
}

impl Pkce {
    /// Generate a fresh verifier/challenge pair.
    pub fn generate() -> Result<Self, OAuthError> {
        let verifier = random_base64url(VERIFIER_BYTES, "PKCE verifier")?;
        let digest = Sha256::digest(verifier.as_bytes());
        Ok(Self {
            challenge: URL_SAFE_NO_PAD.encode(digest),
            verifier,
        })
    }

    /// The verifier, for the token request only.
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// The challenge, for the authorization request.
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

impl fmt::Debug for Pkce {
    /// Manual impl: the verifier must never appear in Debug output. The
    /// challenge is public but is withheld too, since printing it next to
    /// a redaction marker only invites confusion.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce").finish_non_exhaustive()
    }
}

/// Generate a random `state` value for CSRF protection.
pub fn random_state() -> Result<String, OAuthError> {
    random_base64url(STATE_BYTES, "state value")
}

/// `len` random bytes, base64url-encoded without padding.
fn random_base64url(len: usize, what: &'static str) -> Result<String, OAuthError> {
    let mut bytes = vec![0_u8; len];
    getrandom::fill(&mut bytes).map_err(|error| OAuthError::Random {
        what,
        reason: error.to_string(),
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_challenge_is_the_s256_of_the_verifier() {
        let pkce = Pkce::generate().expect("randomness is available");
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier().as_bytes()));
        assert_eq!(pkce.challenge(), expected);
    }

    #[test]
    fn the_challenge_matches_the_rfc_7636_appendix_b_vector() {
        // The RFC's own worked example, so an encoding mistake cannot hide
        // behind a self-consistent implementation.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn a_verifier_meets_rfc_7636_length_and_alphabet_rules() {
        let pkce = Pkce::generate().expect("randomness is available");
        let verifier = pkce.verifier();
        assert!(
            (43..=128).contains(&verifier.len()),
            "verifier length {} is outside 43..=128",
            verifier.len()
        );
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')),
            "verifier uses characters outside the unreserved set: {verifier}"
        );
    }

    #[test]
    fn every_generation_is_different() {
        let first = Pkce::generate().expect("randomness");
        let second = Pkce::generate().expect("randomness");
        assert_ne!(first.verifier(), second.verifier());
        assert_ne!(random_state().unwrap(), random_state().unwrap());
    }

    #[test]
    fn a_state_value_is_url_safe_and_long_enough_to_be_unguessable() {
        let state = random_state().expect("randomness");
        // 16 bytes -> 22 base64url characters, 128 bits of entropy.
        assert_eq!(state.len(), 22, "unexpected state length: {state}");
        assert!(state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')));
    }

    #[test]
    fn debug_output_never_shows_the_verifier() {
        let pkce = Pkce::generate().expect("randomness");
        let rendered = format!("{pkce:?}");
        assert!(
            !rendered.contains(pkce.verifier()),
            "verifier leaked: {rendered}"
        );
    }
}
