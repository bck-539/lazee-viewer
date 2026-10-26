//! Keeping the library current without the user asking: a queue so only
//! one scan runs at a time, a folder watcher for local drives, and a timer
//! that notices drives coming and going and rescans network shares.

use crate::{faces, AppState};
use lazee_core::facescan::FaceProgress;
use lazee_core::{ScanControl, ScanProgress};
use notify::{EventKind, RecursiveMode, Watcher};
use std::collections::{BTreeSet, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// How often to check which drives are connected.
const VOLUME_CHECK: Duration = Duration::from_secs(20);
/// How often to rescan network shares, whose change notifications can't be
/// trusted.
const NETWORK_RESCAN: Duration = Duration::from_secs(60 * 60);
/// Quiet time after the last file change before rescanning, so copying a
/// card's worth of photos triggers one scan rather than hundreds.
const SETTLE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct Scans {
    running: Mutex<Option<Arc<ScanControl>>>,
    pending: Mutex<Pending>,
}

#[derive(Default)]
struct Pending {
    all: bool,
    sources: BTreeSet<i64>,
}

impl Pending {
    fn add(&mut self, which: Option<&[i64]>) {
        match which {
            None => self.all = true,
            Some(ids) => self.sources.extend(ids),
        }
    }

    /// Takes what is queued: `None` if nothing, `Some(None)` for everything.
    fn take(&mut self) -> Option<Option<Vec<i64>>> {
        let p = std::mem::take(self);
        if p.all {
            Some(None)
        } else if !p.sources.is_empty() {
            Some(Some(p.sources.into_iter().collect()))
        } else {
            None
        }
    }
}

impl Scans {
    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    pub fn control(&self) -> Option<Arc<ScanControl>> {
        self.running.lock().unwrap().clone()
    }

    /// Stops the running scan and drops anything queued behind it.
    pub fn cancel(&self) {
        let running = self.running.lock().unwrap();
        *self.pending.lock().unwrap() = Pending::default();
        if let Some(ctl) = running.as_ref() {
            ctl.cancel();
        }
    }
}

/// Starts a scan of the given sources (all when `None`), or queues it if one
/// is already running.
pub fn request_scan(app: &AppHandle, which: Option<Vec<i64>>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut running = state.scans.running.lock().unwrap();
    if running.is_some() {
        state.scans.pending.lock().unwrap().add(which.as_deref());
        return Ok(());
    }
    let ctl = Arc::new(ScanControl::default());
    *running = Some(ctl.clone());
    drop(running);

    let app = app.clone();
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || run_scans(app, ctl, which))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn run_scans(app: AppHandle, mut ctl: Arc<ScanControl>, mut which: Option<Vec<i64>>) {
    let state = app.state::<AppState>();
    loop {
        let _ = app.emit("scan-started", ());
        let on_progress = throttled(|p: &ScanProgress| {
            let _ = app.emit("scan-progress", p);
        });
        let result = match &which {
            None => state.lib.scan(&ctl, &on_progress),
            Some(ids) => state.lib.scan_sources(ids, &ctl, &on_progress),
        }
        .map_err(|e| e.to_string());

        // After the files are up to date, look for faces in new photos,
        // unless the user stopped the scan.
        let find_faces = !ctl.is_cancelled() && faces::enabled(&state.lib);
        let next = take_next(&state, find_faces);
        let _ = app.emit("scan-finished", (result, next.is_some() || find_faces));
        if let Some(n) = next {
            which = n;
            ctl = new_control(&state);
            continue;
        }
        if !find_faces {
            break;
        }

        ctl = new_control(&state);
        let _ = app.emit("faces-started", ());
        let on_progress = throttled(|p: &FaceProgress| {
            let _ = app.emit("face-progress", p);
        });
        let result = faces::with_engine(&app, |engine| {
            lazee_core::facescan::scan(&state.lib, engine, &ctl, &on_progress).map_err(|e| e.to_string())
        })
        .and_then(|r| r);
        if let Err(e) = &result {
            log::warn!("face scan: {e}");
        }
        let next = take_next(&state, false);
        let _ = app.emit("faces-finished", (result, next.is_some()));
        match next {
            Some(n) => {
                which = n;
                ctl = new_control(&state);
            }
            None => break,
        }
    }
}

/// Takes the next queued scan. When there is none, marks scanning as
/// finished unless `more` work follows.
fn take_next(state: &AppState, more: bool) -> Option<Option<Vec<i64>>> {
    let mut running = state.scans.running.lock().unwrap();
    let next = state.scans.pending.lock().unwrap().take();
    if next.is_none() && !more {
        *running = None;
    }
    next
}

fn new_control(state: &AppState) -> Arc<ScanControl> {
    let ctl = Arc::new(ScanControl::default());
    *state.scans.running.lock().unwrap() = Some(ctl.clone());
    ctl
}

/// Progress arrives far more often than the UI needs it.
fn throttled<P>(emit: impl Fn(&P) + Sync) -> impl Fn(&P) + Sync {
    let last = Mutex::new(Instant::now() - Duration::from_secs(1));
    move |p: &P| {
        let mut last = last.lock().unwrap();
        if last.elapsed() >= Duration::from_millis(150) {
            *last = Instant::now();
            emit(p);
        }
    }
}

/// Watches local source folders and queues a rescan of a source shortly
/// after files in it change.
#[derive(Default)]
pub struct FolderWatch {
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

/// (Re)starts watching every source on a connected local or USB drive.
pub fn rewatch(app: &AppHandle) {
    let state = app.state::<AppState>();
    let targets = match state.lib.watch_targets() {
        Ok(t) => t,
        Err(e) => {
            log::warn!("could not list folders to watch: {e}");
            return;
        }
    };
    let (tx, rx) = mpsc::channel::<i64>();
    let watched = targets.clone();
    let handler = move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in &event.paths {
            if !is_interesting(path) {
                continue;
            }
            if let Some((id, _)) = watched.iter().find(|(_, root)| path.starts_with(root)) {
                let _ = tx.send(*id);
            }
        }
    };
    let mut watcher = match notify::recommended_watcher(handler) {
        Ok(w) => w,
        Err(e) => {
            log::warn!("folder watching is unavailable: {e}");
            return;
        }
    };
    for (_, root) in &targets {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            log::warn!("cannot watch {}: {e}", root.display());
        }
    }
    // Replacing the old watcher drops it, which ends its debounce thread.
    *state.watch.watcher.lock().unwrap() = Some(watcher);

    let app = app.clone();
    std::thread::spawn(move || debounce(app, rx));
}

/// Image files, and paths without an extension, which may be folders being
/// moved or deleted.
fn is_interesting(path: &std::path::Path) -> bool {
    path.extension().is_none() || lazee_core::formats::classify(path).is_some()
}

fn debounce(app: AppHandle, rx: mpsc::Receiver<i64>) {
    while let Ok(first) = rx.recv() {
        let mut ids = BTreeSet::from([first]);
        loop {
            match rx.recv_timeout(SETTLE) {
                Ok(id) => {
                    ids.insert(id);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        log::info!("files changed in sources {ids:?}; rescanning");
        let _ = request_scan(&app, Some(ids.into_iter().collect()));
    }
}

/// Runs for the app's lifetime: notices drives being connected or removed,
/// and rescans network shares every so often.
pub fn start_monitor(app: AppHandle) {
    std::thread::Builder::new()
        .name("volumes".into())
        .spawn(move || {
            let state = app.state::<AppState>();
            let online = |lib: &lazee_core::Library| -> HashSet<i64> {
                lib.sources().unwrap_or_default().into_iter().filter(|s| s.online).map(|s| s.id).collect()
            };
            let mut before = online(&state.lib);
            let mut last_network_scan = Instant::now();
            loop {
                std::thread::sleep(VOLUME_CHECK);
                state.lib.refresh_volumes();
                let now = online(&state.lib);
                if now != before {
                    let connected: Vec<i64> = now.difference(&before).copied().collect();
                    log::info!("connected drives changed; newly online sources {connected:?}");
                    let _ = app.emit("volumes-changed", ());
                    rewatch(&app);
                    if !connected.is_empty() {
                        let _ = request_scan(&app, Some(connected));
                    }
                    before = now;
                }
                if last_network_scan.elapsed() >= NETWORK_RESCAN {
                    last_network_scan = Instant::now();
                    let network: Vec<i64> = state
                        .lib
                        .sources()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| s.online && s.volume_kind == lazee_core::volume::VolumeKind::Network)
                        .map(|s| s.id)
                        .collect();
                    if !network.is_empty() {
                        let _ = request_scan(&app, Some(network));
                    }
                }
            }
        })
        .expect("start the drive monitor");
}
