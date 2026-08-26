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

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use reqwest::Url;

use crate::auth::callback_request::handle;
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

/// Total time one connection gets to deliver its whole request line.
///
/// A TOTAL budget, not a per-read timeout, and the difference is the whole
/// point: a socket read timeout only fires when a peer sends NOTHING, so a
/// peer sending one byte every second keeps every read succeeding and holds
/// its handler for as long as it likes - about two and a half hours at the
/// 8 KiB line cap. Budgeting the request line as a whole bounds that at
/// this value no matter how the bytes are paced.
///
/// A browser that has opened a connection sends its request line in the
/// same breath, so 2s is enormous for the legitimate case.
const REQUEST_BUDGET: Duration = Duration::from_secs(2);

/// Total request-line budget for a connection served while at capacity.
///
/// Deliberately tiny. On loopback a real callback's bytes are already in
/// the socket buffer by the time the connection is accepted, so this is
/// thousands of times more than it needs, while a peer trying to occupy the
/// accept loop gets almost nothing for each connection it opens.
const SATURATED_REQUEST_BUDGET: Duration = Duration::from_millis(50);

/// Most connections served on their own thread at once.
///
/// A ceiling on THREADS, not on connections: nothing is ever dropped
/// unread. Beyond this the connection is served inline on the accept loop
/// with [`SATURATED_REQUEST_BUDGET`] instead, because a dropped connection
/// may BE the callback - a browser does not retry a top-level navigation
/// that was accepted and then closed without an answer, and even if it did,
/// a saturated listener would drop the retry too.
const MAX_LIVE_HANDLERS: usize = 64;

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

/// A bound loopback listener plus the exact redirect URI it answers on.
#[derive(Debug)]
pub struct CallbackServer {
    listener: TcpListener,
    redirect_uri: String,
    /// Connections currently being served on their own thread.
    live: Arc<AtomicUsize>,
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
            live: Arc::new(AtomicUsize::new(0)),
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
    ///
    /// **Each connection is served on its own thread.** Reading a request
    /// serially was the flaw: a peer that completes a TCP handshake and
    /// then sends nothing holds the single reader for its whole read
    /// window, and the browser's redirect waits behind it in the accept
    /// queue. At the previous 10s window, 24 silent connections were enough
    /// to consume a 240s login - and no privilege is needed, since
    /// 127.0.0.1 accepts connections from every user on the machine.
    /// Ordinary browsers cause a milder version of the same thing with
    /// prefetch and keep-alive connections that go quiet.
    ///
    /// Concurrency removes the head-of-line blocking outright: a stalled
    /// connection occupies one short-lived thread and nothing else, and the
    /// real redirect is handled the moment it arrives.
    ///
    /// Concurrency alone was not enough, though, and the two gaps are worth
    /// naming because both reproduced the original symptom by another
    /// route. A per-READ timeout let a peer drip-feed bytes and hold a
    /// handler indefinitely, so the thread pool never freed - fixed by
    /// budgeting the request line as a whole ([`REQUEST_BUDGET`]). And when
    /// the pool was full, connections were DROPPED unread - and the
    /// connection over the limit can be the callback - fixed by serving
    /// those inline instead ([`CallbackServer::serve`]).
    ///
    /// What holds now: a flood can delay the callback, bounded by the
    /// kernel's listen backlog times [`SATURATED_REQUEST_BUDGET`], but it
    /// cannot make the callback go unread.
    pub fn wait_for_code(&self, state: &str, timeout: Duration) -> Result<String, OAuthError> {
        let deadline = Instant::now() + timeout;
        let (sender, outcomes) = mpsc::channel();
        while Instant::now() < deadline {
            if let Some(outcome) = collect(&outcomes) {
                return outcome;
            }
            let Some(stream) = self.accept_now()? else {
                std::thread::sleep(POLL_INTERVAL);
                continue;
            };
            self.serve(stream, state, &sender);
        }
        // Drain anything a handler produced in the final moments.
        collect(&outcomes).unwrap_or(Err(OAuthError::CallbackTimeout {
            seconds: timeout.as_secs(),
        }))
    }

    /// Serve one connection, on a worker thread if there is room.
    ///
    /// **Nothing is ever dropped unread.** At capacity - or if a thread
    /// cannot be spawned - the connection is served INLINE with the much
    /// smaller [`SATURATED_REQUEST_BUDGET`]. That keeps three properties at
    /// once: threads stay bounded, the accept loop keeps draining, and a
    /// callback is always read.
    ///
    /// The worst a flood can do is delay the callback by the number of
    /// connections queued ahead of it times that small budget, bounded in
    /// turn by the kernel's listen backlog. It cannot make the callback
    /// disappear, which is what dropping did.
    fn serve(
        &self,
        stream: TcpStream,
        state: &str,
        sender: &mpsc::Sender<Result<String, OAuthError>>,
    ) {
        if self.live.load(Ordering::SeqCst) < MAX_LIVE_HANDLERS
            && self.spawn(&stream, state, sender)
        {
            return;
        }
        if let Some(outcome) = handle(&stream, state, SATURATED_REQUEST_BUDGET) {
            let _ = sender.send(outcome);
        }
    }

    /// Try to hand a connection to a worker thread; `false` if not spawned.
    fn spawn(
        &self,
        stream: &TcpStream,
        state: &str,
        sender: &mpsc::Sender<Result<String, OAuthError>>,
    ) -> bool {
        let Ok(stream) = stream.try_clone() else {
            return false;
        };
        self.live.fetch_add(1, Ordering::SeqCst);
        let expected = state.to_string();
        let sender = sender.clone();
        let held = Arc::clone(&self.live);
        let spawned = std::thread::Builder::new()
            .name("otl-oauth-callback".to_string())
            .spawn(move || {
                if let Some(outcome) = handle(&stream, &expected, REQUEST_BUDGET) {
                    // A departed receiver just means the login already
                    // finished; there is nothing left to report to.
                    let _ = sender.send(outcome);
                }
                held.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.live.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// How many connections are being served on their own thread.
    #[cfg(test)]
    fn live_handlers(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Accept a connection if one is waiting, without blocking.
    fn accept_now(&self) -> Result<Option<TcpStream>, OAuthError> {
        match self.listener.accept() {
            Ok((stream, _)) => {
                // Blocking from here on; how long is decided per read by
                // the request-line budget, not by one fixed timeout.
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_write_timeout(Some(REQUEST_BUDGET));
                Ok(Some(stream))
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(OAuthError::Callback {
                reason: format!("cannot accept the browser redirect ({error})"),
            }),
        }
    }
}

/// Take the first outcome a handler has produced, if any.
fn collect(
    outcomes: &mpsc::Receiver<Result<String, OAuthError>>,
) -> Option<Result<String, OAuthError>> {
    outcomes.try_recv().ok()
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

    /// Wait for the handler count to reach `want`, up to `budget`.
    ///
    /// Polled rather than slept: what these tests assert is that the count
    /// DOES reach a value, and a fixed sleep turns that into an assertion
    /// about machine speed - which is how timing tests become flaky on a
    /// loaded CI box.
    fn await_handlers(server: &CallbackServer, want: usize, budget: Duration) -> usize {
        let deadline = Instant::now() + budget;
        loop {
            let live = server.live_handlers();
            if live == want || Instant::now() >= deadline {
                return live;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// A peer that holds a connection open forever, drip-feeding bytes so
    /// the socket never goes idle but the request line never completes.
    ///
    /// This is the shape that matters: a per-READ timeout never fires
    /// against it, because every read returns data. Only a total budget for
    /// the whole request line can end it.
    fn trickle(port: u16, stop: &Arc<std::sync::atomic::AtomicBool>) -> Option<TcpStream> {
        use std::io::Write as _;

        let stream = TcpStream::connect((CALLBACK_HOST, port)).ok()?;
        let mut feed = stream.try_clone().ok()?;
        let stop = Arc::clone(stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if feed.write_all(b"x").is_err() || feed.flush().is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        Some(stream)
    }

    /// Send a complete, valid callback request and keep the socket alive
    /// long enough for the answer to be written.
    fn send_callback(port: u16) {
        use std::io::Write as _;

        if let Ok(mut stream) = TcpStream::connect((CALLBACK_HOST, port)) {
            let _ = stream.write_all(
                b"GET /callback?code=real-code&state=state HTTP/1.1\r\n\
                  Host: 127.0.0.1\r\n\r\n",
            );
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(600));
        }
    }

    #[test]
    fn a_saturating_flood_of_trickling_peers_cannot_block_the_redirect() {
        // R4 [N1], the reviewer's attack. Concurrency fixed head-of-line
        // blocking but the handler cap reintroduced the same consequence:
        // over the cap a connection was dropped UNREAD, and the real
        // callback can be the connection over the cap. Combined with a
        // per-read (rather than total) timeout, a byte-trickling peer held
        // a slot indefinitely, so the cap never freed.
        //
        // More peers than MAX_LIVE_HANDLERS on purpose: the previous test
        // used 8, which by construction never reached the cap branch at
        // all, and so could not observe this.
        use std::sync::atomic::AtomicBool;

        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port = port_of(server.redirect_uri()).expect("a bound port");

        let stop = Arc::new(AtomicBool::new(false));
        let held: Vec<TcpStream> = (0..MAX_LIVE_HANDLERS + 16)
            .filter_map(|_| trickle(port, &stop))
            .collect();
        assert!(
            held.len() >= MAX_LIVE_HANDLERS,
            "could not saturate the handler cap: only {} peers connected",
            held.len()
        );

        std::thread::spawn(move || {
            // Well after every slot is occupied.
            std::thread::sleep(Duration::from_millis(900));
            send_callback(port);
        });

        let outcome = server.wait_for_code("state", Duration::from_secs(20));
        stop.store(true, Ordering::Relaxed);
        drop(held);
        assert_eq!(
            outcome.ok().as_deref(),
            Some("real-code"),
            "a local process saturated the listener and the real callback \
             never got served"
        );
    }

    #[test]
    fn one_peer_cannot_hold_a_handler_slot_beyond_the_request_budget() {
        // The other half of [N1]: `READ_TIMEOUT` was a per-read timeout, so
        // a peer sending one byte every second kept every read succeeding
        // and held its handler for as long as it liked - about two and a
        // half hours at the 8 KiB line cap. A request line needs a TOTAL
        // budget.
        //
        // The accept loop has to be RUNNING for this to mean anything: a
        // first version of this test checked the handler count without ever
        // calling `wait_for_code`, so nothing was ever accepted and the
        // assertion passed against the bug it was meant to catch.
        use std::sync::atomic::AtomicBool;

        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port = port_of(server.redirect_uri()).expect("a bound port");
        let stop = Arc::new(AtomicBool::new(false));

        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Long enough to outlive the observation below.
                let _ = server.wait_for_code("state", REQUEST_BUDGET * 4);
            });

            let held = trickle(port, &stop).expect("local connect");
            assert_eq!(
                await_handlers(&server, 1, Duration::from_secs(2)),
                1,
                "the trickling peer was never accepted, so this test would \
                 prove nothing"
            );

            // The handler must give up on it despite bytes still arriving
            // every 250ms. Generous budget: what is asserted is that the
            // slot is released at all, not how fast.
            assert_eq!(
                await_handlers(&server, 0, REQUEST_BUDGET * 3),
                0,
                "a trickling peer is still holding a handler slot long after \
                 the request budget elapsed"
            );

            stop.store(true, Ordering::Relaxed);
            drop(held);
        });
    }

    #[test]
    fn stalled_connections_cannot_starve_the_real_redirect() {
        // The R3 finding, reproduced. With a serial reader, a peer that
        // completes a handshake and then sends NOTHING held the single
        // reader for its whole read window; the browser's redirect waited
        // behind it. At a 10s window, 24 silent connections consumed a 240s
        // login - from any unprivileged local process, since 127.0.0.1
        // accepts from every user on the machine.
        //
        // The reviewer's harness showed 3 stalled connections defeating a
        // 25s budget. This asserts the opposite now holds, with a budget
        // far smaller than the stalled connections would previously have
        // consumed.
        use std::io::Write as _;
        use std::net::TcpStream;

        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port = port_of(server.redirect_uri()).expect("a bound port");

        // Hold them open for the whole test: silent, never closed.
        let stalled: Vec<TcpStream> = (0..8)
            .filter_map(|_| TcpStream::connect((CALLBACK_HOST, port)).ok())
            .collect();
        assert_eq!(stalled.len(), 8, "could not set up the stalled peers");

        std::thread::spawn(move || {
            // Arrives after the silent peers are already queued.
            std::thread::sleep(Duration::from_millis(150));
            if let Ok(mut stream) = TcpStream::connect((CALLBACK_HOST, port)) {
                let _ = stream.write_all(
                    b"GET /callback?code=real-code&state=state HTTP/1.1\r\n\
                      Host: 127.0.0.1\r\n\r\n",
                );
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(500));
            }
        });

        // The proof is arithmetic, not wall-clock: serving these serially
        // would cost stalled x REQUEST_BUDGET, which must exceed the budget
        // below - so succeeding at all means the redirect was served
        // concurrently rather than behind them.
        let budget = Duration::from_secs(6);
        assert!(
            REQUEST_BUDGET * stalled.len() as u32 > budget,
            "the budget is too generous for this to prove anything"
        );
        let code = server
            .wait_for_code("state", budget)
            .expect("a silent peer must not starve the real redirect");
        assert_eq!(code, "real-code");
        drop(stalled);
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
            elapsed < budget + REQUEST_BUDGET,
            "a stalled connection extended the wait to {elapsed:?}, well past \
             its {budget:?} budget"
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
    fn valid_non_callback_requests_do_not_consume_the_login() {
        // The specific case a fixed connection budget got wrong: these are
        // well-formed GETs that simply are not the callback (a browser
        // sends them, and so can any local process). They must be answered
        // and ignored without ever ending the wait early - only the
        // deadline may do that.
        use std::io::{Read as _, Write as _};
        use std::net::TcpStream;

        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let port = port_of(server.redirect_uri()).expect("a bound port");

        std::thread::spawn(move || {
            let stray = |path: &str| {
                if let Ok(mut stream) = TcpStream::connect((CALLBACK_HOST, port)) {
                    let _ = stream.write_all(
                        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes(),
                    );
                    let _ = stream.flush();
                    // Read the answer so the exchange completes normally.
                    let mut sink = [0_u8; 64];
                    let _ = stream.read(&mut sink);
                }
            };
            for _ in 0..60 {
                stray("/favicon.ico");
            }
            if let Ok(mut stream) = TcpStream::connect((CALLBACK_HOST, port)) {
                let _ = stream.write_all(
                    b"GET /callback?code=real-code&state=state HTTP/1.1\r\n\
                      Host: 127.0.0.1\r\n\r\n",
                );
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(400));
            }
        });

        let code = server
            .wait_for_code("state", Duration::from_secs(20))
            .expect("stray GETs must not defeat the real redirect");
        assert_eq!(code, "real-code");
    }

    #[test]
    fn the_wait_is_bounded_by_the_deadline_alone() {
        // There is no connection budget left to exhaust: the loop ends when
        // the deadline passes, and the mandatory pause after every
        // unproductive connection is what bounds the iteration count.
        let server = CallbackServer::bind_ephemeral().expect("loopback bind");
        let budget = Duration::from_millis(200);
        let started = Instant::now();
        let error = server
            .wait_for_code("state", budget)
            .expect_err("no redirect is coming");
        assert!(
            matches!(error, OAuthError::CallbackTimeout { .. }),
            "{error:?}"
        );
        assert!(
            started.elapsed() < budget + REQUEST_BUDGET,
            "the wait overran its budget: {:?}",
            started.elapsed()
        );
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
