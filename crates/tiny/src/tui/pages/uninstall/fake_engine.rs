//! A fake engine fed the mock's data (`APPS` and `DATA.worst.apps` in
//! `tools/mock.js`). It records every call, removes nothing, and can hold a
//! plan read or a removal until the test lets it go.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tiny_core::uninstall::{
    AppEntry, Consent, ItemKind, LeftoverClass, Ownership, Plan, RemovalItem,
};

use super::engine::UninstallEngine;
use super::fake_gate::{Gate, GateHandle};

pub const LIBRARY: &str = "/Users/someone/Library";
const MB: u64 = 1_000_000;
pub const APPLE_REFUSAL: &str = "system app (com.apple.Keynote) — refuse";
pub const CASK_REFUSAL: &str =
    "looks like a Homebrew cask — use `brew uninstall --cask docker` (or pass --force)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    List,
    Plan(String),
    Freeze {
        app: String,
        serial: u64,
    },
    Remove {
        app: String,
        hard: bool,
        consent: Consent,
        serial: u64,
    },
}

/// What the fake hands back from `freeze`: the app and a serial number, so a
/// test can tell which frozen plan a removal ran.
pub struct FakeFrozen {
    app: String,
    serial: u64,
}

#[derive(Default)]
pub struct FakeEngine {
    apps: Vec<AppEntry>,
    items: HashMap<String, Vec<RemovalItem>>,
    refusals: HashMap<String, String>,
    /// Refusals that appear only when the plan is built again at enter.
    late_refusals: HashMap<String, String>,
    list_error: Option<String>,
    remove_errors: HashMap<String, String>,
    plan_gate: Option<Arc<Gate>>,
    remove_gate: Option<Arc<Gate>>,
    calls: Mutex<Vec<Call>>,
    serial: AtomicU64,
}

fn app(name: &str, id: &str, bytes: u64, days: Option<u64>) -> AppEntry {
    AppEntry {
        name: name.into(),
        path: PathBuf::from(format!("/Applications/{name}.app")),
        bundle_id: Some(id.into()),
        size: bytes,
        last_used_days: days,
    }
}

fn bundle(entry: &AppEntry) -> RemovalItem {
    RemovalItem {
        path: entry.path.clone(),
        size: entry.size,
        kind: ItemKind::AppBundle,
        ownership: Ownership::AppBundle,
    }
}

/// `RemovalItem` is not `Clone`; the fake hands out a fresh copy per plan.
fn copy(item: &RemovalItem) -> RemovalItem {
    RemovalItem {
        path: item.path.clone(),
        size: item.size,
        kind: item.kind,
        ownership: item.ownership.clone(),
    }
}

fn leftover(path: &str, bytes: u64, ownership: Ownership) -> RemovalItem {
    RemovalItem {
        path: PathBuf::from(format!("{LIBRARY}/{path}")),
        size: bytes,
        kind: ItemKind::Leftover,
        ownership,
    }
}

fn cache(path: &str, bytes: u64) -> RemovalItem {
    leftover(
        path,
        bytes,
        Ownership::Owned {
            class: LeftoverClass::Regenerable,
        },
    )
}

fn data(path: &str, bytes: u64) -> RemovalItem {
    leftover(
        path,
        bytes,
        Ownership::Owned {
            class: LeftoverClass::DataRisk,
        },
    )
}

fn kept(path: &str, reason: &str) -> RemovalItem {
    leftover(
        path,
        0,
        Ownership::ReportOnly {
            reason: reason.into(),
        },
    )
}

impl FakeEngine {
    /// Only `apps`, each `(name, bundle bytes, days since last use)`, each
    /// planned as its bundle, a cache of `cache` bytes and data of `data` bytes.
    pub fn listing(apps: &[(&str, u64, Option<u64>)], cache: u64, data: u64) -> Self {
        let apps: Vec<AppEntry> = apps
            .iter()
            .map(|&(name, bytes, days)| {
                app(name, &format!("com.example.{}", name.len()), bytes, days)
            })
            .collect();
        let items = apps
            .iter()
            .map(|entry| {
                let id = entry.bundle_id.clone().unwrap_or_default();
                (
                    entry.name.clone(),
                    vec![
                        bundle(entry),
                        self::cache(&format!("Caches/{id}"), cache),
                        self::data(&format!("Application Support/{id}"), data),
                    ],
                )
            })
            .collect();
        Self {
            apps,
            items,
            ..Self::default()
        }
    }

    /// The mock's normal list: six apps, Keynote and Docker refused.
    pub fn normal() -> Self {
        let alt = app("AltTab", "com.lwouis.alt-tab-macos", 11_310_000, Some(298));
        let pixelmator = app(
            "Pixelmator Pro",
            "com.pixelmatorteam.pixelmator.x",
            1_240 * MB,
            Some(214),
        );
        let id = "com.lwouis.alt-tab-macos";
        let items = HashMap::from([
            (
                "AltTab".to_string(),
                vec![
                    bundle(&alt),
                    cache(&format!("Caches/{id}"), 17_020_000),
                    data(&format!("HTTPStorages/{id}"), 2_040_000),
                    data(&format!("Application Support/{id}"), 384_000),
                    data(&format!("WebKit/{id}"), 292_000),
                    data(&format!("Preferences/{id}.plist"), 12_000),
                    kept(
                        &format!("Group Containers/group.{id}"),
                        "shared between apps",
                    ),
                ],
            ),
            (
                "Pixelmator Pro".to_string(),
                vec![
                    bundle(&pixelmator),
                    cache("Caches/com.pixelmatorteam.pixelmator.x", 60 * MB),
                    data("Containers/com.pixelmatorteam.pixelmator.x", 200 * MB),
                ],
            ),
        ]);
        Self {
            apps: vec![
                alt,
                pixelmator,
                app("Keynote", "com.apple.Keynote", 702 * MB, Some(120)),
                app("Windows App", "com.microsoft.rdc.macos", 412 * MB, Some(41)),
                app("Docker", "com.docker.docker", 2_100 * MB, Some(3)),
                app("Raycast", "com.raycast.macos", 180 * MB, Some(0)),
            ],
            items,
            refusals: HashMap::from([
                ("Keynote".to_string(), APPLE_REFUSAL.to_string()),
                ("Docker".to_string(), CASK_REFUSAL.to_string()),
            ]),
            ..Self::default()
        }
    }

    /// The mock's worst list: a very long name, a one-letter name, 240 apps.
    /// The long-named app was never opened in the mock ("never"), which the
    /// engine's last-used order puts last; here it was last used 4,000 days
    /// ago so it heads the list as the mock draws it.
    pub fn worst() -> Self {
        let name = "Adobe Photoshop 2027 (Beta) – Creative Cloud Edition for Apple silicon";
        let id = "com.adobe.Photoshop.beta";
        let adobe = AppEntry {
            path: PathBuf::from(
                "/Applications/Adobe Photoshop 2027 (Beta)/Adobe Photoshop 2027 (Beta).app",
            ),
            ..app(name, id, 48_212 * MB, Some(4_000))
        };
        let items = vec![
            bundle(&adobe),
            cache(&format!("Caches/{id}"), 9_120 * MB),
            data(&format!("Application Support/{id}"), 31_044 * MB),
            kept(
                &format!("LaunchAgents/{id}.helper.plist"),
                "a helper plist under another name",
            ),
            kept(
                &format!("Group Containers/group.{id}"),
                "shared between apps",
            ),
        ];
        let helpers = (0..237u64).map(|i| {
            app(
                &format!("Helper App {}", i + 1),
                &format!("com.example.helper{i}"),
                (40 + i) * MB,
                Some(600 - i),
            )
        });
        Self {
            apps: [
                adobe,
                app("x", "com.example.x", 200_000, Some(2_911)),
                app("Unsigned Tool", "com.example.unsigned", 88 * MB, Some(700)),
            ]
            .into_iter()
            .chain(helpers)
            .collect(),
            items: HashMap::from([(name.to_string(), items)]),
            ..Self::default()
        }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn unreadable(reason: &str) -> Self {
        Self {
            list_error: Some(reason.into()),
            ..Self::default()
        }
    }

    /// The engine refuses to remove `app` at run time with `reason`.
    pub fn failing(self, app: &str, reason: &str) -> Self {
        let mut remove_errors = self.remove_errors;
        remove_errors.insert(app.into(), reason.into());
        Self {
            remove_errors,
            ..self
        }
    }

    /// The engine refuses `app` once its plan is built again at enter.
    pub fn refusing_later(self, app: &str, reason: &str) -> Self {
        let mut late_refusals = self.late_refusals;
        late_refusals.insert(app.into(), reason.into());
        Self {
            late_refusals,
            ..self
        }
    }

    /// Plan reads wait at a gate.
    pub fn gated_plans(self) -> (Self, GateHandle) {
        let (gate, handle) = Gate::new();
        (
            Self {
                plan_gate: Some(gate),
                ..self
            },
            handle,
        )
    }

    /// Removals wait at a gate.
    pub fn gated_removals(self) -> (Self, GateHandle) {
        let (gate, handle) = Gate::new();
        (
            Self {
                remove_gate: Some(gate),
                ..self
            },
            handle,
        )
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn removals(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| matches!(call, Call::Remove { .. }))
            .collect()
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

impl UninstallEngine for FakeEngine {
    type Frozen = FakeFrozen;

    fn list_apps(&self) -> Result<Vec<AppEntry>, String> {
        self.record(Call::List);
        match &self.list_error {
            Some(reason) => Err(reason.clone()),
            None => Ok(self.apps.clone()),
        }
    }

    fn refusal(&self, app: &AppEntry) -> Option<String> {
        self.refusals.get(&app.name).cloned()
    }

    fn plan(&self, app: &AppEntry, _installed: &[AppEntry]) -> Result<Plan, String> {
        self.record(Call::Plan(app.name.clone()));
        if let Some(gate) = &self.plan_gate {
            gate.pass(&app.name);
        }
        let items = self.items.get(&app.name);
        Ok(Plan {
            app: app.clone(),
            items: items.map_or_else(
                || vec![bundle(app)],
                |items| items.iter().map(copy).collect(),
            ),
            blocked: self
                .refusal(app)
                .or_else(|| self.late_refusals.get(&app.name).cloned()),
        })
    }

    fn freeze(&self, plan: &Plan) -> Result<FakeFrozen, String> {
        // As `build_checked_plan` does: a blocked plan is refused.
        if let Some(reason) = &plan.blocked {
            return Err(reason.clone());
        }
        let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        let app = plan.app.name.clone();
        self.record(Call::Freeze {
            app: app.clone(),
            serial,
        });
        Ok(FakeFrozen { app, serial })
    }

    fn remove(&self, frozen: &FakeFrozen, hard: bool, consent: Consent) -> Result<(), String> {
        self.record(Call::Remove {
            app: frozen.app.clone(),
            hard,
            consent,
            serial: frozen.serial,
        });
        if let Some(gate) = &self.remove_gate {
            gate.pass(&frozen.app);
        }
        match self.remove_errors.get(&frozen.app) {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    fn library(&self) -> Option<PathBuf> {
        Some(PathBuf::from(LIBRARY))
    }
}
