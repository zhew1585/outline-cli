//! The loopback redirect listener that catches the authorization code.
//!
//! Two binding strategies, because the two client-acquisition paths need
//! different things:
//!
//! - A client an administrator pre-registered can only use redirect URIs
//!   that were registered with it, so the ports are a FIXED, documented
//!   list ([`CALLBACK_PORTS`]) tried in order.
//! - A client `otl` registers itself can name any redirect URI, so it binds
//!   an EPHEMERAL port first and registers that exact URI afterwards.
//!
//! The listener binds `127.0.0.1` literally - never `localhost`, which may
//! resolve to `::1`, and never `0.0.0.0`, which would expose the
//! authorization code to the local network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use reqwest::Url;

use crate::auth::error::OAuthError;

/// Loopback address the callback listener binds. Literal, never resolved.
pub const CALLBACK_HOST: &str = "127.0.0.1";

/// Path the authorization server redirects to.
pub const CALLBACK_PATH: &str = "/callback";

/// The documented fixed callback ports, tried in order.
///
/// This list is a public contract: an administrator pre-registering an
/// application must allow these four redirect URIs, so entries may be
/// appended but never renumbered.
pub const CALLBACK_PORTS: &[u16] = &[8586, 18586, 28586, 38586];

/// How long `otl auth login` waits for the browser to come back.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(240);

/// How often the listener is polled while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Read timeout for one accepted connection.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum bytes read from one request line.
const MAX_REQUEST_LINE_BYTES: u64 = 8 * 1024;

/// Maximum connections handled before giving up.
///
/// Browsers send stray requests (favicon, prefetch) that must be answered
/// and ignored, but an unbounded loop would let a local process keep the
/// login hanging.
const MAX_CONNECTIONS: u32 = 64;

/// Every redirect URI an administrator must allow when pre-registering an
/// application, in the order `otl` tries them.
pub fn documented_redirect_uris() -> Vec<String> {
    CALLBACK_PORTS
        .iter()
        .map(|port| format!("http://{CALLBACK_HOST}:{port}{CALLBACK_PATH}"))
        .collect()
}

/// The port of a redirect URI this CLI produced.
pub fn port_of(redirect_uri: &str) -> Option<u16> {
    Url::parse(redirect_uri).ok()?.port()
}

/// What the browser was redirected with.
struct Redirect {
    /// The `code` parameter, when the grant succeeded.
    code: Option<String>,
    /// The `state` parameter, echoed back by the server.
    state: Option<String>,
    /// The `error` parameter, when the grant was refused.
    error: Option<String>,
    /// The `error_description` parameter, when present.
    description: Option<String>,
}

/// A bound loopback listener plus the exact redirect URI it answers on.
#[derive(Debug)]
pub struct CallbackServer {
    listener: TcpListener,
    redirect_uri: String,
}

impl CallbackServer {
    /// Bind the first free port from [`CALLBACK_PORTS`].
    pub fn bind_fixed() -> Result<Self, OAuthError> {
        for port in CALLBACK_PORTS {
            if let Ok(server) = Self::bind_port(*port) {
                return Ok(server);
            }
        }
        Err(OAuthError::NoCallbackPort {
            ports: CALLBACK_PORTS
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        })
    }

    /// Bind an ephemeral port chosen by the operating system.
    ///
    /// Used on the dynamic-registration path, where the exact redirect URI
    /// is registered after the port is known.
    pub fn bind_ephemeral() -> Result<Self, OAuthError> {
        Self::bind_port(0).map_err(|_| OAuthError::NoCallbackPort {
            ports: "an ephemeral port".to_string(),
        })
    }

    /// Bind one specific port.
    pub fn bind_port(port: u16) -> Result<Self, OAuthError> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(address).map_err(|error| OAuthError::Callback {
            reason: format!("cannot listen on {CALLBACK_HOST}:{port} ({error})"),
        })?;
        let bound = listener
            .local_addr()
            .map_err(|error| OAuthError::Callback {
                reason: format!("cannot read the bound port ({error})"),
            })?;
        listener
            .set_nonblocking(true)
            .map_err(|error| OAuthError::Callback {
                reason: format!("cannot poll the callback listener ({error})"),
            })?;
        Ok(Self {
            listener,
            redirect_uri: format!("http://{CALLBACK_HOST}:{}{CALLBACK_PATH}", bound.port()),
        })
    }

    /// The exact redirect URI to send in the authorization request, and to
    /// register with a dynamically created client.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the redirect and return its authorization code.
    ///
    /// `state` is compared before the code is even read out of the query:
    /// a redirect that did not come from this login attempt must not have
    /// its code exchanged, whatever else it carries.
    pub fn wait_for_code(&self, state: &str, timeout: Duration) -> Result<String, OAuthError> {
        let deadline = Instant::now() + timeout;
        for _ in 0..MAX_CONNECTIONS {
            let stream = self.accept_before(deadline, timeout)?;
            let Some(target) = read_request_target(&stream) else {
                respond(&stream, "400 Bad Request", "Malformed request.");
                continue;
            };
            let Some(redirect) = parse_redirect(&target) else {
                // A stray browser request (favicon, prefetch): answer and
                // keep waiting for the real redirect.
                respond(&stream, "404 Not Found", "Not the otl callback.");
                continue;
            };
            return self.finish(&stream, redirect, state);
        }
        Err(OAuthError::Callback {
            reason: format!(
                "gave up after {MAX_CONNECTIONS} local connections without a \
                 usable redirect"
            ),
        })
    }

    /// Validate one redirect and turn it into a code or a typed failure.
    fn finish(
        &self,
        stream: &TcpStream,
        redirect: Redirect,
        expected_state: &str,
    ) -> Result<String, OAuthError> {
        // State first: nothing else in the query may be acted on until the
        // redirect is known to belong to this login.
        if redirect.state.as_deref() != Some(expected_state) {
            respond(
                stream,
                "400 Bad Request",
                "State mismatch: this redirect did not come from the otl login \
                 that is running. Nothing was exchanged.",
            );
            return Err(OAuthError::StateMismatch);
        }
        if let Some(code) = redirect.error {
            respond(
                stream,
                "200 OK",
                "Authorization was not granted. You can close this tab.",
            );
            return Err(OAuthError::AuthorizationDenied {
                code: sanitize_redirect_text(&code),
                detail: match redirect.description {
                    Some(text) => format!(": {}", sanitize_redirect_text(&text)),
                    None => String::new(),
                },
            });
        }
        match redirect.code {
            Some(code) => {
                respond(
                    stream,
                    "200 OK",
                    "Signed in. You can close this tab and go back to the terminal.",
                );
                Ok(code)
            }
            None => {
                respond(stream, "400 Bad Request", "Redirect carried no code.");
                Err(OAuthError::Callback {
                    reason: "the redirect carried neither a code nor an error".to_string(),
                })
            }
        }
    }

    /// Accept the next connection, or time out.
    fn accept_before(&self, deadline: Instant, budget: Duration) -> Result<TcpStream, OAuthError> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Back to blocking for this connection: the read has
                    // its own timeout, and polling a socket byte by byte
                    // buys nothing.
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
                    return Ok(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    return Err(OAuthError::Callback {
                        reason: format!("cannot accept the browser redirect ({error})"),
                    })
                }
            }
            if Instant::now() >= deadline {
                return Err(OAuthError::CallbackTimeout {
                    seconds: budget.as_secs(),
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Read the request target out of an HTTP request line.
///
/// Only `GET` is accepted; the read is capped, so a client that never sends
/// a newline cannot make the CLI allocate without bound.
fn read_request_target(stream: &TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?).take(MAX_REQUEST_LINE_BYTES);
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    (method == "GET").then(|| target.to_string())
}

/// Parse a redirect target, or `None` if it is not the callback path.
fn parse_redirect(target: &str) -> Option<Redirect> {
    // A request target is path-relative; give it a base so a real URL
    // parser can handle the query, including percent-encoding.
    let url = Url::parse(&format!("http://{CALLBACK_HOST}{target}")).ok()?;
    if url.path() != CALLBACK_PATH {
        return None;
    }
    let mut redirect = Redirect {
        code: None,
        state: None,
        error: None,
        description: None,
    };
    for (key, value) in url.query_pairs() {
        let value = value.into_owned();
        match key.as_ref() {
            "code" => redirect.code = Some(value),
            "state" => redirect.state = Some(value),
            "error" => redirect.error = Some(value),
            "error_description" => redirect.description = Some(value),
            _ => {}
        }
    }
    Some(redirect)
}

/// Maximum characters kept from redirect-supplied text.
const MAX_REDIRECT_TEXT_CHARS: usize = 200;

/// Make redirect query text safe to print.
///
/// The redirect comes through the user's browser, so its values are
/// attacker-influenceable text like any other server response: it goes
/// through the same hygiene pipeline (no secret to redact here, but control
/// characters, invisible codepoints and length are all handled).
fn sanitize_redirect_text(text: &str) -> String {
    engine::sanitize::clean_server_text(text, "", false, MAX_REDIRECT_TEXT_CHARS)
}

/// Send a minimal HTML response. Best effort: the browser tab is a
/// courtesy, and a write failure must not lose an authorization code.
fn respond(stream: &TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>otl</title>\
         <body style=\"font-family:system-ui;margin:3rem\"><p>{message}</p></body>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = stream;
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_fixed_port_list_is_the_documented_contract() {
        // Renumbering these would silently break every application an
        // administrator has already registered.
        assert_eq!(CALLBACK_PORTS, &[8586, 18586, 28586, 38586]);
    }

    #[test]
    fn an_ephemeral_bind_reports_the_port_it_actually_got() {
        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let uri = server.redirect_uri();
        assert!(uri.starts_with("http://127.0.0.1:"), "{uri}");
        assert!(uri.ends_with(CALLBACK_PATH), "{uri}");
        let port: u16 = uri
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches(CALLBACK_PATH)
            .parse()
            .expect("a numeric port");
        assert_ne!(port, 0, "an ephemeral bind must report the real port");
    }

    #[test]
    fn binding_a_taken_port_fails_instead_of_sharing_it() {
        let first = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port: u16 = first
            .redirect_uri()
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches(CALLBACK_PATH)
            .parse()
            .unwrap();
        assert!(
            CallbackServer::bind_port(port).is_err(),
            "two listeners bound the same callback port"
        );
    }

    #[test]
    fn the_documented_redirect_uris_match_the_port_list() {
        let uris = documented_redirect_uris();
        assert_eq!(uris.len(), CALLBACK_PORTS.len());
        assert_eq!(uris[0], "http://127.0.0.1:8586/callback");
        for (uri, port) in uris.iter().zip(CALLBACK_PORTS) {
            assert_eq!(port_of(uri), Some(*port), "{uri}");
        }
    }

    #[test]
    fn a_redirect_uri_without_a_port_has_none_to_rebind() {
        assert_eq!(port_of("http://127.0.0.1/callback"), None);
        assert_eq!(port_of("not a url"), None);
    }

    #[test]
    fn a_redirect_query_is_parsed_including_percent_encoding() {
        let redirect = parse_redirect("/callback?code=abc%2Fdef&state=xyz").expect("callback path");
        assert_eq!(redirect.code.as_deref(), Some("abc/def"));
        assert_eq!(redirect.state.as_deref(), Some("xyz"));
        assert!(redirect.error.is_none());
    }

    #[test]
    fn a_non_callback_path_is_not_treated_as_a_redirect() {
        assert!(parse_redirect("/favicon.ico").is_none());
        assert!(parse_redirect("/callback/extra?code=a").is_none());
        assert!(parse_redirect("/?code=a").is_none());
    }

    #[test]
    fn an_error_redirect_is_parsed_as_such() {
        let redirect =
            parse_redirect("/callback?error=access_denied&error_description=User+said+no&state=s")
                .expect("callback path");
        assert_eq!(redirect.error.as_deref(), Some("access_denied"));
        assert_eq!(redirect.description.as_deref(), Some("User said no"));
        assert!(redirect.code.is_none());
    }

    #[test]
    fn redirect_text_is_stripped_of_control_and_invisible_characters() {
        let cleaned = sanitize_redirect_text("denied\u{1b}[31m\u{200b}by\nadmin");
        assert!(!cleaned.contains('\u{1b}'), "{cleaned}");
        assert!(!cleaned.contains('\u{200b}'), "{cleaned}");
        assert!(!cleaned.contains('\n'), "{cleaned}");
    }

    #[test]
    fn redirect_text_is_length_capped() {
        let cleaned = sanitize_redirect_text(&"x".repeat(1000));
        assert_eq!(cleaned.chars().count(), MAX_REDIRECT_TEXT_CHARS);
    }

    #[test]
    fn waiting_times_out_without_a_redirect() {
        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let error = server
            .wait_for_code("state", Duration::from_millis(120))
            .expect_err("no browser is coming");
        assert!(
            matches!(error, OAuthError::CallbackTimeout { .. }),
            "expected a timeout, got {error:?}"
        );
        assert!(error.to_string().contains("otl auth login"));
    }
}
