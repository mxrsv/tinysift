//! Which group of System Settings › Storage a path below the Data volume belongs to, and which item of that group.
//!
//! A path is named by its components below the Data volume's root (`Users/me/Library/Caches/…`). Classification stops at the
//! first component that settles both the group and the item; everything below that folder belongs to the same item.

use super::{DiskGroup, ItemKind};

/// One item of a group: what the breakdown lists under the group.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Place {
    pub group: DiskGroup,
    /// What the row shows: `~/Downloads`, `/Library`, `Cursor`, a container's folder name.
    pub name: String,
    /// Components below the Data volume's root.
    pub rel: Vec<String>,
    pub kind: ItemKind,
}

/// The current user's home as components below the Data volume's root (`["Users", "me"]`).
pub(crate) struct Layout {
    pub home: Vec<String>,
}

pub(crate) enum Class {
    Item(Place),
    /// A folder whose children belong to different items: walk into it.
    Descend,
}

const ICLOUD_DRIVE: &str = "com~apple~CloudDocs";

impl Layout {
    /// The group and item of `rel` (never empty). Only a folder can be `Descend`: a file always settles its own item.
    pub(crate) fn classify(&self, rel: &[String], is_dir: bool) -> Class {
        let class = self.place(rel);
        match class {
            Some(place) => Class::Item(place),
            None if is_dir => Class::Descend,
            None => Class::Item(self.loose_file(rel)),
        }
    }

    /// `~/...` for a path in the home, `/...` otherwise.
    pub(crate) fn display(&self, rel: &[String]) -> String {
        if rel.len() >= self.home.len() && rel[..self.home.len()] == self.home[..] {
            let rest = &rel[self.home.len()..];
            return if rest.is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.join("/"))
            };
        }
        format!("/{}", rel.join("/"))
    }

    fn place(&self, rel: &[String]) -> Option<Place> {
        let names: Vec<&str> = rel.iter().map(String::as_str).collect();
        let home = self.home.len();
        if rel.len() > home && rel[..home] == self.home[..] {
            return self.in_home(rel, &names[home..]);
        }
        match names.as_slice() {
            ["Applications"] | ["Library"] | ["private"] | ["Users"] => None,
            ["Applications", app, ..] => Some(self.app(rel, app, 2)),
            ["Library", "Developer", ..] => {
                Some(self.item_at(DiskGroup::Developer, rel, 2, ItemKind::Folder))
            }
            ["Library", ..] => Some(self.item_at(DiskGroup::SystemData, rel, 1, ItemKind::Folder)),
            ["private", ..] => Some(self.item_at(DiskGroup::SystemData, rel, 2, ItemKind::Folder)),
            ["Users", user, ..]
                if self.home.get(1).map(String::as_str) != Some(*user) || self.home.len() != 2 =>
            {
                Some(self.item_at(DiskGroup::OtherUsers, rel, 2, ItemKind::Folder))
            }
            ["Users", ..] => None,
            [_, ..] => Some(self.item_at(DiskGroup::SystemData, rel, 1, ItemKind::Folder)),
            [] => None,
        }
    }

    /// `rest` is the path below the home folder (never empty).
    fn in_home(&self, rel: &[String], rest: &[&str]) -> Option<Place> {
        let depth = self.home.len();
        let at =
            |group, extra: usize| Some(self.item_at(group, rel, depth + extra, ItemKind::Folder));
        match rest {
            ["Library"]
            | ["Applications"]
            | ["Library", "Containers"]
            | ["Library", "Group Containers"]
            | ["Library", "Mobile Documents"] => None,
            ["Applications", app, ..] => Some(self.app(rel, app, depth + 2)),
            ["Library", "Developer", ..] => at(DiskGroup::Developer, 2),
            ["Library", "Mail" | "Messages", ..] => at(DiskGroup::Documents, 2),
            ["Library", "Mobile Documents", folder, ..] => {
                let mut place =
                    self.item_at(DiskGroup::ICloudDrive, rel, depth + 3, ItemKind::Folder);
                place.name = icloud_name(folder);
                Some(place)
            }
            ["Library", "Containers" | "Group Containers", id, ..] => {
                let mut place =
                    self.item_at(DiskGroup::SystemData, rel, depth + 3, ItemKind::Container);
                place.name = (*id).to_string();
                Some(place)
            }
            ["Library", ..] => at(DiskGroup::SystemData, 2),
            [".Trash", ..] => at(DiskGroup::Documents, 1),
            [name, ..] if name.starts_with('.') => at(DiskGroup::Developer, 1),
            [_, ..] => at(DiskGroup::Documents, 1),
            [] => None,
        }
    }

    /// An app bundle (`Cursor.app` shows as `Cursor`), or another folder or file of an Applications folder.
    fn app(&self, rel: &[String], name: &str, depth: usize) -> Place {
        match name.strip_suffix(".app") {
            Some(stem) if !stem.is_empty() => {
                let mut place = self.item_at(DiskGroup::Applications, rel, depth, ItemKind::App);
                place.name = stem.to_string();
                place
            }
            _ => self.item_at(DiskGroup::Applications, rel, depth, ItemKind::Folder),
        }
    }

    /// The item made of `rel`'s first `depth` components.
    fn item_at(&self, group: DiskGroup, rel: &[String], depth: usize, kind: ItemKind) -> Place {
        let rel = rel[..depth.min(rel.len())].to_vec();
        Place {
            group,
            name: self.display(&rel),
            rel,
            kind,
        }
    }

    /// A file directly in a folder that `Descend`s (`~/Library/x.plist`, `/Users/x`): System Data, named by its path.
    fn loose_file(&self, rel: &[String]) -> Place {
        let group =
            if rel.first().map(String::as_str) == Some("Users") && !rel.starts_with(&self.home) {
                DiskGroup::OtherUsers
            } else {
                DiskGroup::SystemData
            };
        Place {
            group,
            name: self.display(rel),
            rel: rel.to_vec(),
            kind: ItemKind::File,
        }
    }
}

/// `com~apple~CloudDocs` is iCloud Drive itself; an app's folder (`com~apple~Pages`, `iCloud~md~obsidian`) shows its last part.
fn icloud_name(folder: &str) -> String {
    if folder == ICLOUD_DRIVE {
        return "iCloud Drive".into();
    }
    folder
        .rsplit('~')
        .next()
        .filter(|last| !last.is_empty())
        .unwrap_or(folder)
        .to_string()
}
