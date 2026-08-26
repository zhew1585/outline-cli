//! Opening a URL in the user's browser (`otl docs view --web`).
//!
//! The URL is always passed as a single argv entry to an opener program -
//! never through a shell, and never concatenated into a command string - so
//! a server-provided path cannot turn into a second command. Callers must
//! additionally have validated the URL's shape (see
//! [`crate::session::Session::absolute_url`]).

use std::ffi::OsString;
use std::process::{Command, Stdio};

use anyhow::anyhow;

use crate::exit::CliError;

/// Environment variable naming a preferred opener, honored on every
/// platform (the de-facto `$BROWSER` convention).
pub const ENV_BROWSER: &str = "BROWSER";

/// The opener command for a URL: a program and its leading arguments.
///
/// The URL itself is appended as the final argument by [`open`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    /// Program to run.
    pub program: OsString,
    /// Arguments that precede the URL.
    pub args: Vec<OsString>,
}

impl Opener {
    /// The opener to use: `$BROWSER` when set, else the platform default.
    pub fn from_env() -> Self {
        Self::resolve(std::env::var_os(ENV_BROWSER))
    }

    /// [`Opener::from_env`] with the environment value supplied explicitly.
    pub fn resolve(browser: Option<OsString>) -> Self {
        match browser.and_then(Self::parse) {
            Some(opener) => opener,
            None => Self::platform_default(),
        }
    }

    /// Parse a `$BROWSER` value, or `None` when it is empty.
    ///
    /// Split on ASCII whitespace, no shell: the value may carry flags
    /// (`BROWSER="firefox --new-window"`) but not a pipeline.
    fn parse(value: OsString) -> Option<Self> {
        let Some(text) = value.to_str() else {
            return Some(Self {
                program: value,
                args: Vec::new(),
            });
        };
        let mut words = text.split_whitespace();
        let program = words.next()?;
        Some(Self {
            program: OsString::from(program),
            args: words.map(OsString::from).collect(),
        })
    }

    /// The platform's URL opener.
    ///
    /// Windows deliberately does NOT use `cmd /c start`: `cmd` re-parses its
    /// arguments, so `&` or `|` inside a URL would become command
    /// separators. `rundll32 url.dll,FileProtocolHandler` hands the URL to
    /// the registered handler without a shell in between.
    fn platform_default() -> Self {
        let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("open", &[])
        } else if cfg!(target_os = "windows") {
            ("rundll32", &["url.dll,FileProtocolHandler"])
        } else {
            ("xdg-open", &[])
        };
        Self {
            program: OsString::from(program),
            args: args.iter().map(OsString::from).collect(),
        }
    }
}

/// Open `url` with the resolved opener.
///
/// The opener's own stdout and stderr are discarded: `xdg-open` and friends
/// are chatty, and their output would pollute the data stream. Failure to
/// launch is a real error (exit code 1) - the caller is expected to have
/// printed the URL first, so the user can still open it by hand.
pub fn open(url: &str) -> Result<(), CliError> {
    launch(&Opener::from_env(), url)
}

/// Spawn one opener for `url`.
fn launch(opener: &Opener, url: &str) -> Result<(), CliError> {
    let status = Command::new(&opener.program)
        .args(&opener.args)
        // The URL is one argument, always last, never parsed by a shell.
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            CliError::failure(anyhow!(
                "could not launch {} to open the document ({}); \
                 set {ENV_BROWSER} to a command that opens a URL",
                opener.program.to_string_lossy(),
                error.kind()
            ))
        })?;
    if !status.success() {
        return Err(CliError::failure(anyhow!(
            "{} exited without opening the document",
            opener.program.to_string_lossy()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn browser_env_overrides_the_platform_default() {
        let opener = Opener::resolve(Some(OsString::from("firefox --new-window")));
        assert_eq!(opener.program, OsString::from("firefox"));
        assert_eq!(opener.args, vec![OsString::from("--new-window")]);
    }

    #[test]
    fn empty_browser_env_falls_back_to_the_platform_default() {
        let opener = Opener::resolve(Some(OsString::from("  ")));
        assert_eq!(opener, Opener::platform_default());
        assert_eq!(Opener::resolve(None), Opener::platform_default());
    }

    #[test]
    fn windows_default_avoids_the_shell() {
        // Regression guard: `cmd /c start` would re-parse `&` in a URL.
        let opener = Opener::platform_default();
        assert_ne!(opener.program, OsString::from("cmd"));
    }

    #[test]
    fn a_missing_opener_is_an_error_not_a_panic() {
        let opener = Opener {
            program: OsString::from("otl-no-such-browser-9c7a"),
            args: Vec::new(),
        };
        let error = launch(&opener, "https://example.com/doc/x").unwrap_err();
        assert_eq!(error.code, crate::exit::ExitCode::Failure);
    }

    #[cfg(unix)]
    #[test]
    fn passes_the_url_as_a_single_argument() {
        // `sh -c 'test "$1" = <url>' --` succeeds only if the URL arrived
        // whole, as one argv entry.
        let url = "https://example.com/doc/a&b c";
        let opener = Opener {
            program: OsString::from("sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(format!("test \"$1\" = '{url}'")),
                OsString::from("sh"),
            ],
        };
        assert!(launch(&opener, url).is_ok());
    }
}
