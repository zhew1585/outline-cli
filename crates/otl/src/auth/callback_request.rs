//! Reading and answering one HTTP request on the loopback callback.
//!
//! Split out of [`crate::auth::loopback`], which owns the listener and the
//! wait loop. Everything here is the wire format: parse a request line,
//! decide whether it is the redirect, validate it, write a reply.
//!
//! Two properties are load-bearing and easy to lose in a refactor:
//!
//! - [`read_request_target`] budgets the WHOLE request line, not each read.
//!   A per-read timeout only fires against a peer sending nothing, so a
//!   peer sending one byte per second held a handler for hours.
//! - [`finish`] compares `state` BEFORE it looks at the code. A redirect
//!   that did not come from this login must not have its code exchanged,
//!   whatever else the query contains.

use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use reqwest::Url;

use crate::auth::error::OAuthError;
use crate::auth::loopback::{CALLBACK_HOST, CALLBACK_PATH};

/// Maximum bytes read from one request line.
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;

/// Shortest socket read timeout used while a budget is still running.
///
/// A zero timeout means "block forever" to the socket API, which is the
/// opposite of what any of this wants.
const MIN_READ_WINDOW: Duration = Duration::from_millis(10);

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

/// Serve one connection: `Some` when it was the redirect (successfully or
/// not), `None` when it was anything else and the wait continues.
pub fn handle(
    stream: &TcpStream,
    expected_state: &str,
    budget: Duration,
) -> Option<Result<String, OAuthError>> {
    let Some(target) = read_request_target(stream, budget) else {
        respond(stream, "400 Bad Request", "Malformed request.");
        return None;
    };
    let Some(redirect) = parse_redirect(&target) else {
        // A stray request: answer it and keep waiting for the real one.
        respond(stream, "404 Not Found", "Not the otl callback.");
        return None;
    };
    Some(finish(stream, redirect, expected_state))
}

/// Validate one redirect and turn it into a code or a typed failure.
fn finish(
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

/// Read the request target out of an HTTP request line.
///
/// Only `GET` is accepted, the line is capped at [`MAX_REQUEST_LINE_BYTES`],
/// and - the part that matters - `budget` bounds the WHOLE line rather than
/// each individual read. The loop therefore ends on time whether the peer
/// sends nothing, sends one byte per second forever, or sends a valid
/// request instantly.
///
/// Reading a byte at a time is not a cost: `BufReader` still does one
/// syscall per bufferful, and the per-byte step is what lets the budget be
/// checked as the line arrives.
fn read_request_target(stream: &TcpStream, budget: Duration) -> Option<String> {
    let deadline = Instant::now() + budget;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line: Vec<u8> = Vec::with_capacity(128);
    while line.len() < MAX_REQUEST_LINE_BYTES {
        let left = deadline.checked_duration_since(Instant::now())?;
        stream
            .set_read_timeout(Some(left.max(MIN_READ_WINDOW)))
            .ok()?;
        let mut byte = [0_u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => return None,
            Ok(_) if byte[0] == b'\n' => return request_target(&line),
            Ok(_) => line.push(byte[0]),
            Err(_) => return None,
        }
    }
    None
}

/// The target of a `GET` request line, if that is what this line is.
fn request_target(line: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(line).ok()?;
    let mut parts = text.split_whitespace();
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
}
