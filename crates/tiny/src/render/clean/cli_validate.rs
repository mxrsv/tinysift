//! Pre-execution validation of CLI flag combinations.
//!
//! The matrix is documented in `docs/plans/2026-04-30-tiny-clean.md`. We
//! refuse early so the user gets a clear error before discovery runs.

use anyhow::{anyhow, Result};

use super::providers::known_category_ids;
use crate::cli::CleanOpts;

pub fn validate(opts: &CleanOpts) -> Result<()> {
    let confirm_hard = std::env::var("TINY_CONFIRM_HARD").as_deref() == Ok("1");
    validate_with_env(opts, confirm_hard)
}

pub fn validate_with_env(opts: &CleanOpts, confirm_hard_env: bool) -> Result<()> {
    // Every named category must be a known canonical id.
    for cat in &opts.category {
        if !known_category_ids().contains(&cat.as_str()) {
            return Err(anyhow!(
                "unknown --category '{}'. valid: {}",
                cat,
                known_category_ids().join(", ")
            ));
        }
    }

    // --include-destructive --hard -y without --category is refused
    // outright, even with TINY_CONFIRM_HARD. Checked before the generic
    // -y/--category rule so the user gets the more specific message.
    if opts.include_destructive && opts.hard && opts.yes && opts.category.is_empty() {
        return Err(anyhow!(
            "--include-destructive --hard --yes requires --category to specify scope"
        ));
    }

    // -y requires --category.
    if opts.yes && opts.category.is_empty() {
        return Err(anyhow!("--yes requires --category to specify scope"));
    }

    // --idle-days 0 is meaningless (would flag everything as idle).
    if opts.idle_days == 0 {
        return Err(anyhow!("--idle-days must be > 0"));
    }

    // --hard -y without TINY_CONFIRM_HARD=1 is refused. The interactive
    // path (no -y) handles confirm via Y/n prompt at action time.
    if opts.hard && opts.yes && !confirm_hard_env {
        return Err(anyhow!(
            "--hard with --yes requires TINY_CONFIRM_HARD=1 in the environment"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CleanOpts;

    fn opts() -> CleanOpts {
        CleanOpts {
            dry_run: false,
            yes: false,
            hard: false,
            category: Vec::new(),
            include_review: false,
            include_destructive: false,
            review_paths: false,
            idle_days: 30,
        }
    }

    #[test]
    fn unknown_category_fails() {
        let mut o = opts();
        o.category = vec!["banana".into()];
        assert!(validate_with_env(&o, false).is_err());
    }

    #[test]
    fn known_category_passes() {
        let mut o = opts();
        o.category = vec!["user-logs".into()];
        validate_with_env(&o, false).unwrap();
    }

    #[test]
    fn yes_without_category_fails() {
        let mut o = opts();
        o.yes = true;
        let err = validate_with_env(&o, false).unwrap_err().to_string();
        assert!(err.contains("--yes requires --category"));
    }

    #[test]
    fn yes_with_category_passes() {
        let mut o = opts();
        o.yes = true;
        o.category = vec!["user-logs".into()];
        validate_with_env(&o, false).unwrap();
    }

    #[test]
    fn hard_yes_without_env_fails() {
        let mut o = opts();
        o.yes = true;
        o.hard = true;
        o.category = vec!["user-logs".into()];
        let err = validate_with_env(&o, false).unwrap_err().to_string();
        assert!(err.contains("TINY_CONFIRM_HARD"));
    }

    #[test]
    fn hard_yes_with_env_passes() {
        let mut o = opts();
        o.yes = true;
        o.hard = true;
        o.category = vec!["user-logs".into()];
        validate_with_env(&o, true).unwrap();
    }

    #[test]
    fn destructive_hard_yes_without_category_fails() {
        let mut o = opts();
        o.yes = true;
        o.hard = true;
        o.include_destructive = true;
        let err = validate_with_env(&o, true).unwrap_err().to_string();
        assert!(err.contains("--include-destructive"));
    }

    #[test]
    fn destructive_hard_yes_with_trash_category_passes() {
        let mut o = opts();
        o.yes = true;
        o.hard = true;
        o.include_destructive = true;
        o.category = vec!["trash".into()];
        validate_with_env(&o, true).unwrap();
    }

    #[test]
    fn idle_days_zero_fails() {
        let mut o = opts();
        o.idle_days = 0;
        let err = validate_with_env(&o, false).unwrap_err().to_string();
        assert!(err.contains("--idle-days"));
    }

    #[test]
    fn category_can_repeat() {
        let mut o = opts();
        o.category = vec!["user-logs".into(), "xcode-derived".into()];
        validate_with_env(&o, false).unwrap();
    }
}
