//! Where the agent skill is installed, and what is installed there.
//!
//! Two rules shape this module, and both are about not inventing state.
//!
//! 1. **Only existing agent homes are written to.** `~/.claude` is created
//!    by Claude Code, `~/.codex` by Codex; creating one here would leave a
//!    directory tree for an agent the user does not run, and the next
//!    `otl doctor` would then report a skill nobody can use. So the default
//!    target list is derived from what is already on the machine, and when
//!    nothing is, the command says so instead of guessing.
//! 2. **What is already installed is inspected, never assumed.** An
//!    installed copy is a plain file a user may have edited, replaced with
//!    a symlink, or filled with a different skill entirely. Each of those
//!    is a distinct answer here, because they have distinct remedies.
//!
//! Paths go through `directories`, never through a hand-built `$HOME/...`
//! string - the same rule the credential paths follow.

use std::env;
use std::path::{Path, PathBuf};

/// Environment variable that overrides the skills directory.
///
/// One directory, not a list: it exists so a container with an unusual
/// layout - and every test in this repository - can point the install
/// somewhere else without touching `HOME`.
pub const ENV_SKILL_DIR: &str = "OUTLINE_SKILL_DIR";

/// File name every agent looks for inside a skill directory.
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// Directory holding an agent's skills, inside that agent's home.
const SKILLS_DIR_NAME: &str = "skills";

/// Agent homes under the user's home directory, and the agent each serves.
///
/// Ordered as they are reported. `.agents` is last because it is the
/// convention several tools share rather than one tool's own directory.
const AGENT_HOMES: &[(&str, &str)] = &[
    (".claude", "Claude Code"),
    (".codex", "Codex"),
    (".agents", "agent-agnostic"),
];

/// Largest installed copy that will be read back for comparison.
///
/// An installed copy is authored by this CLI, so anything remotely this
/// size is not one - and a `SKILL.md` that is really a multi-gigabyte file
/// must not be read into memory to find out.
const MAX_INSTALLED_BYTES: u64 = 1 << 20;

/// Why a skills directory is a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An agent's own skills directory, found on this machine.
    Agent(&'static str),
    /// The directory named by `--dir`.
    Flag,
    /// The directory named by [`ENV_SKILL_DIR`].
    Env,
}

impl Source {
    /// How this source is named in output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent(agent) => agent,
            Self::Flag => "--dir",
            Self::Env => ENV_SKILL_DIR,
        }
    }
}

/// One skills directory the document can be installed into.
#[derive(Debug, Clone)]
pub struct Target {
    /// The skills directory itself, which holds one directory per skill.
    pub root: PathBuf,
    /// Why this directory is a target.
    pub source: Source,
}

impl Target {
    /// The directory this one skill owns.
    pub fn dir(&self) -> PathBuf {
        self.root.join(super::SKILL_NAME)
    }

    /// The document itself.
    pub fn file(&self) -> PathBuf {
        self.dir().join(SKILL_FILE_NAME)
    }
}

/// Every skills directory this machine should hold the document in.
///
/// An explicit directory - from `--dir` or [`ENV_SKILL_DIR`] - is the whole
/// answer: someone who names a directory is not also asking for two others.
/// Otherwise every agent home that already exists is a target, so one
/// `otl skill install` covers the agents actually installed here.
///
/// An empty list is a legitimate answer, and its meaning depends on the
/// caller: for `otl skill install` it is a usage error naming `--dir`, for
/// `otl doctor` it is a check with nothing to report.
pub fn resolve(explicit: Option<&Path>) -> Vec<Target> {
    if let Some(dir) = explicit {
        return vec![Target {
            root: dir.to_path_buf(),
            source: Source::Flag,
        }];
    }
    if let Some(dir) = non_empty_env(ENV_SKILL_DIR) {
        return vec![Target {
            root: PathBuf::from(dir),
            source: Source::Env,
        }];
    }
    detected()
}

/// The skills directory of every agent home present under `$HOME`.
fn detected() -> Vec<Target> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    AGENT_HOMES
        .iter()
        .filter(|(dir, _)| home.join(dir).is_dir())
        .map(|(dir, agent)| Target {
            root: home.join(dir).join(SKILLS_DIR_NAME),
            source: Source::Agent(agent),
        })
        .collect()
}

/// The user's home directory, or `None` when the platform has none.
fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// What is installed at one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// Nothing is installed there.
    Absent,
    /// This skill, with the version its frontmatter declares and whether
    /// its bytes are already the ones this binary carries.
    Ours {
        version: Option<String>,
        current: bool,
    },
    /// A `SKILL.md` belonging to some other skill.
    Foreign { name: Option<String> },
    /// Nothing usable is there, and `reason` is a full sentence INCLUDING
    /// its own remedy - which differs case by case, so a single suffix
    /// appended by the caller would be wrong for most of them.
    ///
    /// Never written to, not even with `--force`: every case here is a
    /// situation to look at rather than to overwrite, and following a
    /// symlink with a write is how an install becomes a write elsewhere.
    Unusable { reason: String },
}

/// Examine the copy installed at one target, and the path leading to it.
///
/// Three things are checked, in the order a caller can act on them: the
/// skills directory it was pointed at, the directory this skill owns, and
/// the document. Each uses `symlink_metadata`, never `metadata`: a symlink
/// must be REPORTED rather than resolved and then written through.
///
/// The asymmetry between the two directories is deliberate. The skills root
/// may be a symlink - it is the directory the user named or the one an
/// agent created, and pointing it at a dotfiles checkout is a normal thing
/// to do. `<root>/<skill>` is a directory this command creates and owns, so
/// a symlink there redirects a write this command believes is local.
pub fn inspect(target: &Target) -> Installed {
    if let Some(problem) = examine_root(&target.root) {
        return problem;
    }
    if let Some(problem) = examine_dir(&target.dir()) {
        return problem;
    }
    examine_file(&target.file())
}

/// The skills directory itself: it has to be a directory, or nothing below
/// it can exist.
fn examine_root(root: &Path) -> Option<Installed> {
    match std::fs::metadata(root) {
        // Absent is fine: the install creates it inside an existing agent
        // home, and `--dir` may legitimately name a directory to be made.
        Err(_) => None,
        Ok(meta) if meta.is_dir() => None,
        Ok(_) => Some(Installed::Unusable {
            reason: format!(
                "the skills directory {} is not a directory; point --dir at one",
                crate::config::sanitize_path(root)
            ),
        }),
    }
}

/// The directory this skill owns, which this command creates and writes in.
fn examine_dir(dir: &Path) -> Option<Installed> {
    let meta = std::fs::symlink_metadata(dir).ok()?;
    if is_redirect(&meta) {
        return Some(Installed::Unusable {
            reason: format!(
                "{} is a link to somewhere else, and this command will not write \
                 through one; replace it with a real directory",
                crate::config::sanitize_path(dir)
            ),
        });
    }
    if !meta.is_dir() {
        return Some(Installed::Unusable {
            reason: format!(
                "{} is not a directory; remove it by hand",
                crate::config::sanitize_path(dir)
            ),
        });
    }
    None
}

/// The document itself.
fn examine_file(path: &Path) -> Installed {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Installed::Absent,
        // `kind()`, not the error: the raw form carries a platform errno
        // number that means nothing to the reader of a report.
        Err(error) => {
            return Installed::Unusable {
                reason: format!(
                    "the installed document cannot be examined: {}",
                    error.kind()
                ),
            }
        }
    };
    if !meta.is_file() {
        return Installed::Unusable {
            reason: "the installed document is not a regular file; remove it by hand".to_string(),
        };
    }
    if meta.len() > MAX_INSTALLED_BYTES {
        return Installed::Unusable {
            reason: format!(
                "the installed document is larger than {MAX_INSTALLED_BYTES} bytes, \
                 so it is not one this CLI wrote; remove it by hand"
            ),
        };
    }
    let document = match std::fs::read_to_string(path) {
        Ok(document) => document,
        Err(error) => {
            return Installed::Unusable {
                reason: format!("the installed document cannot be read: {}", error.kind()),
            }
        }
    };
    match frontmatter_value(&document, "name") {
        Some(name) if name == super::SKILL_NAME => Installed::Ours {
            version: frontmatter_value(&document, "version"),
            current: document == super::SKILL_MARKDOWN,
        },
        name => Installed::Foreign { name },
    }
}

/// One top-level frontmatter value of a skill document.
///
/// The authoring side of this rule is in `build.rs`, and both halves read
/// only UNINDENTED lines of the leading fenced block: a nested `metadata:`
/// entry called `version` must not be able to answer for the document.
///
/// The value is SANITIZED, because for an installed copy it is foreign text
/// that reaches a report: the same rule as every other name this CLI echoes
/// (control characters replaced, length capped). Nothing is lost for a
/// document this CLI wrote - `build.rs` accepts neither in its own
/// frontmatter.
pub fn frontmatter_value(document: &str, key: &str) -> Option<String> {
    let mut lines = document.lines();
    if lines.next()? != FRONTMATTER_FENCE {
        return None;
    }
    let prefix = format!("{key}:");
    for line in lines {
        if line == FRONTMATTER_FENCE {
            return None;
        }
        if let Some(value) = line.strip_prefix(&prefix) {
            let value = value.trim().trim_matches('"').trim();
            return (!value.is_empty()).then(|| crate::config::sanitize_name(value));
        }
    }
    None
}

/// Whether this metadata describes a path that redirects elsewhere.
///
/// `is_symlink()` alone is not the question on Windows: a directory
/// JUNCTION reports as an ordinary directory there, so a junction planted
/// at `<skills dir>/<skill>` would be followed by the write while a symlink
/// would not. Both are reparse points, which is what the Windows branch
/// looks at - the platform is answered explicitly rather than assumed to
/// behave like Unix, per `project-context.md`.
pub fn is_redirect(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink() || is_reparse_point(meta)
}

/// Whether a path is a Windows reparse point that `is_symlink()` misses -
/// a junction or a mount point.
///
/// Two functions rather than one with an inner `cfg` block: the branch has
/// to be an expression on both platforms, and `scripts/win-check.sh`
/// (the only gate that compiles the Windows branch at all) rejected the
/// early-return form that made the Unix side read oddly.
#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    /// `FILE_ATTRIBUTE_REPARSE_POINT`, which covers junctions and mount
    /// points as well as symlinks.
    const REPARSE_POINT: u32 = 0x0400;

    meta.file_attributes() & REPARSE_POINT != 0
}

/// On Unix a symlink is the only redirect a path can be, and
/// [`is_redirect`] has already answered that.
#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Frontmatter delimiter line.
const FRONTMATTER_FENCE: &str = "---";

/// Read an environment variable, treating a blank value as unset.
fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_named_directory_is_the_whole_target_list() {
        let dir = Path::new("/tmp/skills");
        let targets = resolve(Some(dir));
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].root, dir);
        assert_eq!(targets[0].source, Source::Flag);
        assert_eq!(
            targets[0].file(),
            dir.join(super::super::SKILL_NAME).join(SKILL_FILE_NAME)
        );
    }

    #[test]
    fn frontmatter_reads_only_unindented_keys_of_the_leading_block() {
        let document = "---\nname: outline-cli\nversion: 1.2.3\nmetadata:\n  version: 9.9.9\n---\n\n# Title\nversion: 0.0.0\n";
        assert_eq!(
            frontmatter_value(document, "name").as_deref(),
            Some("outline-cli")
        );
        assert_eq!(
            frontmatter_value(document, "version").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(frontmatter_value(document, "license"), None);
    }

    #[test]
    fn a_document_without_frontmatter_declares_nothing() {
        assert_eq!(frontmatter_value("# Title\nname: x\n", "name"), None);
        assert_eq!(frontmatter_value("", "name"), None);
        // A quoted value is unquoted; a blank one counts as absent.
        assert_eq!(
            frontmatter_value("---\nname: \"quoted\"\n---\n", "name").as_deref(),
            Some("quoted")
        );
        assert_eq!(frontmatter_value("---\nname:\n---\n", "name"), None);
    }

    #[test]
    fn the_bundled_document_describes_itself() {
        assert_eq!(
            frontmatter_value(super::super::SKILL_MARKDOWN, "name").as_deref(),
            Some(super::super::SKILL_NAME)
        );
        assert_eq!(
            frontmatter_value(super::super::SKILL_MARKDOWN, "version").as_deref(),
            Some(super::super::SKILL_VERSION)
        );
    }

    #[test]
    fn nothing_is_installed_in_an_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target {
            root: dir.path().to_path_buf(),
            source: Source::Flag,
        };
        assert_eq!(inspect(&target), Installed::Absent);
    }

    #[test]
    fn an_installed_copy_is_recognized_by_its_own_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target {
            root: dir.path().to_path_buf(),
            source: Source::Flag,
        };
        std::fs::create_dir_all(target.dir()).unwrap();

        std::fs::write(target.file(), super::super::SKILL_MARKDOWN).unwrap();
        assert_eq!(
            inspect(&target),
            Installed::Ours {
                version: Some(super::super::SKILL_VERSION.to_string()),
                current: true,
            }
        );

        let older = super::super::SKILL_MARKDOWN.replace(
            &format!("version: {}", super::super::SKILL_VERSION),
            "version: 0.0.1",
        );
        std::fs::write(target.file(), older).unwrap();
        assert_eq!(
            inspect(&target),
            Installed::Ours {
                version: Some("0.0.1".to_string()),
                current: false,
            }
        );

        std::fs::write(target.file(), "---\nname: something-else\n---\n").unwrap();
        assert_eq!(
            inspect(&target),
            Installed::Foreign {
                name: Some("something-else".to_string()),
            }
        );
    }

    /// A symlink at the document's path is reported as unusable rather than
    /// resolved: writing through it is how a link becomes a write anywhere.
    #[cfg(unix)]
    #[test]
    fn a_symlink_is_never_treated_as_an_installed_copy() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target {
            root: dir.path().to_path_buf(),
            source: Source::Flag,
        };
        std::fs::create_dir_all(target.dir()).unwrap();
        let elsewhere = dir.path().join("elsewhere.md");
        std::fs::write(&elsewhere, super::super::SKILL_MARKDOWN).unwrap();
        std::os::unix::fs::symlink(&elsewhere, target.file()).unwrap();
        assert!(matches!(inspect(&target), Installed::Unusable { .. }));
    }
}
