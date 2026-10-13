//! Loader and matcher for `.tinyignore` files.
//!
//! Supported pattern syntax (intentionally a small subset of gitignore):
//!
//! - `# comment`           — line is ignored
//! - `dirname`             — skip any directory whose basename matches
//! - `*.ext`               — skip any file whose extension matches
//! - blank lines           — ignored
//!
//! Lookup order: each scan root is checked for `.tinyignore`, then the user's
//! `$HOME` is checked once. All loaded patterns are merged into a single
//! ruleset that applies to the entire scan.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct IgnoreRules {
    dir_names: Vec<String>,
    file_extensions: Vec<String>,
}

impl IgnoreRules {
    pub fn load(roots: &[PathBuf], home: Option<&Path>) -> Self {
        let mut rules = IgnoreRules::default();

        for root in roots {
            rules.merge_file(&root.join(".tinyignore"));
        }
        if let Some(h) = home {
            rules.merge_file(&h.join(".tinyignore"));
        }
        rules
    }

    #[cfg(test)]
    fn from_str(text: &str) -> Self {
        let mut rules = IgnoreRules::default();
        rules.merge_text(text);
        rules
    }

    fn merge_file(&mut self, path: &Path) {
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        self.merge_text(&text);
    }

    fn merge_text(&mut self, text: &str) {
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(ext) = line.strip_prefix("*.") {
                if !ext.is_empty() && !self.file_extensions.iter().any(|e| e == ext) {
                    self.file_extensions.push(ext.to_string());
                }
                continue;
            }
            if !self.dir_names.iter().any(|d| d == line) {
                self.dir_names.push(line.to_string());
            }
        }
    }

    pub fn skip_dir(&self, name: &str) -> bool {
        self.dir_names.iter().any(|d| d == name)
    }

    pub fn skip_file(&self, name: &str) -> bool {
        let ext = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext.is_empty() {
            return false;
        }
        self.file_extensions.iter().any(|e| e == ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dirname_and_ext_patterns() {
        let rules = IgnoreRules::from_str(
            "# header\n\
             node_modules\n\
             *.log\n\
             \n\
             vendor\n\
             *.tmp\n",
        );
        assert!(rules.skip_dir("node_modules"));
        assert!(rules.skip_dir("vendor"));
        assert!(!rules.skip_dir("src"));
        assert!(rules.skip_file("server.log"));
        assert!(rules.skip_file("scratch.tmp"));
        assert!(!rules.skip_file("readme.md"));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let rules = IgnoreRules::from_str("\n\n# nothing\n   \n");
        assert!(!rules.skip_dir("anything"));
        assert!(!rules.skip_file("anything.txt"));
    }

    #[test]
    fn extension_match_is_case_sensitive_and_skips_unknown() {
        let rules = IgnoreRules::from_str("*.log");
        assert!(rules.skip_file("a.log"));
        assert!(!rules.skip_file("a.LOG"));
        assert!(!rules.skip_file("a"));
    }
}
