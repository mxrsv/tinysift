//! Installed web browsers and their profiles, read-only.
//!
//! [`discover`] looks under a home folder for the profile data of Safari,
//! the Chromium browsers (Chrome, Arc, Brave, Edge, Vivaldi, Opera) and
//! Firefox. A browser counts only when its profile data exists: a vendor
//! folder holding nothing but `NativeMessagingHosts` (left by an extension's
//! helper) is not an installed browser. Nothing here writes, quits or
//! signals anything; each [`Browser`] carries the bundle ids and process
//! names a caller needs to tell whether it runs.

mod discovery;
pub mod extensions;
pub mod privacy;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use discovery::discover;

/// One supported browser. The order of [`BrowserKind::ALL`] is the order
/// the Browsers page lists them: Chromium browsers first, then Safari and
/// Firefox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserKind {
    Chrome,
    Arc,
    Brave,
    Edge,
    Vivaldi,
    Opera,
    Safari,
    Firefox,
}

/// The engine family, which decides where and how profiles are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserFamily {
    /// Profiles listed in `Local State` `profile.info_cache`.
    Chromium,
    /// One profile in `~/Library/Safari` and Safari's container.
    WebKit,
    /// Profiles listed in `profiles.ini`.
    Gecko,
}

impl BrowserKind {
    pub const ALL: [BrowserKind; 8] = [
        BrowserKind::Chrome,
        BrowserKind::Arc,
        BrowserKind::Brave,
        BrowserKind::Edge,
        BrowserKind::Vivaldi,
        BrowserKind::Opera,
        BrowserKind::Safari,
        BrowserKind::Firefox,
    ];

    /// The name the browser calls itself.
    pub fn display_name(self) -> &'static str {
        match self {
            BrowserKind::Chrome => "Google Chrome",
            BrowserKind::Arc => "Arc",
            BrowserKind::Brave => "Brave",
            BrowserKind::Edge => "Microsoft Edge",
            BrowserKind::Vivaldi => "Vivaldi",
            BrowserKind::Opera => "Opera",
            BrowserKind::Safari => "Safari",
            BrowserKind::Firefox => "Firefox",
        }
    }

    pub fn family(self) -> BrowserFamily {
        match self {
            BrowserKind::Safari => BrowserFamily::WebKit,
            BrowserKind::Firefox => BrowserFamily::Gecko,
            _ => BrowserFamily::Chromium,
        }
    }

    /// Profile data root, relative to the home folder.
    pub fn data_root(self) -> &'static str {
        match self {
            BrowserKind::Chrome => "Library/Application Support/Google/Chrome",
            BrowserKind::Arc => "Library/Application Support/Arc/User Data",
            BrowserKind::Brave => "Library/Application Support/BraveSoftware/Brave-Browser",
            BrowserKind::Edge => "Library/Application Support/Microsoft Edge",
            BrowserKind::Vivaldi => "Library/Application Support/Vivaldi",
            BrowserKind::Opera => "Library/Application Support/com.operasoftware.Opera",
            BrowserKind::Safari => "Library/Safari",
            BrowserKind::Firefox => "Library/Application Support/Firefox",
        }
    }

    /// The main app's bundle id.
    pub fn bundle_id(self) -> &'static str {
        match self {
            BrowserKind::Chrome => "com.google.Chrome",
            BrowserKind::Arc => "company.thebrowser.Browser",
            BrowserKind::Brave => "com.brave.Browser",
            BrowserKind::Edge => "com.microsoft.edgemac",
            BrowserKind::Vivaldi => "com.vivaldi.Vivaldi",
            BrowserKind::Opera => "com.operasoftware.Opera",
            BrowserKind::Safari => "com.apple.Safari",
            BrowserKind::Firefox => "org.mozilla.firefox",
        }
    }

    /// Bundle ids of the browser's other editions, which share its profile
    /// folders: Firefox Developer Edition and Nightly use Firefox's
    /// `profiles.ini` and `Profiles`.
    pub fn edition_bundle_ids(self) -> &'static [&'static str] {
        match self {
            BrowserKind::Firefox => &["org.mozilla.firefoxdeveloperedition", "org.mozilla.nightly"],
            _ => &[],
        }
    }

    /// Bundle id prefixes of the browser's helper processes. A Chromium
    /// browser's helpers are `<id>.helper`, `<id>.helper.renderer` and
    /// `<id>.framework.*` (seen in Chrome and Brave), so the prefix is the
    /// main id and a dot. Firefox's content processes run from
    /// `plugin-container.app` inside `Firefox.app`, so the running check,
    /// which reads the outermost `.app`, sees them as Firefox itself and
    /// none is listed. Safari's WebKit helpers are shared with every WebKit
    /// app and some `com.apple.Safari.*` services outlive Safari, so none
    /// is listed for it either.
    pub fn helper_bundle_prefixes(self) -> &'static [&'static str] {
        match self {
            BrowserKind::Chrome => &["com.google.Chrome."],
            BrowserKind::Arc => &["company.thebrowser.Browser."],
            BrowserKind::Brave => &["com.brave.Browser."],
            BrowserKind::Edge => &["com.microsoft.edgemac."],
            BrowserKind::Vivaldi => &["com.vivaldi.Vivaldi."],
            BrowserKind::Opera => &["com.operasoftware.Opera."],
            BrowserKind::Safari | BrowserKind::Firefox => &[],
        }
    }

    /// Whether a process with this bundle id belongs to the browser: the
    /// main app, another edition of it or one of its helpers. Bundle ids
    /// compare without case, as Launch Services does.
    pub fn owns_bundle_id(self, id: &str) -> bool {
        let id = id.to_ascii_lowercase();
        std::iter::once(self.bundle_id())
            .chain(self.edition_bundle_ids().iter().copied())
            .any(|own| id == own.to_ascii_lowercase())
            || self
                .helper_bundle_prefixes()
                .iter()
                .any(|prefix| id.starts_with(&prefix.to_ascii_lowercase()))
    }

    /// Process names Clean's running check (`pgrep -x`, which tells case)
    /// gates on, the same names the `browser-caches` and
    /// `browser-offline-data` categories use. Every Firefox edition's
    /// process is `firefox`, its executable; `Firefox` and the editions'
    /// app names are kept as well.
    pub fn app_names(self) -> &'static [&'static str] {
        match self {
            BrowserKind::Chrome => &["Google Chrome"],
            BrowserKind::Arc => &["Arc"],
            BrowserKind::Brave => &["Brave Browser"],
            BrowserKind::Edge => &["Microsoft Edge"],
            BrowserKind::Vivaldi => &["Vivaldi"],
            BrowserKind::Opera => &["Opera"],
            BrowserKind::Safari => &["Safari"],
            BrowserKind::Firefox => &[
                "firefox",
                "Firefox",
                "Firefox Developer Edition",
                "Firefox Nightly",
            ],
        }
    }

    /// Opera keeps its only profile in the data root itself, with no
    /// `Default` folder.
    fn allows_root_profile(self) -> bool {
        self == BrowserKind::Opera
    }
}

/// An installed browser and the profiles found for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Browser {
    pub kind: BrowserKind,
    pub family: BrowserFamily,
    pub name: String,
    /// Where its profiles live: `Local State`'s folder, Firefox's folder
    /// with `profiles.ini`, or `~/Library/Safari`.
    pub data_root: PathBuf,
    /// Safari's sandbox container (`~/Library/Containers/com.apple.Safari`),
    /// which holds its cookies; `None` for other browsers or when absent.
    pub container_dir: Option<PathBuf>,
    pub bundle_id: String,
    pub helper_bundle_prefixes: Vec<String>,
    pub app_names: Vec<String>,
    pub profiles: Vec<BrowserProfile>,
    /// Why the profile list could not be read; `profiles` is then empty.
    pub issue: Option<DiscoveryIssue>,
}

/// One profile, named as the browser names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProfile {
    /// The browser's key for the profile: the `info_cache` folder name
    /// (`Default`, `Profile 1`), Firefox's `Path`, or empty for a profile
    /// kept in the data root (Safari, Opera).
    pub id: String,
    pub name: String,
    /// The signed-in account's email; `None` when not signed in.
    pub user_name: Option<String>,
    pub avatar: ProfileAvatar,
    /// Chromium's `profile_highlight_color`, as ARGB.
    pub highlight_color: Option<u32>,
    /// The profile's own data folder (History, Cookies, Preferences).
    pub data_dir: PathBuf,
    /// Its disk cache folder under `~/Library/Caches`, when it exists:
    /// the data folder's path below `Application Support` moved to
    /// `Caches` (Chromium's rule, seen for Chrome), or Safari's
    /// `com.apple.Safari`.
    pub cache_dir: Option<PathBuf>,
}

/// What a profile row draws as its picture. Chromium only; empty elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileAvatar {
    /// `avatar_icon`, such as `chrome://theme/IDR_PROFILE_AVATAR_26`.
    pub icon: Option<String>,
    /// The number at the end of a built-in avatar's `avatar_icon`.
    pub icon_index: Option<u32>,
    /// `use_gaia_picture`: the user picked the account photo.
    pub uses_account_picture: bool,
    /// The account photo (`gaia_picture_file_name` in the profile folder),
    /// only when the file exists.
    pub account_picture: Option<PathBuf>,
}

/// Why a browser's profile list could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryIssue {
    pub kind: DiscoveryIssueKind,
    /// The file that failed: `Local State` or `profiles.ini`.
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscoveryIssueKind {
    /// macOS refused the read: the app lacks Full Disk Access or the
    /// permission to read other apps' data.
    PermissionDenied,
    /// Missing or failed to read for another reason.
    Unreadable,
    /// Read, but not the shape expected.
    Malformed,
}
