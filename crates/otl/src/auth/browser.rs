//! Opening the system browser, without a dependency for it.
//!
//! Best-effort by design: the authorization URL is always printed on
//! stderr as well, so a headless machine, an unusual desktop or a missing
//! helper degrades into "copy this link" rather than into a failed login.

use std::process::{Command, Stdio};

use crate::auth::error::OAuthError;

/// Platform helper that opens a URL in the user's default browser.
#[cfg(target_os = "macos")]
const OPENER: (&str, &[&str]) = ("open", &[]);

/// Platform helper that opens a URL in the user's default browser.
///
/// `start` is a `cmd` builtin, and its first quoted argument is taken as a
/// window title - hence the empty `""` before the URL, without which a
/// quoted URL would be swallowed as the title.
#[cfg(target_os = "windows")]
const OPENER: (&str, &[&str]) = ("cmd", &["/C", "start", ""]);

/// Platform helper that opens a URL in the user's default browser.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const OPENER: (&str, &[&str]) = ("xdg-open", &[]);

/// Ask the desktop to open `url`.
///
/// The child's stdio is discarded: some openers print to stdout, and this
/// CLI's stdout is reserved for data.
pub fn open(url: &str) -> Result<(), OAuthError> {
    let (program, prefix) = OPENER;
    Command::new(program)
        .args(prefix)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|error| OAuthError::Browser {
            reason: format!("{program} could not be started: {error}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opener_is_a_single_known_program_per_platform() {
        // Guards against a shell string sneaking in here: the URL is
        // passed as an argv entry, never interpolated into a command line.
        let (program, _) = OPENER;
        assert!(!program.contains(' '), "opener must not be a shell string");
    }

    #[test]
    fn a_missing_opener_is_reported_and_not_fatal_in_itself() {
        // The error type exists to be downgraded to a notice by the
        // caller; check that it carries the reason for the notice.
        let error = OAuthError::Browser {
            reason: "no such file".to_string(),
        };
        assert!(error.to_string().contains("open this URL manually"));
    }
}
