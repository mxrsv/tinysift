//! `brew.env` files that can undo the environment `brew` is run with.
//!
//! `bin/brew` reads three files after it starts and exports what they set
//! over the caller's environment, so a cleared one does not win: the system
//! file, the prefix file and the user's `~/.homebrew/brew.env`. A user who
//! turned on greedy upgrades there would make `brew outdated` list apps that
//! updated themselves, and one who set `HOMEBREW_NO_SUDO` or
//! `HOMEBREW_NO_AUTO_UPDATE` could change what this module's callers rely on.
//! Core cannot stop that, so a preview carries the file and the variable.
//!
//! The same files can also set `http_proxy`, `https_proxy`, `all_proxy` and
//! `no_proxy`, which `bin/brew` loads with the rest. Those are harmless here and
//! are not reported: a proxy the process itself has is passed on to `brew`
//! (`ToolLookup::isolated`), and one in a file is applied by `brew` over it.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// More than any real `brew.env`; the rest of a larger file is not read.
const READ_LIMIT: u64 = 64 * 1024;

/// Variables the cask operations rely on. Matched on the name before `=`.
fn matters(name: &str) -> bool {
    name.starts_with("HOMEBREW_UPGRADE_GREEDY")
        || name == "HOMEBREW_NO_SUDO"
        || name == "HOMEBREW_NO_AUTO_UPDATE"
}

/// A setting in a `brew.env` file that `brew` would apply over ours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewEnvWarning {
    pub file: PathBuf,
    pub variable: String,
}

/// The files `bin/brew` reads, in the order it reads them. `brew` is the
/// resolved path (`<prefix>/bin/brew`), which names the prefix on Apple
/// silicon and Intel alike. `XDG_CONFIG_HOME` is unset in the cleared
/// environment, so the user's file is `~/.homebrew/brew.env`.
fn env_files(brew: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut files = vec![PathBuf::from("/etc/homebrew/brew.env")];
    if let Some(prefix) = brew.parent().and_then(Path::parent) {
        files.push(prefix.join("etc/homebrew/brew.env"));
    }
    if let Some(home) = home {
        files.push(home.join(".homebrew/brew.env"));
    }
    files
}

/// What the `brew.env` files of this `brew` set that matters, in file order.
/// Unreadable and missing files say nothing, as for `brew` itself.
pub fn brew_env_warnings(brew: &Path, home: Option<&Path>) -> Vec<BrewEnvWarning> {
    env_files(brew, home)
        .into_iter()
        .flat_map(|file| warnings_in(&file))
        .collect()
}

fn warnings_in(file: &Path) -> Vec<BrewEnvWarning> {
    let mut text = String::new();
    let read = File::open(file).and_then(|f| f.take(READ_LIMIT).read_to_string(&mut text));
    if read.is_err() {
        return Vec::new();
    }
    text.lines()
        // `bin/brew` reads each line with `read -r`, which trims its ends.
        .filter_map(|line| line.trim().split_once('='))
        .filter(|(name, _)| matters(name))
        .map(|(name, _)| BrewEnvWarning {
            file: file.to_path_buf(),
            variable: name.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write(dir: &Path, relative: &str, text: &str) {
        let file = dir.join(relative);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    #[test]
    fn the_prefix_and_user_files_are_found_from_the_brew_path_and_home() {
        let silicon = env_files(
            Path::new("/opt/homebrew/bin/brew"),
            Some(Path::new("/Users/a")),
        );
        assert_eq!(
            silicon,
            [
                "/etc/homebrew/brew.env",
                "/opt/homebrew/etc/homebrew/brew.env",
                "/Users/a/.homebrew/brew.env"
            ]
            .map(PathBuf::from)
        );
        let intel = env_files(Path::new("/usr/local/bin/brew"), None);
        assert_eq!(
            intel,
            ["/etc/homebrew/brew.env", "/usr/local/etc/homebrew/brew.env"].map(PathBuf::from)
        );
    }

    #[test]
    fn only_the_settings_that_undo_ours_are_reported_with_their_file() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".homebrew/brew.env",
            "# a comment\n\
             HOMEBREW_UPGRADE_GREEDY=1\n  HOMEBREW_UPGRADE_GREEDY_CASKS=x  \n\
             HOMEBREW_NO_SUDO=0\nHOMEBREW_NO_AUTO_UPDATE=\n\
             HOMEBREW_CASK_OPTS=--no-quarantine\nHOMEBREW_NO_ANALYTICS=1\n\
             HOMEBREW_NO_SUDO_FOR_REAL=1\nHOMEBREW_NO_SUDO\n",
        );
        let file = home.path().join(".homebrew/brew.env");
        let found = warnings_in(&file);
        let variables: Vec<_> = found.iter().map(|w| w.variable.as_str()).collect();
        assert_eq!(
            variables,
            [
                "HOMEBREW_UPGRADE_GREEDY",
                "HOMEBREW_UPGRADE_GREEDY_CASKS",
                "HOMEBREW_NO_SUDO",
                "HOMEBREW_NO_AUTO_UPDATE"
            ]
        );
        assert!(found.iter().all(|w| w.file == file));
    }

    #[test]
    fn a_missing_empty_or_unreadable_file_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "empty.env", "");
        write(dir.path(), "other.env", "HOMEBREW_NO_AUTO_UPDATE=1\n");
        assert!(warnings_in(&dir.path().join("empty.env")).is_empty());
        assert!(warnings_in(&dir.path().join("missing.env")).is_empty());
        assert!(
            warnings_in(dir.path()).is_empty(),
            "a directory is not a file"
        );
        assert_eq!(warnings_in(&dir.path().join("other.env")).len(), 1);
    }
}
