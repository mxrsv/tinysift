//! Glyphs. The Unicode symbols (`✓ ❯ ◉ ◯ ● ━ ⠹ –`) are drawn in both modes;
//! Nerd mode adds one Material Design (`nf-md-*`) glyph before each tab name
//! and each category name.

use super::pages::PageId;

/// Which glyph set the session draws; `--icons unicode|nerd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum IconMode {
    #[default]
    Unicode,
    Nerd,
}

impl IconMode {
    /// The value stored in the settings file and given to `--icons`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unicode => "unicode",
            Self::Nerd => "nerd",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "unicode" => Some(Self::Unicode),
            "nerd" => Some(Self::Nerd),
            _ => None,
        }
    }

    /// The glyph and a space before a tab name, or nothing in Unicode mode.
    pub fn tab_prefix(self, page: PageId) -> String {
        self.prefix(tab_glyph(page))
    }

    /// The glyph and a space before a category name, or nothing in Unicode mode.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by the Clean page rows, plan task 07")
    )]
    pub fn category_prefix(self, category_id: &str) -> String {
        self.prefix(category_glyph(category_id))
    }

    fn prefix(self, glyph: char) -> String {
        match self {
            Self::Unicode => String::new(),
            Self::Nerd => format!("{glyph} "),
        }
    }
}

/// `nf-md-folder_outline`: every category without its own glyph.
const FOLDER: char = '\u{F0256}';

fn tab_glyph(page: PageId) -> char {
    match page {
        PageId::Clean => '\u{F00E2}',     // nf-md-broom
        PageId::Uninstall => '\u{F0A7A}', // nf-md-trash_can_outline
        PageId::Scan => '\u{F0349}',      // nf-md-magnify
        PageId::Processes => '\u{F061A}', // nf-md-chip
        PageId::System => '\u{F0322}',    // nf-md-laptop
    }
}

/// The glyph for an engine category ID (`known_category_ids` in core).
fn category_glyph(id: &str) -> char {
    match id {
        "xcode-derived" | "xcode-archives" | "xcode-devicesupport" | "xcode-previews" => {
            '\u{F0035}' // nf-md-apple
        }
        "ios-simulators"
        | "simulator-devices"
        | "system-simulator-caches"
        | "simulator-runtimes"
        | "simulator-runtime-images" => '\u{F011C}', // nf-md-cellphone
        "node-modules" => '\u{F0399}',                // nf-md-nodejs
        "npm" => '\u{F06F7}',                         // nf-md-npm
        "pnpm" | "yarn" | "bun-cache" => '\u{F03D7}', // nf-md-package_variant_closed
        "homebrew" => '\u{F1116}',                    // nf-md-glass_mug_variant
        "crash-reports" => '\u{F0A30}',               // nf-md-bug_outline
        "user-logs" => '\u{F09ED}',                   // nf-md-text_box_outline
        "docker" | "docker-volumes" => '\u{F0868}',   // nf-md-docker
        _ => FOLDER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_mode_adds_no_glyph() {
        assert_eq!(IconMode::Unicode.tab_prefix(PageId::Clean), "");
        assert_eq!(IconMode::Unicode.category_prefix("npm"), "");
    }

    #[test]
    fn nerd_mode_uses_the_material_design_set() {
        assert_eq!(IconMode::Nerd.tab_prefix(PageId::Clean), "\u{F00E2} ");
        assert_eq!(IconMode::Nerd.tab_prefix(PageId::System), "\u{F0322} ");
        assert_eq!(
            IconMode::Nerd.category_prefix("xcode-derived"),
            "\u{F0035} "
        );
        assert_eq!(
            IconMode::Nerd.category_prefix("ios-simulators"),
            "\u{F011C} "
        );
        assert_eq!(IconMode::Nerd.category_prefix("yarn"), "\u{F03D7} ");
        assert_eq!(
            IconMode::Nerd.category_prefix("crash-reports"),
            "\u{F0A30} "
        );
    }

    #[test]
    fn every_engine_category_has_a_glyph_and_unknown_ones_get_the_folder() {
        for id in tiny_core::clean::providers::known_category_ids() {
            let glyph = category_glyph(id);
            assert!(('\u{F0000}'..='\u{F1FFF}').contains(&glyph), "{id}");
        }
        assert_eq!(category_glyph("no-such-category"), FOLDER);
    }

    #[test]
    fn names_round_trip() {
        for mode in [IconMode::Unicode, IconMode::Nerd] {
            assert_eq!(IconMode::from_name(mode.name()), Some(mode));
        }
        assert_eq!(IconMode::from_name("emoji"), None);
    }
}
