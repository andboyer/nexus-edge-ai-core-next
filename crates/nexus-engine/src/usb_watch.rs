//! M2.2 Phase 3 — USB hot-plug watcher.
//!
//! Maintains a [`UsbRegistry`] of currently-attached `NEXUS_*`-labeled
//! volumes by polling the configured mount root every few seconds.
//! New / removed entries fire `STORAGE_USB_ATTACHED` /
//! `STORAGE_USB_DETACHED` on the bus and update the in-memory map
//! the recorder consults at clip-open time.
//!
//! ## Why polling instead of `notify`
//!
//! The plan originally suggested `notify-rs`, but for a use case
//! that is:
//!
//!   * driven by a human plugging a stick (latency tolerance is
//!     seconds, not milliseconds),
//!   * scoped to a single shallow directory (typically <10 entries),
//!   * cross-platform (Linux + macOS dev, with subtly different
//!     fsevent semantics),
//!
//! a 5-second `tokio::time::interval` poll is dramatically simpler:
//! no debounce logic, no platform-specific event filtering, no extra
//! crate dependency, and easy to test with a fake clock. The cost
//! is one `read_dir` call every 5s, which is well below the noise
//! floor on any production-class box.
//!
//! ## Mount conventions
//!
//! * **Linux (production):** the udev rule shipped under
//!   `deploy/udev/99-nexus-usb.rules` mounts every `NEXUS_*`-labeled
//!   block device under `<clips_dir>/usb/<label>/` via
//!   `systemd-mount`. The watcher scans `<clips_dir>/usb/`.
//! * **macOS (dev convenience):** macOS auto-mounts under
//!   `/Volumes/<label>`. The watcher will follow a symlink at
//!   `<clips_dir>/usb` pointing at `/Volumes` (`ln -s /Volumes
//!   <clips_dir>/usb`) — by symlinking we keep `motion_clips.hot_path`
//!   storage-relative-to-`clips_dir` exactly the same on both
//!   platforms, which means the API + cold replicator + cold-read
//!   cache work without any further changes.
//!
//! ## Why mount under `clips_dir`
//!
//! The api / replicator / cache all resolve clips via
//! `clips_dir.join(motion_clips.hot_path)`. By keeping USB mounts
//! as a subdirectory of `clips_dir`, USB-rooted clips look like a
//! plain `usb/<label>/<camera>/<date>/<file>.mp4` relative path —
//! one shape for both tiers, zero changes to the read side. The
//! security boundary stays at `clips_dir` (path-traversal guard
//! still works).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tracing::{debug, info, warn};

use nexus_bus::{topic, Bus, BusExt};
use nexus_store::Store;

/// Default scan interval. Picked to be unobtrusive (no measurable
/// load even on a busy box) while still feeling instant to a human
/// who just plugged a stick in. Operators rarely care about
/// sub-second hot-plug latency.
pub const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Label prefix the watcher accepts. Anything else under the mount
/// root is ignored — protects against stray mounts (Time Machine,
/// SD cards, etc.) accidentally being claimed as a hot-tier
/// target.
pub const USB_LABEL_PREFIX: &str = "NEXUS_";

/// One attached USB volume. The watcher hands these to the bus
/// payload + the API listing; the recorder only ever needs the
/// label (to compute `hot_handle`) and the relative mount path
/// (to compute `effective_clips_dir`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsbVolume {
    /// Filesystem label, with the `NEXUS_` prefix preserved (e.g.
    /// `"NEXUS_VAULT"`). Used both for `hot_handle` (`usb-<label>`)
    /// and as the registry key.
    pub label: String,
    /// Mount path **relative to the engine's `clips_dir`**, e.g.
    /// `usb/NEXUS_VAULT`. Joining with `clips_dir` gives the
    /// absolute mount root the recorder writes under.
    pub mount_relpath: PathBuf,
}

/// Shared, clone-able registry of attached USB volumes. Cheap to
/// clone (single `Arc`) — every recorder + the API handler holds
/// a clone.
///
/// The recorder calls [`UsbRegistry::lookup`] on every `open()`;
/// the watcher calls [`UsbRegistry::set_attached`] / [`UsbRegistry::detach`]
/// from its scan loop. All access is behind a `parking_lot::RwLock`
/// so the recorder hot path is wait-free in the common (no-mutation)
/// case.
#[derive(Debug, Clone, Default)]
pub struct UsbRegistry {
    inner: Arc<RwLock<HashMap<String, PathBuf>>>,
}

impl UsbRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the mount-relpath for a label. Returns `None` if the
    /// volume isn't attached — the recorder treats that as "fall
    /// back to local hot tier".
    pub fn lookup(&self, label: &str) -> Option<PathBuf> {
        self.inner.read().get(label).cloned()
    }
    /// Mark a volume as attached. Returns `true` if this is a new
    /// attach (the watcher uses this to decide whether to publish
    /// `STORAGE_USB_ATTACHED`); `false` for a no-op re-attach
    /// (path unchanged).
    pub fn set_attached(&self, label: &str, mount_relpath: PathBuf) -> bool {
        let mut g = self.inner.write();
        match g.get(label) {
            Some(existing) if existing == &mount_relpath => false,
            _ => {
                g.insert(label.to_string(), mount_relpath);
                true
            }
        }
    }

    /// Mark a volume as detached. Returns `true` if it was actually
    /// attached (the watcher uses this for the bus publish gating).
    pub fn detach(&self, label: &str) -> bool {
        self.inner.write().remove(label).is_some()
    }

    /// Snapshot of all currently-attached volumes. Sorted by label
    /// for stable UI / API output.
    pub fn list(&self) -> Vec<UsbVolume> {
        let mut v: Vec<UsbVolume> = self
            .inner
            .read()
            .iter()
            .map(|(label, mount_relpath)| UsbVolume {
                label: label.clone(),
                mount_relpath: mount_relpath.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.label.cmp(&b.label));
        v
    }
}

/// Bridge to the recorder's IoC trait. Lets the recorder consult
/// the live registry without depending on `nexus-engine`.
impl nexus_pipeline::recorder::UsbResolver for UsbRegistry {
    fn lookup(&self, label: &str) -> Option<PathBuf> {
        UsbRegistry::lookup(self, label)
    }
}

/// Configuration for [`run_usb_watch`]. Constructed once at boot
/// from `cfg.runtime.clips`.
#[derive(Debug, Clone)]
pub struct UsbWatchConfig {
    /// Engine clips_dir. The watcher resolves `mount_root` and
    /// reports mount paths as relative to this.
    pub clips_dir: PathBuf,
    /// Subdirectory of `clips_dir` to scan for `NEXUS_*` mounts.
    /// Default is `"usb"` — paired with the udev rule that mounts
    /// each device under `<clips_dir>/usb/<label>`.
    pub mount_subdir: PathBuf,
    /// How often to re-scan. Default [`DEFAULT_SCAN_INTERVAL`].
    pub scan_interval: Duration,
}

impl UsbWatchConfig {
    pub fn new(clips_dir: impl AsRef<Path>) -> Self {
        Self {
            clips_dir: clips_dir.as_ref().to_path_buf(),
            mount_subdir: PathBuf::from("usb"),
            scan_interval: DEFAULT_SCAN_INTERVAL,
        }
    }
}

/// Run the USB watch loop until `shutdown` resolves.
///
/// On every tick:
///
/// 1. Read the entries under `<clips_dir>/<mount_subdir>/` (silent
///    no-op if the directory is missing — the operator may not
///    have plugged anything in yet, or the udev rule may not be
///    installed on a dev box).
/// 2. Filter for entries whose name starts with [`USB_LABEL_PREFIX`]
///    and which are directories (mounts are always directories;
///    bare files in this dir are user error and ignored).
/// 3. Diff against the registry and emit
///    `STORAGE_USB_ATTACHED` / `STORAGE_USB_DETACHED` for each
///    delta. Updates the registry under the same lock so a recorder
///    `open()` racing with this scan sees a consistent state.
///
/// This function never returns an error — every IO failure is
/// logged at WARN and the loop continues. A persistently broken
/// watch root will just produce a steady drip of "could not read
/// mount root" warnings, which is the right signal-to-operator
/// behaviour (visible but non-fatal).
pub async fn run_usb_watch<F>(
    cfg: UsbWatchConfig,
    registry: UsbRegistry,
    store: Arc<Store>,
    bus: Arc<dyn Bus>,
    shutdown: F,
) where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let mount_root = cfg.clips_dir.join(&cfg.mount_subdir);
    info!(
        mount_root = %mount_root.display(),
        scan_interval_secs = cfg.scan_interval.as_secs(),
        "usb_watch starting"
    );

    let mut ticker = tokio::time::interval(cfg.scan_interval);
    // First tick fires immediately so a stick that's already
    // mounted at boot is registered without waiting an interval.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("usb_watch shutdown received");
                return;
            }
            _ = ticker.tick() => {
                scan_once(&cfg, &mount_root, &registry, &store, &bus).await;
            }
        }
    }
}

/// One scan pass. Extracted for unit tests.
pub async fn scan_once(
    cfg: &UsbWatchConfig,
    mount_root: &Path,
    registry: &UsbRegistry,
    store: &Arc<Store>,
    bus: &Arc<dyn Bus>,
) {
    let entries = match read_nexus_dirs(mount_root).await {
        Ok(v) => v,
        Err(e) => {
            // ENOENT is the common case (no usb mounts ever made
            // on this box) — log at debug so it doesn't spam
            // production logs. Other errors are noisy.
            if e.kind() == std::io::ErrorKind::NotFound {
                debug!(
                    path = %mount_root.display(),
                    "usb_watch: mount root missing (no NEXUS volumes attached?)"
                );
            } else {
                warn!(
                    path = %mount_root.display(),
                    error = %e,
                    "usb_watch: failed to read mount root"
                );
            }
            return;
        }
    };

    // Build the "currently visible" set first so we can compute
    // both attach and detach diffs in one pass under the registry
    // lock semantics.
    let mut visible: HashMap<String, PathBuf> = HashMap::new();
    for label in entries {
        let mount_relpath = cfg.mount_subdir.join(&label);
        visible.insert(label, mount_relpath);
    }

    // Detach: anything in the registry that isn't visible anymore.
    let known: Vec<String> = registry.inner.read().keys().cloned().collect();
    for label in known {
        if !visible.contains_key(&label) && registry.detach(&label) {
            info!(label = %label, "usb_watch: volume detached");
            let _ = bus
                .publish(
                    topic::STORAGE_USB_DETACHED,
                    &serde_json::json!({ "label": label }),
                )
                .await;
        }
    }

    // Attach: anything visible that's new (or path changed).
    for (label, mount_relpath) in visible {
        if registry.set_attached(&label, mount_relpath.clone()) {
            info!(
                label = %label,
                mount_relpath = %mount_relpath.display(),
                "usb_watch: volume attached"
            );

            // Upsert the `storage_backends` row BEFORE the registry
            // becomes consultable by the recorder via the bus event.
            // The recorder writes `motion_clips.hot_handle = "usb-<label>"`
            // and the schema's FK is `ON DELETE RESTRICT`, so the row
            // must exist before any clip can be opened on this volume.
            // The config_json carries the relative mount so an
            // operator inspecting the table sees where it lives.
            let handle = format!("usb-{label}");
            let cfg_json = serde_json::json!({
                "label": label,
                "mount_relpath": mount_relpath,
            })
            .to_string();
            if let Err(e) = store
                .upsert_storage_backend(&handle, "usb", &cfg_json)
                .await
            {
                warn!(
                    label = %label,
                    handle = %handle,
                    error = %e,
                    "usb_watch: failed to upsert storage_backends row; clips will fall back to local until next scan"
                );
                // Roll the registry back so the recorder doesn't try
                // to use a label whose backend row failed to land.
                registry.detach(&label);
                continue;
            }

            let _ = bus
                .publish(
                    topic::STORAGE_USB_ATTACHED,
                    &serde_json::json!({
                        "label": label,
                        "mount_path": mount_relpath,
                    }),
                )
                .await;
        }
    }
}

/// List subdirectory names under `root` that match the
/// `NEXUS_` prefix. Returns ENOENT verbatim so the caller can
/// pick its log level.
async fn read_nexus_dirs(root: &Path) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(root).await?;
    while let Some(entry) = rd.next_entry().await? {
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue, // Non-UTF8 mount name — ignore.
        };
        if !name.starts_with(USB_LABEL_PREFIX) {
            continue;
        }
        // Use file_type() (no extra stat) where possible; fall
        // back to metadata if the FS doesn't fill it (rare).
        let is_dir = match entry.file_type().await {
            Ok(ft) => ft.is_dir() || ft.is_symlink(),
            Err(_) => false,
        };
        if !is_dir {
            continue;
        }
        out.push(name);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use nexus_bus::BroadcastBus;
    use nexus_config::StoreConfig;
    use tempfile::TempDir;

    async fn fixture() -> (
        TempDir,
        UsbWatchConfig,
        UsbRegistry,
        Arc<Store>,
        Arc<dyn Bus>,
    ) {
        let dir = TempDir::new().unwrap();
        let cfg = UsbWatchConfig::new(dir.path());
        let registry = UsbRegistry::new();
        let store_cfg = StoreConfig {
            url: format!("sqlite:{}?mode=rwc", dir.path().join("nexus.db").display()),
            seed_from_config: false,
            duckdb_attach: false,
            duckdb_path: PathBuf::from("/tmp/unused.duckdb"),
        };
        let store = Arc::new(Store::open(&store_cfg).await.unwrap());
        let bus: Arc<dyn Bus> = Arc::new(BroadcastBus::new(64));
        (dir, cfg, registry, store, bus)
    }

    #[tokio::test]
    async fn registry_lookup_and_list_are_stable() {
        let r = UsbRegistry::new();
        assert!(r.lookup("NEXUS_X").is_none());
        assert!(r.set_attached("NEXUS_X", PathBuf::from("usb/NEXUS_X")));
        assert_eq!(r.lookup("NEXUS_X"), Some(PathBuf::from("usb/NEXUS_X")));
        // Re-attach with same path is a no-op.
        assert!(!r.set_attached("NEXUS_X", PathBuf::from("usb/NEXUS_X")));
        // Re-attach with new path counts as a change.
        assert!(r.set_attached("NEXUS_X", PathBuf::from("usb/OTHER_PATH")));
        assert!(r.detach("NEXUS_X"));
        assert!(!r.detach("NEXUS_X"));
    }

    #[tokio::test]
    async fn list_is_sorted_by_label() {
        let r = UsbRegistry::new();
        r.set_attached("NEXUS_B", PathBuf::from("usb/NEXUS_B"));
        r.set_attached("NEXUS_A", PathBuf::from("usb/NEXUS_A"));
        let v = r.list();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].label, "NEXUS_A");
        assert_eq!(v[1].label, "NEXUS_B");
    }

    #[tokio::test]
    async fn scan_skips_missing_mount_root_silently() {
        let (_dir, cfg, registry, store, bus) = fixture().await;
        // mount_root doesn't exist yet — must not panic, must not
        // emit anything.
        let mount_root = cfg.clips_dir.join(&cfg.mount_subdir);
        scan_once(&cfg, &mount_root, &registry, &store, &bus).await;
        assert!(registry.list().is_empty());
    }

    #[tokio::test]
    async fn scan_picks_up_nexus_dirs_and_ignores_others() {
        let (dir, cfg, registry, store, bus) = fixture().await;
        let mount_root = cfg.clips_dir.join(&cfg.mount_subdir);
        tokio::fs::create_dir_all(mount_root.join("NEXUS_VAULT"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(mount_root.join("TimeMachineBackups"))
            .await
            .unwrap();
        // Bare file with NEXUS_ prefix — must be ignored.
        tokio::fs::write(mount_root.join("NEXUS_NOT_A_DIR.txt"), b"x")
            .await
            .unwrap();

        scan_once(&cfg, &mount_root, &registry, &store, &bus).await;
        let v = registry.list();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].label, "NEXUS_VAULT");
        assert_eq!(v[0].mount_relpath, PathBuf::from("usb/NEXUS_VAULT"));

        // The schema FK requires a `storage_backends` row before
        // any clip can be opened on this volume — make sure the
        // watcher upserted it.
        let backends = store.list_storage_backends().await.unwrap();
        assert!(
            backends
                .iter()
                .any(|b| b.handle == "usb-NEXUS_VAULT" && b.kind == "usb"),
            "expected usb-NEXUS_VAULT/usb backend row, got {:?}",
            backends
                .iter()
                .map(|b| (&b.handle, &b.kind))
                .collect::<Vec<_>>()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn scan_emits_attach_then_detach() {
        let (_dir, cfg, registry, store, bus) = fixture().await;
        let mount_root = cfg.clips_dir.join(&cfg.mount_subdir);
        let label = "NEXUS_E2E";
        let mount = mount_root.join(label);
        tokio::fs::create_dir_all(&mount).await.unwrap();

        // Subscribe BEFORE the scan so we don't lose the broadcast.
        let mut sub = bus
            .subscribe_raw(topic::STORAGE_USB_ATTACHED)
            .await
            .unwrap();
        let mut det = bus
            .subscribe_raw(topic::STORAGE_USB_DETACHED)
            .await
            .unwrap();

        scan_once(&cfg, &mount_root, &registry, &store, &bus).await;
        let attached = sub.next().await.unwrap().unwrap();
        assert_eq!(attached["label"], label);

        tokio::fs::remove_dir_all(&mount).await.unwrap();
        scan_once(&cfg, &mount_root, &registry, &store, &bus).await;
        let detached = det.next().await.unwrap().unwrap();
        assert_eq!(detached["label"], label);
        assert!(registry.list().is_empty());
    }
}
