//! Shared platform paths for user-owned configuration.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
use directories::BaseDirs;
#[cfg(not(target_os = "macos"))]
use directories::ProjectDirs;

/// Return the directory that holds shareable configuration and credentials.
///
/// macOS deliberately follows the same XDG-style layout as Linux. This keeps
/// `otl` configuration in `~/.config` instead of `~/Library/Application
/// Support`, while Windows continues to use its native configuration root.
pub(crate) fn config_dir(app_name: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        BaseDirs::new().map(|dirs| dirs.home_dir().join(".config").join(app_name))
    }

    #[cfg(not(target_os = "macos"))]
    {
        ProjectDirs::from("", "", app_name).map(|dirs| dirs.config_dir().to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_configuration_uses_dot_config_under_home() {
        let home = match directories::BaseDirs::new() {
            Some(home) => home,
            None => panic!("macOS has no home directory"),
        };
        assert_eq!(
            super::config_dir("outline-cli"),
            Some(home.home_dir().join(".config/outline-cli"))
        );
    }
}
