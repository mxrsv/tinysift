//! Records the Browsers page reads, mirroring core's browser data. Paths a
//! write would need (databases, profile folders) and the privacy removal
//! lists stay in Rust; only what the page draws crosses.

use tiny_core::browsers::extensions::{Extension, ExtensionSource};
use tiny_core::browsers::privacy::{PrivacyIssue, PrivacyIssueKind, ProfilePrivacy};
use tiny_core::browsers::{BrowserKind, DiscoveryIssue, DiscoveryIssueKind, ProfileAvatar};

/// One supported browser, in the order the page lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum FfiBrowserKind {
    Chrome,
    Arc,
    Brave,
    Edge,
    Vivaldi,
    Opera,
    Safari,
    Firefox,
}

/// What a Browsers scan found under the user's home.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiBrowsersReport {
    /// Names this scan. A later privacy write passes it back with a profile
    /// and its parts; a newer scan makes it stale.
    pub generation: u64,
    /// Installed browsers, Chromium ones first, then Safari and Firefox.
    pub browsers: Vec<FfiBrowser>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiBrowser {
    pub kind: FfiBrowserKind,
    /// The name the browser calls itself ("Google Chrome").
    pub name: String,
    pub bundle_id: String,
    /// Whether the browser's app runs now, by Clean's `pgrep` check;
    /// `None` when the check could not answer.
    pub running: Option<bool>,
    pub profiles: Vec<FfiBrowserProfile>,
    /// Why the profile list could not be read; `profiles` is then empty.
    pub issue: Option<FfiBrowserIssue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiBrowserProfile {
    /// The browser's key for the profile (`Default`, `Profile 1`, Firefox's
    /// `Path`); empty for Safari and Opera's single profile. Unique within
    /// its browser only.
    pub id: String,
    pub name: String,
    /// The signed-in account's email; `None` when not signed in.
    pub user_name: Option<String>,
    pub avatar: FfiProfileAvatar,
    /// Chromium's `profile_highlight_color`, as ARGB.
    pub highlight_color: Option<u32>,
    pub privacy: FfiProfilePrivacy,
    /// User-installed extensions, by name then id; empty outside Chromium.
    pub extensions: Vec<FfiBrowserExtension>,
    /// Why the extension list could not be read; `extensions` is then empty.
    pub extensions_issue: Option<FfiBrowserIssue>,
}

/// What a profile row draws as its picture: the account photo when the
/// user picked it and it exists, else Chromium's built-in avatar.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiProfileAvatar {
    /// The number of a built-in avatar (`IDR_PROFILE_AVATAR_26` -> 26).
    pub icon_index: Option<u32>,
    pub uses_account_picture: bool,
    /// The account photo's absolute path, only when the file exists.
    pub account_picture: Option<String>,
}

/// The counts of a profile's Privacy part. The cookies and history rows
/// behind them stay in the session, so a write takes exactly what was
/// counted here.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiProfilePrivacy {
    /// False when the profile was not scanned; `issue` says why and every
    /// count is zero.
    pub supported: bool,
    pub issue: Option<FfiBrowserIssue>,
    /// Cookies, counted by row.
    pub trackers: u64,
    pub old: u64,
    pub kept: u64,
    /// Distinct sites among the tracker and the old cookies.
    pub tracker_sites: u64,
    pub old_sites: u64,
    pub old_history_entries: u64,
    /// The profile has no history visit, so no cookie was classified by
    /// history.
    pub history_is_empty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiExtensionSource {
    /// Installed by the user, normally from the web store.
    Installed,
    /// Loaded unpacked in developer mode.
    Unpacked,
}

/// One user-installed extension. Removal is the user's click in the
/// browser's own extensions page; nothing here removes one.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiBrowserExtension {
    /// The 32-letter id, as in `chrome://extensions/?id=<id>`.
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub source: FfiExtensionSource,
    pub from_webstore: bool,
    pub disabled: bool,
    /// Not updated (or, never updated, installed) in a year. Never "unused".
    pub stale: bool,
    /// Unix seconds.
    pub installed_at: Option<i64>,
    /// Unix seconds.
    pub updated_at: Option<i64>,
    pub size_bytes: u64,
}

/// Why something could not be read. `PermissionDenied` is the case the
/// app answers with its Full Disk Access guide.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiBrowserIssue {
    pub kind: FfiBrowserIssueKind,
    /// The file that failed, when one did.
    pub path: Option<String>,
    /// Diagnostic text; user-facing copy lives in Swift.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiBrowserIssueKind {
    /// macOS refused the read: the app lacks Full Disk Access.
    PermissionDenied,
    /// Missing or failed to read for another reason.
    Unreadable,
    /// Read, but not the shape expected.
    Malformed,
    /// A database table or column the privacy scan needs is missing.
    UnknownSchema,
    /// This build does not scan that browser's privacy data (Safari).
    NotSupported,
}

impl From<BrowserKind> for FfiBrowserKind {
    fn from(kind: BrowserKind) -> Self {
        match kind {
            BrowserKind::Chrome => Self::Chrome,
            BrowserKind::Arc => Self::Arc,
            BrowserKind::Brave => Self::Brave,
            BrowserKind::Edge => Self::Edge,
            BrowserKind::Vivaldi => Self::Vivaldi,
            BrowserKind::Opera => Self::Opera,
            BrowserKind::Safari => Self::Safari,
            BrowserKind::Firefox => Self::Firefox,
        }
    }
}

impl From<FfiBrowserKind> for BrowserKind {
    fn from(kind: FfiBrowserKind) -> Self {
        match kind {
            FfiBrowserKind::Chrome => Self::Chrome,
            FfiBrowserKind::Arc => Self::Arc,
            FfiBrowserKind::Brave => Self::Brave,
            FfiBrowserKind::Edge => Self::Edge,
            FfiBrowserKind::Vivaldi => Self::Vivaldi,
            FfiBrowserKind::Opera => Self::Opera,
            FfiBrowserKind::Safari => Self::Safari,
            FfiBrowserKind::Firefox => Self::Firefox,
        }
    }
}

impl From<&ProfileAvatar> for FfiProfileAvatar {
    fn from(avatar: &ProfileAvatar) -> Self {
        Self {
            icon_index: avatar.icon_index,
            uses_account_picture: avatar.uses_account_picture,
            account_picture: avatar
                .account_picture
                .as_ref()
                .map(|p| p.display().to_string()),
        }
    }
}

/// The counts only: `cookies_to_remove`, `history_to_remove` and the store
/// paths are left behind on purpose.
impl From<&ProfilePrivacy> for FfiProfilePrivacy {
    fn from(privacy: &ProfilePrivacy) -> Self {
        Self {
            supported: privacy.supported,
            issue: privacy.reason.as_ref().map(Into::into),
            trackers: privacy.trackers,
            old: privacy.old,
            kept: privacy.kept,
            tracker_sites: privacy.tracker_sites,
            old_sites: privacy.old_sites,
            old_history_entries: privacy.old_history_entries,
            history_is_empty: privacy.history_is_empty,
        }
    }
}

impl From<&Extension> for FfiBrowserExtension {
    fn from(extension: &Extension) -> Self {
        Self {
            id: extension.id.clone(),
            name: extension.name.clone(),
            version: extension.version.clone(),
            source: match extension.source {
                ExtensionSource::Installed => FfiExtensionSource::Installed,
                ExtensionSource::Unpacked => FfiExtensionSource::Unpacked,
            },
            from_webstore: extension.from_webstore,
            disabled: extension.disabled,
            stale: extension.stale,
            installed_at: extension.installed_at,
            updated_at: extension.updated_at,
            size_bytes: extension.size_bytes,
        }
    }
}

impl From<&DiscoveryIssue> for FfiBrowserIssue {
    fn from(issue: &DiscoveryIssue) -> Self {
        Self {
            kind: match issue.kind {
                DiscoveryIssueKind::PermissionDenied => FfiBrowserIssueKind::PermissionDenied,
                DiscoveryIssueKind::Unreadable => FfiBrowserIssueKind::Unreadable,
                DiscoveryIssueKind::Malformed => FfiBrowserIssueKind::Malformed,
            },
            path: Some(issue.path.display().to_string()),
            detail: issue.detail.clone(),
        }
    }
}

impl From<&PrivacyIssue> for FfiBrowserIssue {
    fn from(issue: &PrivacyIssue) -> Self {
        Self {
            kind: match issue.kind {
                PrivacyIssueKind::PermissionDenied => FfiBrowserIssueKind::PermissionDenied,
                PrivacyIssueKind::Unreadable => FfiBrowserIssueKind::Unreadable,
                PrivacyIssueKind::UnknownSchema => FfiBrowserIssueKind::UnknownSchema,
                PrivacyIssueKind::NotSupported => FfiBrowserIssueKind::NotSupported,
            },
            path: issue.path.as_ref().map(|p| p.display().to_string()),
            detail: issue.detail.clone(),
        }
    }
}
