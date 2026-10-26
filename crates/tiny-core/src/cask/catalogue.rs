//! The hand-picked list "Get apps" offers, and which of it the Mac already has.
//!
//! The list ships with the app: a curated list stays honest, and install
//! counts never decide what is shown. A token that is not here cannot be
//! installed ([`catalogue_entry`]).

use crate::uninstall::AppEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Recommended,
    More,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogueEntry {
    /// The Homebrew cask token, the only thing `brew` is ever given.
    pub token: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub group: Group,
    /// `CFBundleIdentifier` of the app the cask installs. An installed app
    /// is matched by this, not by its name, so a renamed copy still counts.
    pub bundle_id: &'static str,
}

/// Shown in this order within each group. Every cask installs an app with no
/// administrator password: `brew info --json=v2` shows no `pkg`, `installer`
/// or `sudo` step and no `uninstall` directive that always needs root
/// (the rules in `elevation.rs`).
///
/// Every `bundle_id` is the `CFBundleIdentifier` of the app the cask installs.
/// The first seven were verified two ways on 2026-10-25: the app installed on a
/// Mac (`defaults read <app>/Contents/Info CFBundleIdentifier`), and the
/// identifier in the cask's own `uninstall quit` stanza (Brave has none, so
/// its zap stanza's `com.brave.Browser` preference and cache paths stand in).
/// The thirteen after them were read on 2026-10-26 from the Info.plist inside
/// the archive `brew fetch --cask` downloads (its checksum checked by `brew`),
/// and for Cursor and Postman also from the installed app; the `uninstall quit`
/// stanza was only a hint. ChatGPT's identifier is `com.openai.codex`, not
/// `com.openai.chat`: the current cask installs that app. Cursor's is
/// ToDesktop's generated identifier, not a name.
pub const CATALOGUE: &[CatalogueEntry] = &[
    entry(
        "visual-studio-code",
        "Visual Studio Code",
        "Code editor",
        Group::Recommended,
        "com.microsoft.VSCode",
    ),
    entry(
        "cursor",
        "Cursor",
        "AI code editor",
        Group::Recommended,
        "com.todesktop.230313mzl4w4u92",
    ),
    entry(
        "zed",
        "Zed",
        "Editor with built-in collaboration",
        Group::Recommended,
        "dev.zed.Zed",
    ),
    entry(
        "ghostty",
        "Ghostty",
        "Terminal emulator",
        Group::Recommended,
        "com.mitchellh.ghostty",
    ),
    entry(
        "iterm2",
        "iTerm2",
        "Terminal with split panes",
        Group::Recommended,
        "com.googlecode.iterm2",
    ),
    entry(
        "rectangle",
        "Rectangle",
        "Snap windows with shortcuts",
        Group::Recommended,
        "com.knollsoft.Rectangle",
    ),
    entry(
        "postman",
        "Postman",
        "Build and test APIs",
        Group::Recommended,
        "com.postmanlabs.mac",
    ),
    entry(
        "jetbrains-toolbox",
        "JetBrains Toolbox",
        "Install and update JetBrains IDEs",
        Group::Recommended,
        "com.jetbrains.toolbox",
    ),
    entry(
        "sublime-text",
        "Sublime Text",
        "Text editor for code and prose",
        Group::More,
        "com.sublimetext.4",
    ),
    entry(
        "warp",
        "Warp",
        "Terminal with command blocks and AI",
        Group::More,
        "dev.warp.Warp-Stable",
    ),
    entry(
        "fork",
        "Fork",
        "Git client",
        Group::More,
        "com.DanPristupov.Fork",
    ),
    entry(
        "bruno",
        "Bruno",
        "Open-source API client",
        Group::More,
        "com.usebruno.app",
    ),
    entry(
        "dbeaver-community",
        "DBeaver",
        "SQL client for many databases",
        Group::More,
        "org.jkiss.dbeaver.core.product",
    ),
    entry(
        "podman-desktop",
        "Podman Desktop",
        "Manage containers and images",
        Group::More,
        "io.podmandesktop.PodmanDesktop",
    ),
    entry(
        "utm",
        "UTM",
        "Virtual machines and emulation",
        Group::More,
        "com.utmapp.UTM",
    ),
    entry(
        "drawio",
        "draw.io",
        "Diagram editor",
        Group::More,
        "com.jgraph.drawio.desktop",
    ),
    entry(
        "chatgpt",
        "ChatGPT",
        "Chat app from OpenAI",
        Group::More,
        "com.openai.codex",
    ),
    entry(
        "brave-browser",
        "Brave",
        "Browser",
        Group::More,
        "com.brave.Browser",
    ),
    entry(
        "lm-studio",
        "LM Studio",
        "Run language models locally",
        Group::More,
        "ai.elementlabs.lmstudio",
    ),
    entry(
        "ollama-app",
        "Ollama",
        "Local language models with a CLI",
        Group::More,
        "com.electron.ollama",
    ),
];

const fn entry(
    token: &'static str,
    name: &'static str,
    blurb: &'static str,
    group: Group,
    bundle_id: &'static str,
) -> CatalogueEntry {
    CatalogueEntry {
        token,
        name,
        blurb,
        group,
        bundle_id,
    }
}

/// The entry for `token`, matched exactly.
pub fn catalogue_entry(token: &str) -> Option<&'static CatalogueEntry> {
    CATALOGUE.iter().find(|entry| entry.token == token)
}

/// A catalogue entry and whether the Mac has its app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogueApp {
    pub entry: &'static CatalogueEntry,
    pub on_this_mac: bool,
}

/// The whole catalogue, in its own order, each entry marked by whether an app
/// with its bundle identifier is among `installed` (the Uninstall listing).
/// Names play no part. An installed app whose identifier could not be read
/// matches nothing, so an install of it is stopped later by `brew` itself or
/// by the check on where the cask would put its app.
pub fn catalogue_with_presence(installed: &[AppEntry]) -> Vec<CatalogueApp> {
    CATALOGUE
        .iter()
        .map(|entry| CatalogueApp {
            entry,
            on_this_mac: installed_copy(entry, installed).is_some(),
        })
        .collect()
}

/// The app among `installed` that has `entry`'s bundle identifier, whatever
/// it is called. Where it sits is up to the caller's list: the Uninstall
/// listing is the top level of `/Applications` only, and the FFI adds the top
/// level of `~/Applications` ([`list_apps_in`](super::user_apps::list_apps_in)).
pub fn installed_copy<'a>(
    entry: &CatalogueEntry,
    installed: &'a [AppEntry],
) -> Option<&'a AppEntry> {
    installed.iter().find(|app| {
        app.bundle_id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(entry.bundle_id))
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cask::valid_token;
    use crate::uninstall::valid_bundle_id;

    fn app(name: &str, bundle_id: Option<&str>) -> AppEntry {
        AppEntry {
            name: name.into(),
            path: PathBuf::from(format!("/Applications/{name}.app")),
            bundle_id: bundle_id.map(String::from),
            size: 0,
            last_used_days: None,
        }
    }

    fn on_this_mac(installed: &[AppEntry]) -> Vec<&'static str> {
        catalogue_with_presence(installed)
            .into_iter()
            .filter(|app| app.on_this_mac)
            .map(|app| app.entry.token)
            .collect()
    }

    #[test]
    fn the_catalogue_is_the_agreed_twenty_in_order() {
        let listed: Vec<_> = CATALOGUE
            .iter()
            .map(|e| (e.token, e.name, e.blurb, e.group))
            .collect();
        assert_eq!(
            listed,
            [
                (
                    "visual-studio-code",
                    "Visual Studio Code",
                    "Code editor",
                    Group::Recommended
                ),
                ("cursor", "Cursor", "AI code editor", Group::Recommended),
                (
                    "zed",
                    "Zed",
                    "Editor with built-in collaboration",
                    Group::Recommended
                ),
                (
                    "ghostty",
                    "Ghostty",
                    "Terminal emulator",
                    Group::Recommended
                ),
                (
                    "iterm2",
                    "iTerm2",
                    "Terminal with split panes",
                    Group::Recommended
                ),
                (
                    "rectangle",
                    "Rectangle",
                    "Snap windows with shortcuts",
                    Group::Recommended
                ),
                (
                    "postman",
                    "Postman",
                    "Build and test APIs",
                    Group::Recommended
                ),
                (
                    "jetbrains-toolbox",
                    "JetBrains Toolbox",
                    "Install and update JetBrains IDEs",
                    Group::Recommended
                ),
                (
                    "sublime-text",
                    "Sublime Text",
                    "Text editor for code and prose",
                    Group::More
                ),
                (
                    "warp",
                    "Warp",
                    "Terminal with command blocks and AI",
                    Group::More
                ),
                ("fork", "Fork", "Git client", Group::More),
                ("bruno", "Bruno", "Open-source API client", Group::More),
                (
                    "dbeaver-community",
                    "DBeaver",
                    "SQL client for many databases",
                    Group::More
                ),
                (
                    "podman-desktop",
                    "Podman Desktop",
                    "Manage containers and images",
                    Group::More
                ),
                ("utm", "UTM", "Virtual machines and emulation", Group::More),
                ("drawio", "draw.io", "Diagram editor", Group::More),
                ("chatgpt", "ChatGPT", "Chat app from OpenAI", Group::More),
                ("brave-browser", "Brave", "Browser", Group::More),
                (
                    "lm-studio",
                    "LM Studio",
                    "Run language models locally",
                    Group::More
                ),
                (
                    "ollama-app",
                    "Ollama",
                    "Local language models with a CLI",
                    Group::More
                ),
            ]
        );
    }

    #[test]
    fn every_entry_is_usable_as_a_token_and_an_identity() {
        for entry in CATALOGUE {
            assert!(valid_token(entry.token), "{}", entry.token);
            assert!(valid_bundle_id(entry.bundle_id), "{}", entry.bundle_id);
            assert!(!entry.blurb.is_empty() && !entry.name.is_empty());
            assert_eq!(catalogue_entry(entry.token), Some(entry));
        }
        let mut tokens: Vec<_> = CATALOGUE.iter().map(|e| e.token).collect();
        let mut ids: Vec<_> = CATALOGUE.iter().map(|e| e.bundle_id).collect();
        tokens.sort();
        tokens.dedup();
        ids.sort();
        ids.dedup();
        assert_eq!(
            (tokens.len(), ids.len()),
            (CATALOGUE.len(), CATALOGUE.len())
        );
        assert_eq!(catalogue_entry("Rectangle"), None, "exact, not case-folded");
        assert_eq!(catalogue_entry("brew"), None);
    }

    #[test]
    fn an_app_counts_by_bundle_identity_not_by_name() {
        // A renamed copy still counts; an app that only shares the name does not.
        let installed = [
            app("My Window Snapper", Some("com.knollsoft.Rectangle")),
            app("Ghostty", Some("com.example.NotGhostty")),
            app("iTerm", None),
        ];
        assert_eq!(on_this_mac(&installed), ["rectangle"]);
    }

    #[test]
    fn identities_compare_without_regard_to_case() {
        let installed = [app("Brave Browser", Some("com.brave.browser"))];
        assert_eq!(on_this_mac(&installed), ["brave-browser"]);
    }

    #[test]
    fn nothing_installed_leaves_everything_to_get() {
        let all = catalogue_with_presence(&[]);
        assert_eq!(all.len(), CATALOGUE.len());
        assert!(all.iter().all(|app| !app.on_this_mac));
        let tokens: Vec<_> = all.iter().map(|app| app.entry.token).collect();
        assert_eq!(
            tokens,
            CATALOGUE.iter().map(|e| e.token).collect::<Vec<_>>()
        );
    }
}
