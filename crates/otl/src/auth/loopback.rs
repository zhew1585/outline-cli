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

/// Longest a single connection may take to send its request line.
///
/// Also clamped to whatever is left of the overall deadline, so a slow
/// client cannot extend the login past the timeout the user was promised.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum bytes read from one request line.
const MAX_REQUEST_LINE_BYTES: u64 = 8 * 1024;

/// Sanity bound on connections handled in one login.
///
/// The real limit is the DEADLINE, not this: browsers send stray requests
/// (favicon, prefetch, connection reuse) that must be answered and ignored,
/// and failing the login because a local process opened some sockets would
/// hand any unprivileged local user a way to break every login. This exists
/// only so a client that connects and disconnects instantly cannot spin the
/// loop forever, and it is generous enough that legitimate traffic never
/// reaches it.
const MAX_CONNECTIONS: u32 = 4096;

/// Pause after a connection that yielded nothing, so a peer that connects
/// and closes immediately cannot spin the accept loop at full speed.
///
/// Sized together with [`MAX_CONNECTIONS`]: the product
/// (4096 x 5ms ~ 20s) is comfortably under [`AUTH_TIMEOUT`], so a flood of
/// empty connections runs out the iteration bound only long after the user
/// would have finished consenting - it cannot end a login early.
const EMPTY_CONNECTION_BACKOFF: Duration = Duration::from_millis(5);

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
            // Checked before every connection AND inside the accept poll,
            // so the announced timeout is the real bound on this loop. A
            // per-connection read timeout alone would let N slow clients
            // stretch the wait to N times its length.
            if Instant::now() >= deadline {
                break;
            }
            let stream = self.accept_before(deadline, timeout)?;
            let Some(target) = read_request_target(&stream, deadline) else {
                respond(&stream, "400 Bad Request", "Malformed request.");
                // Nothing usable arrived; do not spin on a peer that keeps
                // connecting and closing.
                std::thread::sleep(EMPTY_CONNECTION_BACKOFF);
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
        Err(OAuthError::CallbackTimeout {
            seconds: timeout.as_secs(),
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
    ///
    /// Per-connection read and write timeouts are clamped to the time left
    /// before `deadline`, so no accepted connection can outlive the login's
    /// own budget.
    fn accept_before(&self, deadline: Instant, budget: Duration) -> Result<TcpStream, OAuthError> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Back to blocking for this connection: the read has
                    // its own timeout, and polling a socket byte by byte
                    // buys nothing.
                    let _ = stream.set_nonblocking(false);
                    let window = connection_window(deadline);
                    let _ = stream.set_read_timeout(Some(window));
                    let _ = stream.set_write_timeout(Some(window));
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
fn read_request_target(stream: &TcpStream, deadline: Instant) -> Option<String> {
    // Re-clamped here as well: `accept_before` set the window when the
    // connection arrived, and time has passed since.
    let _ = stream.set_read_timeout(Some(connection_window(deadline)));
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

/// How long one connection may take: the shorter of [`READ_TIMEOUT`] and
/// the time left before the login's own deadline.
///
/// Never zero - a zero timeout means "block forever" to the socket API,
/// which is the opposite of what is wanted here.
fn connection_window(deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    remaining.min(READ_TIMEOUT).max(MINIMUM_WINDOW)
}

/// Floor for a per-connection timeout, since zero means "never time out".
const MINIMUM_WINDOW: Duration = Duration::from_millis(50);

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
    fn a_stalled_connection_cannot_outlive_the_announced_timeout() {
        // Reported: the per-connection read timeout was a flat 10s and the
        // deadline was only checked when `accept` would have blocked, so a
        // peer that kept connections ready could stretch a 240s login to
        // roughly 64 x 10s. Clamping each connection to the time remaining
        // makes the announced timeout the real bound.
        use std::net::TcpStream;

        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port = port_of(server.redirect_uri()).expect("a bound port");

        // Connect and then say nothing at all, holding the socket open.
        let _stalled = TcpStream::connect((CALLBACK_HOST, port)).expect("local connect");

        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let error = server
            .wait_for_code("state", budget)
            .expect_err("no redirect is coming");
        let elapsed = started.elapsed();

        assert!(
            matches!(error, OAuthError::CallbackTimeout { .. }),
            "expected a timeout, got {error:?}"
        );
        // Generous, but far below the old flat per-connection timeout: the
        // point is that one silent peer cannot add 10s to the wait.
        assert!(
            elapsed < READ_TIMEOUT,
            "a stalled connection extended the wait to {elapsed:?}, past the \
             {budget:?} budget and up to the {READ_TIMEOUT:?} read timeout"
        );
    }

    #[test]
    fn stray_connections_do_not_consume_the_login() {
        // A local peer opening and closing sockets used to burn the whole
        // connection budget and fail the login outright. Only the deadline
        // may end the wait, so after several stray connections the listener
        // is still willing to serve the real redirect.
        use std::io::Write as _;
        use std::net::TcpStream;

        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port = port_of(server.redirect_uri()).expect("a bound port");

        // More stray connections than the OLD budget allowed in total.
        std::thread::spawn(move || {
            for _ in 0..80 {
                if let Ok(stream) = TcpStream::connect((CALLBACK_HOST, port)) {
                    drop(stream);
                }
            }
            // Then the genuine redirect.
            if let Ok(mut stream) = TcpStream::connect((CALLBACK_HOST, port)) {
                let _ = stream.write_all(
                    b"GET /callback?code=real-code&state=state HTTP/1.1\r\n\
                      Host: 127.0.0.1\r\n\r\n",
                );
                let _ = stream.flush();
                // Keep the socket alive long enough to be read.
                std::thread::sleep(Duration::from_millis(400));
            }
        });

        let code = server
            .wait_for_code("state", Duration::from_secs(20))
            .expect("stray connections must not defeat the real redirect");
        assert_eq!(code, "real-code");
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
