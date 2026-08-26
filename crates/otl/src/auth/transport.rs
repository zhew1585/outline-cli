//! Transport security for every URL the OAuth flow touches.
//!
//! One rule, applied to the instance URL and to every endpoint the instance
//! advertises: **TLS, unless the host is a loopback literal.**
//!
//! The OAuth flow puts the authorization code, the PKCE verifier, the
//! refresh token, the client secret and the token being revoked into
//! request bodies. Over plaintext HTTP all of them are readable by anything
//! on the path, and the refresh token in particular is a long-lived
//! credential. There is no partial mitigation available at this layer, so
//! plaintext is refused rather than warned about.
//!
//! The loopback exception is not a convenience: `otl`'s own redirect URI is
//! `http://127.0.0.1:<port>/callback`, which the RFCs both require and bless
//! (BCP 212 / RFC 8252 §7.3 - a loopback address never leaves the machine,
//! and no CA will issue a certificate for it). It is deliberately narrow:
//! only IP literals in `127.0.0.0/8` and `[::1]`, never the NAME
//! `localhost`, which resolves through the resolver and can be pointed
//! somewhere else.

use std::net::IpAddr;

use reqwest::Url;

use crate::auth::error::OAuthError;

/// The only scheme allowed for a non-loopback host.
const SECURE_SCHEME: &str = "https";

/// The scheme tolerated on a loopback IP literal.
const LOOPBACK_SCHEME: &str = "http";

/// Whether `url` may carry credentials.
///
/// `true` for `https://` anywhere, and for `http://` on a loopback IP
/// literal. Everything else - including `http://localhost`, which is a name
/// and not a literal - is false.
pub fn is_secure(url: &Url) -> bool {
    match url.scheme() {
        SECURE_SCHEME => true,
        LOOPBACK_SCHEME => is_loopback_literal(url),
        _ => false,
    }
}

/// Whether the URL's host is a loopback IP LITERAL.
///
/// The host text must PARSE as an IP address for this to be true, so a name
/// can never reach the loopback branch. That is the point: resolving
/// `localhost` here would reintroduce exactly the ambiguity the check
/// exists to remove, since a resolver, a hosts file, or a DNS answer can
/// make a name mean something else. Covers all of `127.0.0.0/8` and `::1`
/// through `IpAddr::is_loopback`.
fn is_loopback_literal(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    // An IPv6 host is serialized with brackets (`[::1]`).
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    literal
        .parse::<IpAddr>()
        .map(|address| address.is_loopback())
        .unwrap_or(false)
}

/// Refuse a URL that would carry credentials in plaintext.
///
/// `what` names the URL for the error message: the instance URL, or the
/// metadata field an endpoint came from.
pub fn require_secure(url: &str, what: &'static str) -> Result<(), OAuthError> {
    let parsed = Url::parse(url).map_err(|error| OAuthError::InsecureTransport {
        what,
        detail: format!("it is not a valid URL ({error})"),
    })?;
    if is_secure(&parsed) {
        return Ok(());
    }
    Err(OAuthError::InsecureTransport {
        what,
        detail: match parsed.scheme() {
            LOOPBACK_SCHEME => format!(
                "it uses plaintext http:// with a non-loopback host, which \
                 would send credentials in the clear; use https://, or \
                 {LOOPBACK_SCHEME}:// with a 127.0.0.1 / [::1] address for a \
                 local development instance"
            ),
            other => format!("its scheme is {other:?}, and only https:// is accepted"),
        },
    })
}

/// Refuse a plaintext endpoint that came out of the credential file.
///
/// Same rule as [`require_secure`], but reported as a STORED value rather
/// than a configured one, because the remedy differs: a fresh `otl auth
/// login` re-discovers the endpoints, whereas a bad `OUTLINE_URL` is edited
/// by hand.
pub fn require_stored_secure(
    url: &str,
    profile: &str,
    what: &'static str,
) -> Result<(), OAuthError> {
    require_secure(url, what).map_err(|error| OAuthError::InsecureStoredEndpoint {
        profile: profile.to_string(),
        what,
        detail: match error {
            OAuthError::InsecureTransport { detail, .. } => detail,
            other => other.to_string(),
        },
    })
}

/// Refuse a stored endpoint that does not belong to the instance in use.
///
/// Discovery enforced this when the endpoint was first recorded; enforcing
/// it again at use time means a credential file that was edited, or carried
/// from another machine, cannot redirect a refresh token to a third party.
pub fn require_same_origin(
    url: &str,
    origin: &str,
    profile: &str,
    what: &'static str,
) -> Result<(), OAuthError> {
    if crate::auth::endpoint::origin_of(url) == origin {
        return Ok(());
    }
    Err(OAuthError::InsecureStoredEndpoint {
        profile: profile.to_string(),
        what,
        detail: format!("it does not belong to {origin}, the instance in use"),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn secure(url: &str) -> bool {
        is_secure(&Url::parse(url).expect("a parseable URL"))
    }

    #[test]
    fn https_is_accepted_anywhere() {
        assert!(secure("https://docs.example.com"));
        assert!(secure("https://docs.example.com:8443/oauth/token"));
        assert!(secure("https://127.0.0.1:3000"));
    }

    #[test]
    fn plaintext_http_on_a_remote_host_is_refused() {
        assert!(!secure("http://docs.example.com"));
        assert!(!secure("http://192.168.1.10:3000"));
        assert!(!secure("http://10.0.0.1"));
        // A public address that merely LOOKS local is still remote.
        assert!(!secure("http://0.0.0.0:8080"));
    }

    #[test]
    fn plaintext_http_on_a_loopback_literal_is_the_only_exception() {
        assert!(secure("http://127.0.0.1:8586/callback"));
        assert!(secure("http://127.0.0.1"));
        // The whole 127/8 block, not just .0.1.
        assert!(secure("http://127.1.2.3:9000"));
        assert!(secure("http://[::1]:8586/callback"));
    }

    #[test]
    fn the_name_localhost_is_not_a_loopback_literal() {
        // `localhost` goes through the resolver, so a hosts file or a DNS
        // answer can point it elsewhere. Only literals are trusted.
        assert!(!secure("http://localhost:8586"));
        assert!(!secure("http://localhost.localdomain"));
        assert!(!secure("http://127.0.0.1.evil.example.com"));
    }

    #[test]
    fn other_schemes_are_refused() {
        assert!(!secure("ftp://docs.example.com"));
        assert!(!secure("file:///etc/passwd"));
        assert!(!secure("ws://docs.example.com"));
    }

    #[test]
    fn the_refusal_explains_what_to_do_about_it() {
        let error = require_secure("http://docs.example.com", "the instance URL")
            .expect_err("remote plaintext must be refused");
        let text = error.to_string();
        assert!(text.contains("the instance URL"), "{text}");
        assert!(text.contains("https://"), "{text}");
        assert!(text.contains("127.0.0.1"), "{text}");
    }

    #[test]
    fn a_non_http_scheme_is_named_in_the_refusal() {
        let error =
            require_secure("ftp://docs.example.com", "the token endpoint").expect_err("refused");
        assert!(error.to_string().contains("\"ftp\""), "{error}");
    }

    #[test]
    fn an_unparseable_url_is_refused_rather_than_assumed_secure() {
        assert!(require_secure("not a url", "the instance URL").is_err());
        assert!(require_secure("", "the instance URL").is_err());
    }
}
