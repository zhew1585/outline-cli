//! Authorization-server metadata discovery (RFC 8414).
//!
//! Every endpoint the login flow uses comes from
//! `/.well-known/oauth-authorization-server` on the instance itself - none
//! is hard-coded, so a self-hosted Outline that moves its routes keeps
//! working.
//!
//! Three checks stand between a metadata document and a credential going
//! somewhere it should not. All three refuse rather than warn, because a
//! warning is printed after the request has already gone out.
//!
//! 1. **Same origin.** Every advertised endpoint must sit on the instance's
//!    own origin. The authorization code, the PKCE verifier and the refresh
//!    token are all posted to the token endpoint, so a document that
//!    pointed it at another host would hand those to a server the user
//!    never named. Outline serves its own OAuth endpoints, so this costs
//!    nothing.
//! 2. **TLS.** Every endpoint must be `https://`, or `http://` on a
//!    loopback literal (see [`crate::auth::transport`]).
//! 3. **Issuer.** RFC 8414 section 3.3 requires the document's `issuer` to
//!    match the identifier the well-known URL was derived from. Without it,
//!    a same-origin reverse proxy, a multi-tenant deployment or a poisoned
//!    cache could serve another tenant's authorization server from the same
//!    origin - and checks 1 and 2 would both pass.

use reqwest::blocking::Client;
use serde_json::Value;

use crate::auth::endpoint::{self, Call};
use crate::auth::error::{OAuthError, Stage};
use crate::auth::transport;

/// Well-known path of the authorization-server metadata document.
pub const METADATA_PATH: &str = "/.well-known/oauth-authorization-server";

/// PKCE code challenge method this CLI uses. `plain` is never acceptable.
pub const CODE_CHALLENGE_METHOD: &str = "S256";

/// Scopes requested by default. The test instance advertises exactly these
/// two global scopes.
pub const DEFAULT_SCOPE: &str = "read write";

/// Metadata field names, so a message and its check cannot drift apart.
const FIELD_ISSUER: &str = "issuer";

/// What the instance advertises about its OAuth endpoints.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// Issuer identifier. Validated to identify this instance, so it is
    /// not optional the way the wire format allows.
    pub issuer: String,
    /// Where the browser is sent for consent.
    pub authorization_endpoint: String,
    /// Where codes and refresh tokens are exchanged.
    pub token_endpoint: String,
    /// RFC 7591 dynamic client registration, when offered.
    pub registration_endpoint: Option<String>,
    /// RFC 7009 token revocation, when offered.
    pub revocation_endpoint: Option<String>,
    /// Scopes the server says it supports, when advertised.
    pub scopes_supported: Vec<String>,
    /// Whether the server advertises PKCE `S256`.
    pub supports_s256: bool,
}

/// Fetch and validate the metadata document for `base_url`.
///
/// The instance URL's own transport is checked BEFORE the request goes out:
/// a plaintext discovery request would already have leaked which instance
/// is being used, and would be answered by whoever is on the path.
pub fn discover(http: &Client, base_url: &str) -> Result<Metadata, OAuthError> {
    transport::require_secure(base_url, "the instance URL")?;
    // The expected issuer goes through the SAME URL parser the endpoint
    // origin check uses. Comparing the user's raw `OUTLINE_URL` text
    // against a server's canonical identifier made every equivalent-but-
    // differently-spelled URL - a mixed-case host, an explicit `:443`, a
    // legacy numeric IPv4 form - fail login while working everywhere else,
    // and blamed the server for it.
    let issuer = canonical_issuer(base_url)?;
    let origin = endpoint::origin_of(base_url);
    let url = format!("{}{METADATA_PATH}", base_url.trim_end_matches('/'));
    let call = Call {
        stage: Stage::Discovery,
        url: &url,
        secrets: &[],
    };
    let document = endpoint::get_json(http, call)?;
    build(&document, &issuer, &origin, call)
}

/// Turn a fetched document into validated [`Metadata`].
///
/// `expected_issuer` is the instance URL the well-known path was appended
/// to, which is exactly the identifier RFC 8414 says the document must
/// claim.
fn build(
    document: &Value,
    expected_issuer: &str,
    origin: &str,
    call: Call<'_>,
) -> Result<Metadata, OAuthError> {
    let issuer = require_issuer(document, expected_issuer, origin)?;
    let authorization_endpoint = usable_endpoint(
        endpoint::require_str(document, "authorization_endpoint", call)?,
        origin,
        "authorization_endpoint",
    )?;
    let token_endpoint = usable_endpoint(
        endpoint::require_str(document, "token_endpoint", call)?,
        origin,
        "token_endpoint",
    )?;
    let registration_endpoint = optional_usable_endpoint(
        endpoint::optional_str(document, "registration_endpoint"),
        origin,
        "registration_endpoint",
    )?;
    let revocation_endpoint = optional_usable_endpoint(
        endpoint::optional_str(document, "revocation_endpoint"),
        origin,
        "revocation_endpoint",
    )?;
    Ok(Metadata {
        issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        revocation_endpoint,
        scopes_supported: string_list(document, "scopes_supported"),
        supports_s256: string_list(document, "code_challenge_methods_supported")
            .iter()
            .any(|method| method == CODE_CHALLENGE_METHOD),
    })
}

/// Require the document to claim exactly the issuer it was fetched from.
///
/// Comparison is on the full identifier, not just the origin: two tenants
/// behind one hostname differ only by path, and that is precisely the case
/// an origin comparison would wave through. A single trailing slash is
/// tolerated on either side, since RFC 8414 leaves it unconstrained and
/// servers differ.
fn require_issuer(document: &Value, expected: &str, origin: &str) -> Result<String, OAuthError> {
    let mismatch = |detail: String| OAuthError::IssuerMismatch {
        origin: origin.to_string(),
        detail,
    };
    let Some(claimed) = endpoint::optional_str(document, FIELD_ISSUER) else {
        return Err(mismatch(format!(
            " (it has no {FIELD_ISSUER} field, which RFC 8414 requires)"
        )));
    };
    let Some(claimed_canonical) = canonical_issuer(&claimed).ok() else {
        return Err(mismatch(format!(
            " (its {FIELD_ISSUER} is not a usable URL)"
        )));
    };
    if claimed_canonical != expected {
        // The claimed value is server-controlled text, so it is not echoed:
        // naming the expectation is enough to act on, and enough to avoid
        // putting an attacker's string on the terminal.
        return Err(mismatch(format!(
            " (its {FIELD_ISSUER} is not {expected:?})"
        )));
    }
    Ok(claimed)
}

/// Reduce an issuer to the one spelling two parties can agree on.
///
/// Parsing normalizes case, default ports, and legacy address forms, so two
/// URLs that name the same server compare equal however they were typed.
/// Exactly ONE trailing slash is then dropped - never more, because
/// `https://host/tenant///` and `https://host/tenant` are different paths
/// to a reverse proxy that routes on the path without collapsing repeated
/// separators, and those can be different security domains.
fn canonical_issuer(url: &str) -> Result<String, OAuthError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| OAuthError::Malformed {
        stage: Stage::Discovery,
        origin: endpoint::origin_of(url),
        reason: format!("{url:?} is not a valid URL ({error})"),
    })?;
    Ok(strip_one_slash(parsed.as_str()).to_string())
}

/// Drop at most ONE trailing slash.
///
/// RFC 8414 leaves the trailing slash of an issuer unconstrained and
/// servers differ, so a single one is treated as a formatting difference.
/// Collapsing ANY number would not be: `https://host/tenant///` and
/// `https://host/tenant` are different paths to a reverse proxy that routes
/// on the path without normalizing repeated separators, and those can be
/// different security domains.
fn strip_one_slash(issuer: &str) -> &str {
    issuer.strip_suffix('/').unwrap_or(issuer)
}

/// Require an advertised endpoint to be same-origin AND TLS-protected.
fn usable_endpoint(url: String, origin: &str, field: &'static str) -> Result<String, OAuthError> {
    if endpoint::origin_of(&url) != origin {
        return Err(OAuthError::ForeignEndpoint {
            origin: origin.to_string(),
            endpoint: field,
        });
    }
    // Same-origin does not imply secure: an instance reached over https can
    // still advertise an http:// endpoint on its own host, and reqwest keeps
    // the Authorization header across a same-host scheme downgrade.
    transport::require_secure(&url, field)?;
    Ok(url)
}

/// [`usable_endpoint`] for an endpoint the server may legitimately omit.
fn optional_usable_endpoint(
    url: Option<String>,
    origin: &str,
    field: &'static str,
) -> Result<Option<String>, OAuthError> {
    url.map(|url| usable_endpoint(url, origin, field))
        .transpose()
}

/// A JSON array of strings, or an empty list.
fn string_list(document: &Value, field: &str) -> Vec<String> {
    document
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;

    /// The instance identifier the well-known URL was derived from, which
    /// is also what the document must claim as its issuer.
    const ISSUER: &str = "https://docs.example.com";
    const ORIGIN: &str = "https://docs.example.com";

    fn call() -> Call<'static> {
        Call {
            stage: Stage::Discovery,
            url: "https://docs.example.com/.well-known/oauth-authorization-server",
            secrets: &[],
        }
    }

    /// Validate a document as if it had been fetched from [`ISSUER`].
    fn check(document: &Value) -> Result<Metadata, OAuthError> {
        build(
            document,
            &canonical_issuer(ISSUER).expect("a valid test issuer"),
            ORIGIN,
            call(),
        )
    }

    fn full_document() -> Value {
        json!({
            "issuer": ISSUER,
            "authorization_endpoint": "https://docs.example.com/oauth/authorize",
            "token_endpoint": "https://docs.example.com/oauth/token",
            "registration_endpoint": "https://docs.example.com/oauth/register",
            "revocation_endpoint": "https://docs.example.com/oauth/revoke",
            "scopes_supported": ["read", "write"],
            "code_challenge_methods_supported": ["S256"]
        })
    }

    #[test]
    fn a_complete_document_is_accepted() {
        let metadata = check(&full_document()).expect("valid metadata");
        assert!(metadata.supports_s256);
        assert_eq!(metadata.scopes_supported, vec!["read", "write"]);
        assert_eq!(metadata.issuer, ISSUER);
        assert_eq!(
            metadata.registration_endpoint.as_deref(),
            Some("https://docs.example.com/oauth/register")
        );
    }

    #[test]
    fn a_missing_token_endpoint_is_refused() {
        let mut document = full_document();
        document
            .as_object_mut()
            .map(|map| map.remove("token_endpoint"));
        let error = check(&document).expect_err("token_endpoint is required");
        assert!(error.to_string().contains("token_endpoint"), "{error}");
    }

    #[test]
    fn an_off_origin_token_endpoint_is_refused_rather_than_followed() {
        let mut document = full_document();
        document["token_endpoint"] = json!("https://evil.example.net/oauth/token");
        let error = check(&document).expect_err("an off-origin endpoint is unsafe");
        let text = error.to_string();
        assert!(text.contains("token_endpoint"), "{text}");
        assert!(text.contains("different host"), "{text}");
    }

    #[test]
    fn an_off_origin_optional_endpoint_is_refused_too() {
        for field in ["registration_endpoint", "revocation_endpoint"] {
            let mut document = full_document();
            document[field] = json!("https://evil.example.net/x");
            assert!(check(&document).is_err(), "{field} was accepted off-origin");
        }
    }

    #[test]
    fn a_different_port_counts_as_a_different_origin() {
        let mut document = full_document();
        document["token_endpoint"] = json!("https://docs.example.com:8443/oauth/token");
        assert!(check(&document).is_err());
    }

    // --- finding [2]: plaintext transport ------------------------------

    #[test]
    fn a_plaintext_remote_instance_is_refused_by_the_transport_rule() {
        // A wholly plaintext remote instance: every endpoint would carry
        // the code, the verifier and the refresh token in the clear.
        let base = "http://docs.example.com";
        let document = json!({
            "issuer": base,
            "authorization_endpoint": "http://docs.example.com/oauth/authorize",
            "token_endpoint": "http://docs.example.com/oauth/token",
            "registration_endpoint": "http://docs.example.com/oauth/register",
            "revocation_endpoint": "http://docs.example.com/oauth/revoke",
            "code_challenge_methods_supported": ["S256"]
        });
        let error = build(&document, base, base, call())
            .expect_err("a plaintext remote instance must be refused");
        let text = error.to_string();
        assert!(text.contains("https://"), "{text}");
        assert!(
            text.contains("authorization_endpoint"),
            "the refused field should be named: {text}"
        );
    }

    #[test]
    fn a_scheme_downgrade_on_the_same_host_is_refused() {
        // An https instance advertising an http endpoint on its own host.
        // Caught as a different ORIGIN, because an origin includes the
        // scheme - which is also why the origin check is not redundant with
        // the transport check.
        for field in [
            "authorization_endpoint",
            "token_endpoint",
            "registration_endpoint",
            "revocation_endpoint",
        ] {
            let mut document = full_document();
            document[field] = json!("http://docs.example.com/oauth/x");
            let error = check(&document)
                .expect_err("a plaintext endpoint must never be used, however it is classified");
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn a_loopback_instance_may_use_plaintext() {
        // The documented exception, and the only one: a local development
        // instance on a loopback literal.
        let document = json!({
            "issuer": "http://127.0.0.1:3000",
            "authorization_endpoint": "http://127.0.0.1:3000/oauth/authorize",
            "token_endpoint": "http://127.0.0.1:3000/oauth/token",
            "code_challenge_methods_supported": ["S256"]
        });
        let metadata = build(
            &document,
            "http://127.0.0.1:3000",
            "http://127.0.0.1:3000",
            call(),
        )
        .expect("a loopback instance is usable over http");
        assert!(metadata.supports_s256);
    }

    #[test]
    fn a_plaintext_instance_url_is_refused_before_any_request() {
        // `discover` checks the instance URL itself first, so no plaintext
        // discovery request is ever made.
        let error = transport::require_secure("http://docs.example.com", "the instance URL")
            .expect_err("a remote plaintext instance must be refused");
        assert!(error.to_string().contains("instance URL"), "{error}");
    }

    // --- finding [12]: RFC 8414 issuer ---------------------------------

    #[test]
    fn a_document_without_an_issuer_is_refused() {
        let mut document = full_document();
        document.as_object_mut().map(|map| map.remove("issuer"));
        let error = check(&document).expect_err("RFC 8414 requires an issuer");
        let text = error.to_string();
        assert!(text.contains("issuer"), "{text}");
        assert!(text.contains("RFC 8414"), "{text}");
    }

    #[test]
    fn an_issuer_for_another_tenant_on_the_same_origin_is_refused() {
        // The case an origin comparison cannot catch: same scheme, host and
        // port, different path. Multi-tenant deployments and path-routing
        // reverse proxies make this reachable.
        let mut document = full_document();
        document["issuer"] = json!("https://docs.example.com/other-tenant");
        let error = check(&document).expect_err("a different tenant must be refused");
        assert!(error.to_string().contains("issuer"), "{error}");
    }

    #[test]
    fn an_issuer_on_another_host_is_refused() {
        let mut document = full_document();
        document["issuer"] = json!("https://evil.example.net");
        assert!(check(&document).is_err());
    }

    #[test]
    fn a_trailing_slash_on_the_issuer_is_tolerated() {
        // RFC 8414 does not constrain the trailing slash and servers differ;
        // this is a formatting difference, not a different identity.
        let mut document = full_document();
        document["issuer"] = json!("https://docs.example.com/");
        assert!(check(&document).is_ok());
    }

    #[test]
    fn repeated_trailing_slashes_are_not_folded_away() {
        // One trailing slash is a formatting difference RFC 8414 leaves
        // open. Several are a different PATH, and a reverse proxy that
        // routes on the path without collapsing separators can serve a
        // different security domain from it.
        for claimed in ["https://docs.example.com//", "https://docs.example.com///"] {
            let mut document = full_document();
            document["issuer"] = json!(claimed);
            assert!(
                check(&document).is_err(),
                "{claimed:?} was folded onto the expected issuer"
            );
        }
    }

    #[test]
    fn a_tenant_path_with_repeated_slashes_is_a_different_issuer() {
        let expected = "https://docs.example.com/tenant";
        let mut document = full_document();
        document["issuer"] = json!("https://docs.example.com/tenant///");
        document["authorization_endpoint"] = json!("https://docs.example.com/oauth/authorize");
        document["token_endpoint"] = json!("https://docs.example.com/oauth/token");
        assert!(build(&document, expected, ORIGIN, call()).is_err());
    }

    #[test]
    fn an_equivalent_spelling_of_the_instance_url_still_matches() {
        // R3 [29]: the expected issuer was the user's RAW `OUTLINE_URL`
        // text while endpoints were compared as parsed origins. Any
        // equivalent-but-not-byte-identical spelling then failed login -
        // and blamed the server for it - while working in every other
        // command. Both sides now go through the same parser.
        for spelling in [
            "https://DOCS.example.com",
            "https://docs.example.com:443",
            "https://docs.example.com/",
        ] {
            let document = full_document();
            assert!(
                build(
                    &document,
                    &canonical_issuer(spelling).unwrap(),
                    ORIGIN,
                    call()
                )
                .is_ok(),
                "{spelling:?} was rejected as a different issuer"
            );
        }
    }

    #[test]
    fn a_legacy_numeric_address_normalizes_to_the_same_issuer() {
        // `0177.0.0.1` is a legal spelling of 127.0.0.1 that the URL parser
        // canonicalizes; the two must not be treated as different servers.
        assert_eq!(
            canonical_issuer("http://0177.0.0.1:45124").unwrap(),
            canonical_issuer("http://127.0.0.1:45124").unwrap()
        );
    }

    #[test]
    fn canonicalizing_does_not_merge_different_paths() {
        // Normalization must not become laxity: distinct tenants stay
        // distinct, and repeated separators are not collapsed.
        let base = canonical_issuer("https://docs.example.com/tenant").unwrap();
        for other in [
            "https://docs.example.com/other",
            "https://docs.example.com/tenant///",
            "https://docs.example.com",
        ] {
            assert_ne!(
                canonical_issuer(other).unwrap(),
                base,
                "{other:?} was merged with {base:?}"
            );
        }
    }

    #[test]
    fn an_unparseable_claimed_issuer_is_refused() {
        let mut document = full_document();
        document["issuer"] = json!("not a url");
        assert!(check(&document).is_err());
    }

    #[test]
    fn a_refused_issuer_is_not_echoed_back_to_the_terminal() {
        // The claimed issuer is server-controlled text; naming what was
        // expected is actionable without printing an attacker's string.
        let mut document = full_document();
        document["issuer"] = json!("https://evil.example.net/PAYLOAD-9c7a");
        let error = check(&document).expect_err("refused");
        let text = format!("{error} / {error:?}");
        assert!(!text.contains("PAYLOAD-9c7a"), "server text echoed: {text}");
    }

    // --- unchanged behaviour -------------------------------------------

    #[test]
    fn a_server_without_s256_is_reported_as_such() {
        let mut document = full_document();
        document["code_challenge_methods_supported"] = json!(["plain"]);
        let metadata = check(&document).expect("still parseable");
        assert!(!metadata.supports_s256);
    }

    #[test]
    fn optional_endpoints_may_be_absent() {
        let document = json!({
            "issuer": ISSUER,
            "authorization_endpoint": "https://docs.example.com/oauth/authorize",
            "token_endpoint": "https://docs.example.com/oauth/token"
        });
        let metadata = check(&document).expect("minimal metadata is valid");
        assert!(metadata.registration_endpoint.is_none());
        assert!(metadata.revocation_endpoint.is_none());
        assert!(!metadata.supports_s256);
    }
}
