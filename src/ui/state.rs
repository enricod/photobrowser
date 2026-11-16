use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use lru::LruCache;

use super::folderpanel::FolderPanel;
use super::item_object::ItemObject;
use super::pathbar::{PathBar, display_dir};
use super::viewer::{Viewer, info_text};
use crate::catalog::diff::diff_items;
use crate::catalog::rating::{self, RatingChange};
use crate::catalog::sync::{self, ScanResult};
use crate::config::Config;
use crate::decode::{self, Quality, Rgba, THUMB_EDGE};
use crate::exif::{self, ExifInfo};
use crate::fileops::{self, ConflictPolicy, OpKind, TransferOutcome};
use crate::filter::{self, FilterState, SortState};
use crate::index::{FileRecord, Index};
use crate::jobs::{CancelToken, JobPool, Priority};
use crate::media::{Item, ItemId, MediaFile};
use crate::organize::{self, OrganizePattern};
use crate::thumbcache::DiskCache;
use crate::xmp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zoom {
    Fit,
    Actual,
}

/// Results sent from worker threads to the GTK main thread.
pub enum Msg {
    /// `serial` identifies the request; a result whose serial is no longer pending is stale.
    Thumb {
        id: ItemId,
        generation: u64,
        serial: u64,
        result: Result<Rgba, String>,
    },
    Scanned {
        generation: u64,
        result: Result<ScanResult, String>,
    },
    XmpWritten {
        path: PathBuf,
        result: Result<i64, String>,
    },
    Full {
        id: ItemId,
        generation: u64,
        zoom: Zoom,
        result: Result<Rgba, String>,
    },
    Exif {
        id: ItemId,
        info: ExifInfo,
    },
    Transferred {
        id: ItemId,
        kind: OpKind,
        outcome: TransferOutcome,
    },
    TransfersFinished {
        batch: BatchInfo,
        done: usize,
        skipped: usize,
        failed: usize,
        first_failure: Option<String>,
        /// Full text of every failure whose rollback didn't complete (files may be left
        /// in both places): too important to only show truncated in a toast.
        rollback_failures: Vec<String>,
    },
}

pub struct Widgets {
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub stack: gtk::Stack,
    pub grid: gtk::GridView,
    pub counter: gtk::Label,
    pub filters: gtk::Box,
    pub viewer: Rc<Viewer>,
    pub path_bar: PathBar,
    pub folder_panel: FolderPanel,
    /// The grid page: photos, with the folder panel on the right.
    pub folder_split: adw::OverlaySplitView,
    /// Main-menu section: Move, Copy, Organize, Open in ‹favourite›, Open With…
    pub file_menu: gio::Menu,
}

/// Position of "Open in ‹favourite›" in `Widgets::file_menu`.
pub const OPEN_FAVOURITE_MENU_POS: i32 = 3;

pub struct AppState {
    pub config: RefCell<Config>,
    pub index: RefCell<Index>,
    /// The open folder's subfolders, from its last scan (see `refresh_folder_panel`).
    subfolders: RefCell<Vec<PathBuf>>,
    /// Why the on-disk index could not be opened, if the app fell back to an in-memory one.
    index_unavailable: Option<String>,
    pub pool: JobPool,
    pub disk: Arc<DiskCache>,
    pub tx: async_channel::Sender<Msg>,
    /// A second handle on the message channel, used only by `shutdown` to process the
    /// `XmpWritten` results the main loop will no longer get to.
    msg_rx: async_channel::Receiver<Msg>,
    pub store: gio::ListStore,
    pub filter: gtk::CustomFilter,
    pub sorter: gtk::CustomSorter,
    pub selection: gtk::MultiSelection,
    pub filter_state: Rc<RefCell<FilterState>>,
    pub sort_state: Rc<Cell<SortState>>,
    pub widgets: OnceCell<Widgets>,
    objects: RefCell<HashMap<ItemId, ItemObject>>,
    /// How many list cells currently show each item (grid + filmstrip).
    bound: RefCell<HashMap<ItemId, u32>>,
    /// In-flight thumbnail request per item: (request serial, cancel token).
    pending_thumbs: RefCell<HashMap<ItemId, (u64, CancelToken)>>,
    next_thumb_serial: Cell<u64>,
    thumb_lru: RefCell<LruCache<ItemId, ()>>,
    /// Fit-to-screen decodes (current image plus prefetched neighbours).
    full_cache: RefCell<LruCache<ItemId, gdk::Texture>>,
    /// 100 % decodes: kept for the current and previous image only (spec §5.3).
    actual_cache: RefCell<LruCache<ItemId, gdk::Texture>>,
    /// In-flight full-size decodes: the priority they were queued at and their cancel token.
    pending_full: RefCell<HashMap<(ItemId, Zoom), (Priority, CancelToken)>>,
    exif_cache: RefCell<HashMap<ItemId, ExifInfo>>,
    folder: RefCell<Option<PathBuf>>,
    /// Bumped on every folder change; results from older generations are dropped.
    generation: Cell<u64>,
    next_id: Cell<ItemId>,
    first_scan_done: Cell<bool>,
    /// `None` once the window is closing: the writer thread then drains and exits.
    xmp_tx: RefCell<Option<mpsc::Sender<XmpJob>>>,
    xmp_writer: RefCell<Option<std::thread::JoinHandle<()>>>,
    xmp_inflight: RefCell<HashMap<PathBuf, u32>>,
    xmp_failures: Cell<u32>,
    /// Pending debounced config-file write, if any (see `zoom_thumbs`).
    config_save_source: RefCell<Option<glib::SourceId>>,
    /// Position `show_at` last displayed: where the viewer continues if its item leaves
    /// the visible list (see `sync_viewer`).
    viewer_pos: Cell<Option<u32>>,
    /// A `sync_viewer` run is already queued for this burst of list changes.
    viewer_sync_queued: Cell<bool>,
    /// Items accepted into a move/copy batch (spawned or waiting on XMP writes to drain), by
    /// their media paths: `rate()`, `start_transfer()`, `on_scanned()` and `flush_dirty_xmp()`
    /// all leave those paths alone to avoid racing the transfer. Survives a folder change:
    /// the transfers keep running, and a reopened folder's new objects match by path.
    transferring: RefCell<InFlight>,
    /// Move/copy batches accepted and not yet finished (waiting on XMP writes or running),
    /// and how many of their items are still to go. Like `transferring`, these survive
    /// a folder change: they gate closing the window, which would kill the transfer thread.
    batches_active: Cell<usize>,
    transfer_items_left: Cell<usize>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    rescan_source: RefCell<Option<glib::SourceId>>,
    /// A rescan came in while one was already running: run once more when it finishes,
    /// instead of starting a second scan thread that could race the first.
    rescan_pending: Cell<bool>,
    scan_running: Cell<bool>,
}

/// 100 % textures kept in memory: the current and the previous image.
const ACTUAL_CACHE_ITEMS: usize = 2;

fn capacity(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n.max(1)).unwrap()
}

/// A move/copy batch that was accepted (ids already marked `transferring`) but is still
/// waiting for its source paths to clear `xmp_inflight` before the transfer thread spawns.
struct PendingBatch {
    jobs: Vec<TransferJob>,
    batch: BatchInfo,
    policy: ConflictPolicy,
}

/// One item of a move/copy batch and the folder it goes to.
#[derive(Debug, Clone)]
pub struct TransferJob {
    pub id: ItemId,
    pub sources: Vec<PathBuf>,
    pub dest: PathBuf,
}

/// What a batch is, for its messages: a move/copy into `target`, or organizing into dated
/// folders under `target` (`undated` items were left out for lack of a shot date).
#[derive(Debug, Clone)]
pub struct BatchInfo {
    pub kind: OpKind,
    pub target: PathBuf,
    pub organize: bool,
    pub undated: usize,
}

impl BatchInfo {
    fn destination_text(&self) -> String {
        if self.organize {
            format!("into dated folders under {}", display_dir(&self.target))
        } else {
            format!("to {}", display_dir(&self.target))
        }
    }

    fn started_text(&self, items: usize) -> String {
        let verb = match self.kind {
            OpKind::Move => "Moving",
            OpKind::Copy => "Copying",
        };
        format!(
            "{verb} {items} item{} {}…",
            if items == 1 { "" } else { "s" },
            self.destination_text()
        )
    }

    /// Summary without the failure details, which the caller appends.
    fn finished_text(&self, done: usize, skipped: usize) -> String {
        let verb = match self.kind {
            OpKind::Move => "Moved",
            OpKind::Copy => "Copied",
        };
        let mut text = format!(
            "{verb} {done} item{} {}",
            if done == 1 { "" } else { "s" },
            self.destination_text()
        );
        if skipped > 0 {
            text.push_str(&format!(", {skipped} skipped"));
        }
        if self.undated > 0 {
            text.push_str(&format!(", {} without a shot date", self.undated));
        }
        text
    }
}

/// Media paths of the items in running or pending move/copy batches, by item id.
#[derive(Debug, Default)]
struct InFlight {
    by_id: HashMap<ItemId, Vec<PathBuf>>,
}

impl InFlight {
    fn add(&mut self, id: ItemId, paths: Vec<PathBuf>) {
        self.by_id.insert(id, paths);
    }

    /// Ends an item's part of its batch, whatever the outcome. Unknown ids are ignored.
    fn finish(&mut self, id: ItemId) {
        self.by_id.remove(&id);
    }

    fn paths(&self) -> HashSet<PathBuf> {
        self.by_id.values().flatten().cloned().collect()
    }

    fn contains_any<'a>(&self, mut files: impl Iterator<Item = &'a std::path::Path>) -> bool {
        files.any(|f| self.by_id.values().flatten().any(|p| p == f))
    }
}

/// The batch job for `obj`'s files going into `dest`.
fn transfer_job(obj: &ItemObject, dest: PathBuf) -> TransferJob {
    obj.with_item(|i| TransferJob {
        id: i.id,
        sources: i.files.iter().map(|f| f.path.clone()).collect(),
        dest,
    })
}

/// Marker `fileops` puts in a failure message when undoing a partial transfer failed.
const ROLLBACK_INCOMPLETE: &str = "rollback incomplete";

/// True once none of a batch's media paths still have a queued/in-flight XMP write.
///
/// Spawning the transfer while a write is in flight can race the sidecar writer thread:
/// the transfer's plan may be built before a sidecar exists (so it isn't moved, and the
/// writer then recreates it at the old, now-orphaned path), or the writer's save can land
/// at the old path after the file itself was already renamed away. Waiting until every
/// source path is clear avoids both races.
fn batch_ready(jobs: &[TransferJob], inflight: &HashMap<PathBuf, u32>) -> bool {
    jobs.iter()
        .all(|j| j.sources.iter().all(|p| !inflight.contains_key(p)))
}

pub fn texture_from(img: Rgba) -> gdk::Texture {
    let stride = img.width as usize * 4;
    let bytes = glib::Bytes::from_owned(img.data);
    gdk::MemoryTexture::new(
        img.width as i32,
        img.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    )
    .upcast()
}

pub struct XmpJob {
    pub path: PathBuf,
    pub rating: u8,
    pub rejected: bool,
}

/// One writer thread: sidecar writes are serialised and ordered. It exits once the
/// returned sender is dropped and every queued job has been written.
fn spawn_xmp_writer(
    tx: async_channel::Sender<Msg>,
) -> (mpsc::Sender<XmpJob>, std::thread::JoinHandle<()>) {
    spawn_xmp_writer_with(tx, |job| {
        xmp::write_rating(&job.path, job.rating, job.rejected).map_err(|e| e.to_string())
    })
}

/// The writer loop, with the per-file write as a parameter (tests pass their own). A panic
/// while writing one sidecar (e.g. inside the exiv2 bindings) becomes an error result for
/// that file: the thread lives on, so later ratings still reach their sidecars.
fn spawn_xmp_writer_with(
    tx: async_channel::Sender<Msg>,
    write: impl Fn(&XmpJob) -> Result<i64, String> + Send + 'static,
) -> (mpsc::Sender<XmpJob>, std::thread::JoinHandle<()>) {
    let (job_tx, job_rx) = mpsc::channel::<XmpJob>();
    let handle = std::thread::Builder::new()
        .name("pb-xmp".into())
        .spawn(move || {
            for job in job_rx {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(&job)))
                    .unwrap_or_else(|_| Err("internal error while writing the sidecar".into()));
                if tx
                    .send_blocking(Msg::XmpWritten {
                        path: job.path,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .expect("failed to spawn XMP writer");
    (job_tx, handle)
}

impl AppState {
    pub fn new(
        tx: async_channel::Sender<Msg>,
        msg_rx: async_channel::Receiver<Msg>,
    ) -> Rc<AppState> {
        let config = Config::load_from(&Config::default_path());
        let (index, index_unavailable) = match Index::open(&Index::default_path()) {
            Ok(index) => (index, None),
            Err(e) => {
                eprintln!("index unavailable ({e}); using an in-memory index");
                let index = Index::open_in_memory().expect("SQLite is unusable");
                (index, Some(e.to_string()))
            }
        };
        let disk = DiskCache::new(DiskCache::default_dir(), config.disk_cache_mb * 1024 * 1024)
            .expect("cannot create the thumbnail cache directory");

        let filter_state = Rc::new(RefCell::new(FilterState::default()));
        let sort_state = Rc::new(Cell::new(SortState::default()));
        let fs = filter_state.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            obj.downcast_ref::<ItemObject>()
                .is_some_and(|o| o.with_item(|i| fs.borrow().matches(i)))
        });
        let ss = sort_state.clone();
        let sorter = gtk::CustomSorter::new(move |a, b| {
            let a = a.downcast_ref::<ItemObject>().expect("ItemObject");
            let b = b.downcast_ref::<ItemObject>().expect("ItemObject");
            a.with_item(|ia| b.with_item(|ib| filter::compare(ia, ib, ss.get())))
                .into()
        });
        let store = gio::ListStore::new::<ItemObject>();
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), Some(sorter.clone()));
        let selection = gtk::MultiSelection::new(Some(sorted));
        let thumb_cap = config.thumb_cache_items;
        let full_cap = config.full_cache_items;
        let (xmp_tx, xmp_writer) = spawn_xmp_writer(tx.clone());

        Rc::new(AppState {
            config: RefCell::new(config),
            index: RefCell::new(index),
            subfolders: RefCell::default(),
            index_unavailable,
            pool: JobPool::new(JobPool::default_threads()),
            disk: Arc::new(disk),
            tx,
            msg_rx,
            store,
            filter,
            sorter,
            selection,
            filter_state,
            sort_state,
            widgets: OnceCell::new(),
            objects: RefCell::default(),
            bound: RefCell::default(),
            pending_thumbs: RefCell::default(),
            next_thumb_serial: Cell::new(0),
            thumb_lru: RefCell::new(LruCache::new(capacity(thumb_cap))),
            full_cache: RefCell::new(LruCache::new(capacity(full_cap))),
            actual_cache: RefCell::new(LruCache::new(capacity(ACTUAL_CACHE_ITEMS))),
            pending_full: RefCell::default(),
            exif_cache: RefCell::default(),
            folder: RefCell::new(None),
            generation: Cell::new(0),
            next_id: Cell::new(0),
            first_scan_done: Cell::new(false),
            xmp_tx: RefCell::new(Some(xmp_tx)),
            xmp_writer: RefCell::new(Some(xmp_writer)),
            xmp_inflight: RefCell::default(),
            xmp_failures: Cell::new(0),
            config_save_source: RefCell::new(None),
            viewer_pos: Cell::new(None),
            viewer_sync_queued: Cell::new(false),
            transferring: RefCell::default(),
            batches_active: Cell::new(0),
            transfer_items_left: Cell::new(0),
            monitor: RefCell::new(None),
            rescan_source: RefCell::new(None),
            rescan_pending: Cell::new(false),
            scan_running: Cell::new(false),
        })
    }

    pub fn widgets(&self) -> &Widgets {
        self.widgets
            .get()
            .expect("widgets are set during window construction")
    }

    pub fn toast(&self, text: &str) {
        self.widgets().toasts.add_toast(adw::Toast::new(text));
    }

    /// Warnings about the session that the user should see once the window is up.
    pub fn show_startup_warnings(&self) {
        if let Some(reason) = &self.index_unavailable {
            let toast = adw::Toast::new(&format!(
                "Index unavailable ({reason}): ratings are only saved to sidecars this session"
            ));
            toast.set_timeout(0); // stays until dismissed
            self.widgets().toasts.add_toast(toast);
        }
    }

    pub fn folder(&self) -> Option<PathBuf> {
        self.folder.borrow().clone()
    }

    pub fn object(&self, id: ItemId) -> Option<ItemObject> {
        self.objects.borrow().get(&id).cloned()
    }

    fn alloc_id(&self) -> ItemId {
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        id
    }

    pub fn refilter(&self) {
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn resort(&self) {
        self.sorter.changed(gtk::SorterChange::Different);
    }

    pub fn update_counter(&self) {
        if let Some(w) = self.widgets.get() {
            w.counter.set_label(&format!(
                "{} / {}",
                self.selection.n_items(),
                self.store.n_items()
            ));
        }
    }

    pub fn handle(self: &Rc<Self>, msg: Msg) {
        match msg {
            Msg::Thumb {
                id,
                generation,
                serial,
                result,
            } => self.on_thumb(id, generation, serial, result),
            Msg::Scanned { generation, result } => self.on_scanned(generation, result),
            Msg::XmpWritten { path, result } => self.on_xmp_written(path, result),
            Msg::Full {
                id,
                generation,
                zoom,
                result,
            } => self.on_full(id, generation, zoom, result),
            Msg::Exif { id, info } => self.on_exif(id, info),
            Msg::Transferred { id, kind, outcome } => self.on_transferred(id, kind, outcome),
            Msg::TransfersFinished {
                batch,
                done,
                skipped,
                failed,
                first_failure,
                rollback_failures,
            } => self.on_transfers_finished(
                &batch,
                done,
                skipped,
                failed,
                first_failure,
                rollback_failures,
            ),
        }
    }

    // ---- folders -------------------------------------------------------------

    pub fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Open Folder")
            .modal(true)
            .build();
        if let Some(folder) = self.folder() {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let s = self.clone();
        dialog.select_folder(
            Some(&self.widgets().window),
            gio::Cancellable::NONE,
            move |res| {
                if let Some(path) = res.ok().and_then(|f| f.path()) {
                    s.open_folder(path);
                }
            },
        );
    }

    pub fn open_folder(self: &Rc<Self>, folder: PathBuf) {
        self.reset();
        *self.folder.borrow_mut() = Some(folder.clone());
        {
            let mut config = self.config.borrow_mut();
            config.last_folder = Some(folder.clone());
            let _ = config.save_to(&Config::default_path());
        }
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder.display().to_string());
        self.widgets()
            .window
            .set_title(Some(&format!("{name} — Photobrowser")));
        self.widgets().path_bar.set_folder(Some(&folder));
        // Subfolders follow with the scan; a filter typed for the previous folder goes.
        self.widgets().folder_panel.clear_filter();
        self.refresh_folder_panel();

        let known = self.index.borrow().load_folder(&folder).unwrap_or_default();
        let mut items = sync::items_from_records(&known);
        for item in &mut items {
            item.id = self.alloc_id();
        }
        self.add_objects(items);
        self.widgets().stack.set_visible_child_name("grid");
        self.start_scan(folder.clone(), known);
        self.watch_folder(&folder);
    }

    /// Forgets the current folder: cancels queued work, clears models and caches.
    fn reset(&self) {
        self.generation.set(self.generation.get() + 1);
        self.first_scan_done.set(false);
        self.pool.clear();
        for (_, (_, token)) in self.pending_thumbs.borrow_mut().drain() {
            token.cancel();
        }
        // Empty the model first: its cells unbind (and touch the LRU) while items still exist.
        self.store.remove_all();
        self.thumb_lru.borrow_mut().clear();
        self.full_cache.borrow_mut().clear();
        self.actual_cache.borrow_mut().clear();
        for (_, (_, token)) in self.pending_full.borrow_mut().drain() {
            token.cancel();
        }
        self.exif_cache.borrow_mut().clear();
        if let Some(w) = self.widgets.get()
            && w.stack.visible_child_name().as_deref() == Some("viewer")
        {
            w.stack.set_visible_child_name("grid");
        }
        self.objects.borrow_mut().clear();
        self.subfolders.borrow_mut().clear();
        self.filter_state.borrow_mut().pinned.clear();
        // In-flight transfers (spawned, or still waiting on pending XMP writes) keep running
        // on disk paths, independent of which folder is open: `transferring` is kept, so
        // the guard still covers those paths when the folder is reopened.
        *self.folder.borrow_mut() = None;
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            monitor.cancel();
        }
        if let Some(source) = self.rescan_source.borrow_mut().take() {
            source.remove();
        }
        self.rescan_pending.set(false);
        self.scan_running.set(false);
    }

    pub fn folder_lost(self: &Rc<Self>, message: &str) {
        self.reset();
        self.widgets().stack.set_visible_child_name("empty");
        self.widgets().window.set_title(Some("Photobrowser"));
        self.widgets().path_bar.set_folder(None);
        self.refresh_folder_panel();
        self.toast(message);
    }

    fn add_objects(&self, items: Vec<Item>) {
        let objs: Vec<ItemObject> = items.into_iter().map(ItemObject::new).collect();
        {
            let mut map = self.objects.borrow_mut();
            for o in &objs {
                map.insert(o.id(), o.clone());
            }
        }
        self.store.extend_from_slice(&objs);
    }

    /// Media paths of the items currently in a move/copy batch.
    fn transferring_paths(&self) -> HashSet<PathBuf> {
        self.transferring.borrow().paths()
    }

    /// True if any file of `obj` is part of a running or pending move/copy.
    fn is_transferring(&self, obj: &ItemObject) -> bool {
        let transferring = self.transferring.borrow();
        obj.with_item(|i| transferring.contains_any(i.files.iter().map(|f| f.path.as_path())))
    }

    fn remove_object(&self, id: ItemId) {
        let removed = self.objects.borrow_mut().remove(&id);
        if let Some(obj) = removed
            && let Some(pos) = self.store.find(&obj)
        {
            self.store.remove(pos);
        }
        self.cancel_thumb(id);
        self.thumb_lru.borrow_mut().pop(&id);
    }

    fn start_scan(&self, folder: PathBuf, known: HashMap<PathBuf, FileRecord>) {
        self.scan_running.set(true);
        let tx = self.tx.clone();
        let generation = self.generation.get();
        std::thread::Builder::new()
            .name("pb-scan".into())
            .spawn(move || {
                let result = sync::scan_folder(&folder, &known).map_err(|e| e.to_string());
                let _ = tx.send_blocking(Msg::Scanned { generation, result });
            })
            .expect("failed to spawn scan thread");
    }

    /// Never overlaps a scan already in flight: if one is running, just remembers to run
    /// again once it finishes (see `on_scanned`) instead of spawning a second scan thread.
    pub fn rescan(&self) {
        if self.scan_running.get() {
            self.rescan_pending.set(true);
            return;
        }
        let Some(folder) = self.folder() else { return };
        let known = self.index.borrow().load_folder(&folder).unwrap_or_default();
        self.start_scan(folder, known);
    }

    fn on_scanned(self: &Rc<Self>, generation: u64, result: Result<ScanResult, String>) {
        if generation != self.generation.get() {
            return;
        }
        self.scan_running.set(false);
        let mut scan = match result {
            Ok(scan) => scan,
            Err(e) => {
                self.folder_lost(&format!("Couldn't read the folder: {e}"));
                return;
            }
        };
        *self.subfolders.borrow_mut() = std::mem::take(&mut scan.folders);
        self.refresh_folder_panel();
        // A file being moved away right now is "missing" to the scan, but its row is still
        // needed: `on_transferred` re-keys it to the destination.
        let moving = self.transferring_paths();
        let removed: Vec<PathBuf> = scan
            .removed
            .into_iter()
            .filter(|p| !moving.contains(p))
            .collect();
        {
            let mut index = self.index.borrow_mut();
            if let Err(e) = index.upsert_many(&scan.upserts) {
                eprintln!("index update failed: {e}");
            }
            if let Err(e) = index.remove_paths(&removed) {
                eprintln!("index cleanup failed: {e}");
            }
        }

        let old: Vec<Item> = self
            .objects
            .borrow()
            .values()
            .map(ItemObject::item)
            .collect();
        let diff = diff_items(&old, scan.items, || self.alloc_id());
        for id in diff.removed {
            self.remove_object(id);
        }
        // The viewer keeps its last item after closing: only refresh it while it is shown.
        let viewed = self
            .viewer_active()
            .then(|| self.widgets().viewer.current_id())
            .flatten();
        for update in diff.updated {
            let Some(obj) = self.object(update.item.id) else {
                continue;
            };
            let mut item = update.item;
            let changed_on_disk = obj.with_item(|old| files_changed(&old.files, &item.files));
            // The index is the truth for ratings: it keeps pending local edits.
            let rec = self
                .index
                .borrow()
                .get(&item.display_file().path)
                .ok()
                .flatten();
            if let Some(rec) = rec {
                item.rating = rec.rating;
                item.rejected = rec.rejected;
            }
            obj.set_item(item);
            if changed_on_disk {
                self.forget_full_size(obj.id());
            }
            if viewed == Some(obj.id()) {
                self.refresh_viewed(&obj, changed_on_disk);
            }
            if update.display_changed {
                // A request still in flight decodes the old file: supersede it.
                self.cancel_thumb(obj.id());
                obj.set_thumbnail(None);
                obj.set_error(None);
                self.thumb_lru.borrow_mut().pop(&obj.id());
                if self.bound.borrow().contains_key(&obj.id()) {
                    self.request_thumb(&obj, Priority::Visible);
                }
            }
        }
        let warm: Vec<MediaFile> = if self.first_scan_done.replace(true) {
            diff.added
                .iter()
                .map(|i| i.display_file().clone())
                .collect()
        } else {
            let existing = self.objects.borrow();
            existing
                .values()
                .map(|o| o.with_item(|i| i.display_file().clone()))
                .chain(diff.added.iter().map(|i| i.display_file().clone()))
                .collect()
        };
        self.add_objects(diff.added);
        self.refilter();
        self.resort();
        self.warm_disk_cache(warm);
        self.flush_dirty_xmp();
        // Only now, after this scan's index writes and diff have been applied, is it safe
        // to let a rescan requested mid-scan run: it must see this scan's own results (via
        // the index) rather than a stale snapshot, or it would redo this scan's work and
        // could clobber a rating set on a newly-appeared file in the meantime.
        if self.rescan_pending.replace(false) {
            self.rescan();
        }
    }

    // ---- folder monitoring -----------------------------------------------

    fn watch_folder(self: &Rc<Self>, folder: &std::path::Path) {
        let dir = gio::File::for_path(folder);
        let monitor = match dir
            .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("cannot watch {}: {e}", folder.display());
                return;
            }
        };
        let s = self.clone();
        monitor.connect_changed(move |_, changed, _, event| {
            let is_dir_itself = changed.equal(&dir);
            match event {
                gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedOut
                    if is_dir_itself =>
                {
                    s.folder_lost("The folder was removed");
                }
                gio::FileMonitorEvent::Unmounted => {
                    s.folder_lost("The folder's drive was unmounted")
                }
                _ => s.schedule_rescan(),
            }
        });
        *self.monitor.borrow_mut() = Some(monitor);
    }

    /// Coalesces bursts of file events (a card copy, our own sidecar writes) into one rescan.
    fn schedule_rescan(self: &Rc<Self>) {
        if let Some(source) = self.rescan_source.borrow_mut().take() {
            source.remove();
        }
        let s = self.clone();
        let source = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            s.rescan_source.borrow_mut().take();
            s.rescan();
        });
        *self.rescan_source.borrow_mut() = Some(source);
    }

    // ---- thumbnails ----------------------------------------------------------

    pub fn bind_cell(self: &Rc<Self>, obj: &ItemObject) {
        *self.bound.borrow_mut().entry(obj.id()).or_insert(0) += 1;
        self.request_thumb(obj, Priority::Visible);
    }

    pub fn unbind_cell(&self, obj: &ItemObject) {
        let id = obj.id();
        let still_bound = {
            let mut bound = self.bound.borrow_mut();
            match bound.get_mut(&id) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    true
                }
                _ => {
                    bound.remove(&id);
                    false
                }
            }
        };
        if still_bound {
            return;
        }
        self.cancel_thumb(id);
        // A texture kept while bound but pushed out of the LRU goes back under its control.
        let tracked = self.thumb_lru.borrow().contains(&id);
        if obj.thumbnail().is_some() && !tracked {
            self.track_thumb(id);
        }
    }

    pub fn request_thumb(&self, obj: &ItemObject, priority: Priority) {
        let id = obj.id();
        if obj.thumbnail().is_some() {
            self.thumb_lru.borrow_mut().promote(&id);
            return;
        }
        if obj.has_error() || self.pending_thumbs.borrow().contains_key(&id) {
            return;
        }
        let token = CancelToken::new();
        let serial = self.next_thumb_serial.get() + 1;
        self.next_thumb_serial.set(serial);
        self.pending_thumbs
            .borrow_mut()
            .insert(id, (serial, token.clone()));
        let file = obj.with_item(|i| i.display_file().clone());
        let (disk, tx, generation) = (self.disk.clone(), self.tx.clone(), self.generation.get());
        self.pool.submit(priority, token, move || {
            let result = match disk.load(&file) {
                Some(img) => Ok(img),
                None => {
                    decode::guarded(|| decode::decode_for_display(&file, THUMB_EDGE, Quality::Fast))
                        .inspect(|img| {
                            let _ = disk.store(&file, img);
                        })
                        .map_err(|e| e.to_string())
                }
            };
            let _ = tx.send_blocking(Msg::Thumb {
                id,
                generation,
                serial,
                result,
            });
        });
    }

    /// Cancels and forgets the in-flight thumbnail request for `id`, if any.
    fn cancel_thumb(&self, id: ItemId) {
        let pending = self.pending_thumbs.borrow_mut().remove(&id);
        if let Some((_, token)) = pending {
            token.cancel();
        }
    }

    fn on_thumb(&self, id: ItemId, generation: u64, serial: u64, result: Result<Rgba, String>) {
        if generation != self.generation.get() {
            return;
        }
        {
            // Only the current request for this item may install its result.
            let mut pending = self.pending_thumbs.borrow_mut();
            match pending.get(&id) {
                Some((current, _)) if *current == serial => {
                    pending.remove(&id);
                }
                _ => return,
            }
        }
        let Some(obj) = self.object(id) else { return };
        match result {
            Ok(img) => {
                obj.set_thumbnail(Some(texture_from(img)));
                self.track_thumb(id);
            }
            Err(e) => obj.set_error(Some(e)),
        }
    }

    /// Adds `id` to the thumbnail LRU; evicts the least recent texture unless it is on screen.
    fn track_thumb(&self, id: ItemId) {
        let evicted = self.thumb_lru.borrow_mut().push(id, ());
        if let Some((old_id, ())) = evicted {
            let on_screen = self.bound.borrow().contains_key(&old_id);
            if old_id != id
                && !on_screen
                && let Some(old) = self.object(old_id)
            {
                old.set_thumbnail(None);
            }
        }
    }

    /// Fills the disk cache in the background so later scrolling never waits on a decoder.
    fn warm_disk_cache(&self, files: Vec<MediaFile>) {
        for file in files {
            let disk = self.disk.clone();
            self.pool
                .submit(Priority::Background, CancelToken::new(), move || {
                    if !disk.contains(&file)
                        && let Ok(img) = decode::guarded(|| {
                            decode::decode_for_display(&file, THUMB_EDGE, Quality::Fast)
                        })
                    {
                        let _ = disk.store(&file, &img);
                    }
                });
        }
    }

    // ---- filtering -----------------------------------------------------------

    /// Changing a filter also releases pinned items.
    pub fn set_filter(&self, f: impl FnOnce(&mut FilterState)) {
        {
            let mut fs = self.filter_state.borrow_mut();
            f(&mut fs);
            fs.pinned.clear();
        }
        self.refilter();
    }

    pub fn set_sort(&self, sort: SortState) {
        self.sort_state.set(sort);
        self.resort();
    }

    /// Called when the selection changes: re-rated items may now leave the view.
    pub fn unpin(self: &Rc<Self>) {
        let had_pins = {
            let mut fs = self.filter_state.borrow_mut();
            let had = !fs.pinned.is_empty();
            fs.pinned.clear();
            had
        };
        if had_pins {
            let s = self.clone();
            glib::idle_add_local_once(move || s.refilter());
        }
    }

    /// Selects every photo the grid shows (hidden ones, e.g. filtered-out rejects, stay
    /// unselected so a following move or organize never touches what you can't see).
    /// Handled at window level: GTK's own Ctrl+A only works while the grid has focus.
    pub fn select_all(&self) {
        self.selection.select_all();
    }

    /// Fills the folder panel with the open folder's subfolders and shows or hides it, per
    /// the "Show folders" box.
    pub fn refresh_folder_panel(&self) {
        let w = self.widgets();
        w.folder_panel.set_folders(&self.subfolders.borrow());
        w.folder_split
            .set_show_sidebar(self.config.borrow().show_folders && self.folder().is_some());
    }

    /// Opens the parent of the open folder (path bar ↑, Alt+↑).
    pub fn open_parent(self: &Rc<Self>) {
        if let Some(parent) = self.folder().and_then(|f| f.parent().map(PathBuf::from)) {
            self.open_folder(parent);
        }
    }

    /// The "Show folders" box changed: remember it and show or hide the panel.
    pub fn set_show_folders(self: &Rc<Self>, on: bool) {
        if self.config.borrow().show_folders == on {
            return;
        }
        self.config.borrow_mut().show_folders = on;
        self.schedule_config_save();
        self.refresh_folder_panel();
    }

    /// Remembers (or forgets) the favourite "Open With" program and relabels its menu entry.
    pub fn set_open_with_app(self: &Rc<Self>, app_id: Option<String>) {
        self.config.borrow_mut().open_with_app = app_id;
        self.schedule_config_save();
        self.refresh_open_menu();
    }

    /// Shows the favourite program's name in the "Open in …" menu entry.
    pub fn refresh_open_menu(&self) {
        let menu = &self.widgets().file_menu;
        menu.remove(OPEN_FAVOURITE_MENU_POS);
        menu.insert(
            OPEN_FAVOURITE_MENU_POS,
            Some(&super::openwith::favourite_label(self)),
            Some("win.open-favourite"),
        );
    }

    pub fn focus_filters(&self) {
        if let Some(first) = self.widgets().filters.first_child() {
            first.grab_focus();
        }
    }

    pub fn zoom_thumbs(self: &Rc<Self>, step: i32) {
        use super::style::{THUMB_SIZE_MAX, THUMB_SIZE_MIN, THUMB_SIZE_STEP, set_thumb_size};
        let mut config = self.config.borrow_mut();
        let size = (config.thumb_display_size + step * THUMB_SIZE_STEP)
            .clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX);
        if size != config.thumb_display_size {
            config.thumb_display_size = size;
            set_thumb_size(size);
            drop(config);
            self.schedule_config_save();
        }
    }

    /// Debounces the config-file write: a burst of zoom steps (e.g. touchpad scroll) saves
    /// once, ~500 ms after the last change, instead of once per step.
    fn schedule_config_save(self: &Rc<Self>) {
        if let Some(id) = self.config_save_source.borrow_mut().take() {
            id.remove();
        }
        let s = self.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(500), move || {
            s.config_save_source.borrow_mut().take();
            let _ = s.config.borrow().save_to(&Config::default_path());
        });
        *self.config_save_source.borrow_mut() = Some(id);
    }

    // ---- viewer --------------------------------------------------------------

    pub fn viewer_active(&self) -> bool {
        self.widgets().stack.visible_child_name().as_deref() == Some("viewer")
    }

    /// Position of the viewed item while the viewer is open (falling back to the first
    /// selected item if it left the model); otherwise the first selected item.
    pub fn current_position(&self) -> Option<u32> {
        if self.viewer_active()
            && let Some(id) = self.widgets().viewer.current_id()
            && let Some(pos) = self.visible_position(id)
        {
            return Some(pos);
        }
        let set = self.selection.selection();
        (!set.is_empty()).then(|| set.minimum())
    }

    /// Position of `id` in the visible (filtered, sorted) list.
    fn visible_position(&self, id: ItemId) -> Option<u32> {
        (0..self.selection.n_items()).find(|&i| {
            self.selection
                .item(i)
                .and_downcast::<ItemObject>()
                .is_some_and(|o| o.id() == id)
        })
    }

    /// Called on every change to the visible list. Coalesces a burst of changes (a batch
    /// move, a rescan) into one `sync_viewer` run once GTK has finished updating the model.
    pub fn queue_viewer_sync(self: &Rc<Self>) {
        if self.viewer_sync_queued.replace(true) {
            return;
        }
        let s = self.clone();
        glib::idle_add_local_once(move || {
            s.viewer_sync_queued.set(false);
            s.sync_viewer();
        });
    }

    /// Keeps the viewer on an item that is still in the visible list. When its item was
    /// moved, removed by a rescan or hidden by a filter, it shows the item now at that
    /// position (the next one), or closes when the list is empty.
    fn sync_viewer(self: &Rc<Self>) {
        if !self.viewer_active() {
            return;
        }
        let visible = self
            .widgets()
            .viewer
            .current_id()
            .is_some_and(|id| self.visible_position(id).is_some());
        match viewer_fallback(visible, self.viewer_pos.get(), self.selection.n_items()) {
            ViewerFallback::Keep => {}
            ViewerFallback::Show(pos) => self.show_at(pos),
            ViewerFallback::Close => self.close_viewer(),
        }
    }

    pub fn open_viewer(self: &Rc<Self>, pos: u32) {
        self.widgets().stack.set_visible_child_name("viewer");
        self.show_at(pos);
    }

    pub fn close_viewer(&self) {
        let w = self.widgets();
        let pos = self.current_position(); // while still active: the viewed item
        w.stack.set_visible_child_name("grid");
        if let Some(pos) = pos {
            w.grid.scroll_to(
                pos,
                gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                None,
            );
        }
    }

    pub fn show_at(self: &Rc<Self>, pos: u32) {
        let Some(obj) = self.selection.item(pos).and_downcast::<ItemObject>() else {
            return;
        };
        let viewer = self.widgets().viewer.clone();
        let text = self.info_for(&obj);
        // Set before selecting, so `follow_selection` sees the selection already matching.
        viewer.set_current(&obj, &text);
        self.viewer_pos.set(Some(pos));
        self.selection.select_item(pos, true);
        viewer.scroll_strip_to(pos);

        // Drop queued decodes for items that left the current ±2 window.
        let n = self.selection.n_items() as i64;
        let window: Vec<ItemId> = (pos as i64 - 2..=pos as i64 + 2)
            .filter(|p| (0..n).contains(p))
            .filter_map(|p| self.selection.item(p as u32).and_downcast::<ItemObject>())
            .map(|o| o.id())
            .collect();
        self.pending_full
            .borrow_mut()
            .retain(|(id, _), (_, token)| {
                let keep = window.contains(id);
                if !keep {
                    token.cancel();
                }
                keep
            });

        let cached = self.cached_full(obj.id(), Zoom::Fit);
        match cached {
            Some(texture) => viewer.set_texture(&texture),
            None => {
                if let Some(thumb) = obj.thumbnail() {
                    viewer.set_texture(&thumb); // instant low-res placeholder
                }
                self.request_full(&obj, Zoom::Fit, Priority::Current);
            }
        }
        self.request_thumb(&obj, Priority::Current);
        if viewer.info_visible() {
            self.request_exif(&obj);
        }
        for delta in [1i64, -1, 2, -2] {
            let p = pos as i64 + delta;
            if (0..n).contains(&p)
                && let Some(o) = self.selection.item(p as u32).and_downcast::<ItemObject>()
            {
                self.request_full(&o, Zoom::Fit, Priority::Prefetch);
            }
        }
    }

    /// A click in the filmstrip changes the shared selection: show the newly selected item.
    pub fn follow_selection(self: &Rc<Self>) {
        if !self.viewer_active() {
            return;
        }
        let current = self.widgets().viewer.current_id();
        let set = self.selection.selection();
        let target = (0..set.size()).map(|i| set.nth(i as u32)).find(|&pos| {
            self.selection
                .item(pos)
                .and_downcast::<ItemObject>()
                .is_some_and(|o| Some(o.id()) != current)
        });
        if let Some(pos) = target {
            self.show_at(pos);
        }
    }

    pub fn step(self: &Rc<Self>, delta: i64) {
        let Some(cur) = self.current_position() else {
            return;
        };
        let n = self.selection.n_items() as i64;
        if n == 0 {
            return;
        }
        let next = (cur as i64 + delta).clamp(0, n - 1);
        if next != cur as i64 {
            self.show_at(next as u32);
        }
    }

    pub fn toggle_zoom(self: &Rc<Self>) {
        let viewer = self.widgets().viewer.clone();
        let Some(obj) = viewer.current() else { return };
        let zoom = match viewer.zoom() {
            Zoom::Fit => Zoom::Actual,
            Zoom::Actual => Zoom::Fit,
        };
        viewer.set_zoom(zoom);
        let cached = self.cached_full(obj.id(), zoom);
        match cached {
            Some(texture) => viewer.set_texture(&texture),
            None => self.request_full(&obj, zoom, Priority::Current),
        }
    }

    pub fn toggle_info(self: &Rc<Self>) {
        let viewer = self.widgets().viewer.clone();
        if viewer.toggle_info()
            && let Some(obj) = viewer.current()
        {
            self.request_exif(&obj);
        }
    }

    fn info_for(&self, obj: &ItemObject) -> String {
        let exif = self.exif_cache.borrow().get(&obj.id()).cloned();
        obj.with_item(|item| info_text(item, exif.as_ref()))
    }

    fn refresh_info(&self) {
        let viewer = &self.widgets().viewer;
        if let Some(obj) = viewer.current() {
            viewer.set_info(&self.info_for(&obj));
        }
    }

    fn cache_for(&self, zoom: Zoom) -> &RefCell<LruCache<ItemId, gdk::Texture>> {
        match zoom {
            Zoom::Fit => &self.full_cache,
            Zoom::Actual => &self.actual_cache,
        }
    }

    /// Cached texture for `id` at `zoom`, marked most recently used.
    fn cached_full(&self, id: ItemId, zoom: Zoom) -> Option<gdk::Texture> {
        self.cache_for(zoom).borrow_mut().get(&id).cloned()
    }

    pub fn request_full(&self, obj: &ItemObject, zoom: Zoom, priority: Priority) {
        let key = (obj.id(), zoom);
        if self.cache_for(zoom).borrow().contains(&obj.id()) {
            return;
        }
        if let Some((queued_at, token)) = self.pending_full.borrow().get(&key) {
            if *queued_at >= priority {
                return;
            }
            // Queued too low (e.g. a prefetch that is now the current image): requeue higher.
            token.cancel();
        }
        let token = CancelToken::new();
        self.pending_full
            .borrow_mut()
            .insert(key, (priority, token.clone()));
        let item = obj.item();
        let edge = self.screen_edge();
        let (tx, generation) = (self.tx.clone(), self.generation.get());
        self.pool.submit(priority, token, move || {
            let result = decode::guarded(|| match zoom {
                Zoom::Fit => decode::decode_for_display(item.display_file(), edge, Quality::High),
                Zoom::Actual => decode::decode_actual_size(&item),
            })
            .map_err(|e| e.to_string());
            let _ = tx.send_blocking(Msg::Full {
                id: item.id,
                generation,
                zoom,
                result,
            });
        });
    }

    fn on_full(&self, id: ItemId, generation: u64, zoom: Zoom, result: Result<Rgba, String>) {
        // The same decode may still be queued (after a requeue): one result is enough.
        let pending = self.pending_full.borrow_mut().remove(&(id, zoom));
        let Some((_, token)) = pending else {
            // Its request was withdrawn (the file changed on disk, or the item left the
            // prefetch window) while the decode was already running: don't cache it.
            return;
        };
        token.cancel();
        if generation != self.generation.get() {
            return;
        }
        let viewer = self.widgets().viewer.clone();
        let is_current = viewer.current_id() == Some(id);
        match result {
            Ok(img) => {
                let texture = texture_from(img);
                self.cache_for(zoom).borrow_mut().push(id, texture.clone());
                if is_current && viewer.zoom() == zoom {
                    viewer.set_texture(&texture);
                }
            }
            Err(e) if is_current => self.toast(&format!("Couldn't open image: {e}")),
            Err(_) => {}
        }
    }

    /// Drops everything derived from an item's files: full-size textures, decodes in flight
    /// and EXIF. Used when a rescan finds the files changed on disk.
    fn forget_full_size(&self, id: ItemId) {
        self.full_cache.borrow_mut().pop(&id);
        self.actual_cache.borrow_mut().pop(&id);
        self.exif_cache.borrow_mut().remove(&id);
        self.pending_full
            .borrow_mut()
            .retain(|(pending_id, _), (_, token)| {
                let keep = *pending_id != id;
                if !keep {
                    token.cancel();
                }
                keep
            });
    }

    /// Brings the viewer up to date with a rescanned version of the item it shows: always
    /// its rating and info; if its files changed, a fresh decode at the current zoom (the old
    /// image stays until it arrives) and fresh EXIF.
    fn refresh_viewed(&self, obj: &ItemObject, changed_on_disk: bool) {
        let viewer = self.widgets().viewer.clone();
        if changed_on_disk {
            self.request_full(obj, viewer.zoom(), Priority::Current);
            if viewer.info_visible() {
                self.request_exif(obj);
            }
        }
        self.refresh_info();
    }

    fn request_exif(&self, obj: &ItemObject) {
        if self.exif_cache.borrow().contains_key(&obj.id()) {
            return;
        }
        let (id, path) =
            obj.with_item(|i| (i.id, i.raw_file().unwrap_or(i.display_file()).path.clone()));
        let tx = self.tx.clone();
        self.pool
            .submit(Priority::Visible, CancelToken::new(), move || {
                let _ = tx.send_blocking(Msg::Exif {
                    id,
                    info: exif::read_info(&path),
                });
            });
    }

    fn on_exif(&self, id: ItemId, info: ExifInfo) {
        self.exif_cache.borrow_mut().insert(id, info);
        if self.widgets().viewer.current_id() == Some(id) {
            self.refresh_info();
        }
    }

    /// Long edge of the monitor in device pixels (full view decode size).
    fn screen_edge(&self) -> u32 {
        let window = &self.widgets().window;
        let display = gtk::prelude::WidgetExt::display(window);
        window
            .surface()
            .and_then(|surface| display.monitor_at_surface(&surface))
            .map(|m| {
                let g = m.geometry();
                (g.width().max(g.height()) * m.scale_factor()) as u32
            })
            .unwrap_or(2560)
    }

    // ---- rating --------------------------------------------------------------

    pub fn selected_objects(&self) -> Vec<ItemObject> {
        let set = self.selection.selection();
        (0..set.size())
            .filter_map(|i| self.selection.item(set.nth(i as u32)))
            .filter_map(|o| o.downcast::<ItemObject>().ok())
            .collect()
    }

    /// What a rating/move/copy shortcut applies to.
    pub fn rating_targets(&self) -> Vec<ItemObject> {
        if self.viewer_active() {
            return self.widgets().viewer.current().into_iter().collect();
        }
        self.selected_objects()
    }

    pub fn rate(self: &Rc<Self>, change: RatingChange) {
        // `objects` check: a stale object (e.g. one the viewer still holds after it left
        // the folder) must never write a rating to its vacated path.
        let targets: Vec<ItemObject> = self
            .rating_targets()
            .into_iter()
            .filter(|o| !self.is_transferring(o) && self.object(o.id()).is_some())
            .collect();
        if targets.is_empty() {
            return;
        }
        self.filter_state
            .borrow_mut()
            .pinned
            .extend(targets.iter().map(ItemObject::id));
        for obj in &targets {
            obj.update_item(|item| rating::apply(item, change));
            let item = obj.item();
            for file in &item.files {
                if let Err(e) = self
                    .index
                    .borrow()
                    .set_rating(file, item.rating, item.rejected)
                {
                    eprintln!("index: {e}");
                }
                self.write_xmp(file.path.clone(), item.rating, item.rejected);
            }
        }
        if self.viewer_active() {
            self.refresh_info();
            if self.config.borrow().auto_advance {
                self.step(1);
            }
        }
    }

    fn write_xmp(&self, path: PathBuf, rating: u8, rejected: bool) {
        // Shutting down: the index row stays dirty and the next launch writes it.
        if self.xmp_tx.borrow().is_none() {
            return;
        }
        *self
            .xmp_inflight
            .borrow_mut()
            .entry(path.clone())
            .or_insert(0) += 1;
        let job = XmpJob {
            path,
            rating,
            rejected,
        };
        let Err(job) = self.send_xmp_job(job) else {
            return;
        };
        // The writer thread is gone. Start a new one rather than silently dropping this and
        // every later sidecar write.
        eprintln!("XMP writer thread stopped; restarting it");
        let (tx, handle) = spawn_xmp_writer(self.tx.clone());
        *self.xmp_tx.borrow_mut() = Some(tx);
        *self.xmp_writer.borrow_mut() = Some(handle);
        let Err(job) = self.send_xmp_job(job) else {
            return;
        };
        // Still no writer: undo the pending count (or moves of this photo would wait for
        // it forever), keep the row dirty for a later retry, and say so.
        {
            let mut inflight = self.xmp_inflight.borrow_mut();
            if let Some(n) = inflight.get_mut(&job.path) {
                *n -= 1;
                if *n == 0 {
                    inflight.remove(&job.path);
                }
            }
        }
        let _ = self.index.borrow().mark_xmp_dirty(&job.path);
        self.toast(&format!(
            "Couldn't write XMP for {}",
            job.path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }

    /// Queues a sidecar write; gives the job back if there is no writer to take it.
    fn send_xmp_job(&self, job: XmpJob) -> Result<(), XmpJob> {
        match self.xmp_tx.borrow().as_ref() {
            Some(tx) => tx.send(job).map_err(|e| e.0),
            None => Err(job),
        }
    }

    fn on_xmp_written(self: &Rc<Self>, path: PathBuf, result: Result<i64, String>) {
        let failed = apply_xmp_result(
            &self.index.borrow(),
            &mut self.xmp_inflight.borrow_mut(),
            &path,
            result,
        );
        if failed {
            self.report_xmp_failure();
        }
    }

    /// Collects failures for half a second into one toast.
    fn report_xmp_failure(self: &Rc<Self>) {
        let before = self.xmp_failures.get();
        self.xmp_failures.set(before + 1);
        if before == 0 {
            let s = self.clone();
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                let n = s.xmp_failures.replace(0);
                s.toast(&format!(
                    "Couldn't write XMP for {n} file{}",
                    if n == 1 { "" } else { "s" }
                ));
            });
        }
    }

    /// Retries sidecar writes that failed or never ran (e.g. the app was closed).
    fn flush_dirty_xmp(&self) {
        let Some(folder) = self.folder() else { return };
        let dirty = self
            .index
            .borrow()
            .dirty_records(&folder)
            .unwrap_or_default();
        // A write racing a transfer could recreate a sidecar at a vacated path; the
        // transfer's own completion re-writes dirty rows at their destination instead.
        let moving = self.transferring_paths();
        for rec in dirty {
            let in_flight = self.xmp_inflight.borrow().contains_key(&rec.path);
            if !in_flight && !moving.contains(&rec.path) {
                self.write_xmp(rec.path, rec.rating, rec.rejected);
            }
        }
    }

    // ---- move / copy -----------------------------------------------------

    pub fn move_to_last(self: &Rc<Self>) {
        let last = self
            .index
            .borrow()
            .recent_destinations()
            .unwrap_or_default()
            .into_iter()
            .find(|d| d.is_dir() && Some(d) != self.folder().as_ref());
        match last {
            Some(dest) => {
                let targets = self.rating_targets();
                if targets.is_empty() {
                    self.toast("Select one or more images first");
                } else {
                    self.start_transfer(targets, dest, OpKind::Move);
                }
            }
            None => super::dialogs::choose_destination(self, OpKind::Move),
        }
    }

    pub fn start_transfer(self: &Rc<Self>, targets: Vec<ItemObject>, dest: PathBuf, kind: OpKind) {
        let Some(targets) = self.drop_transferring(targets) else {
            return;
        };
        let jobs = targets
            .iter()
            .map(|o| transfer_job(o, dest.clone()))
            .collect();
        let batch = BatchInfo {
            kind,
            target: dest,
            organize: false,
            undated: 0,
        };
        self.start_jobs(jobs, batch);
    }

    /// Moves or copies the items into dated folders under `root` (see `organize`). Items
    /// without a shot date are left where they are and counted in the messages.
    pub fn start_organize(self: &Rc<Self>, targets: Vec<ItemObject>, root: PathBuf, kind: OpKind) {
        let Some(targets) = self.drop_transferring(targets) else {
            return;
        };
        let dates = self.shot_dates(&targets);
        let plan = organize::plan(&dates, &root, OrganizePattern::default());
        let jobs: Vec<TransferJob> = targets
            .iter()
            .zip(plan.destinations)
            .filter_map(|(o, dest)| Some(transfer_job(o, dest?)))
            .collect();
        if jobs.is_empty() {
            self.toast("None of the selected photos has a shot date");
            return;
        }
        let batch = BatchInfo {
            kind,
            target: root,
            organize: true,
            undated: plan.undated,
        };
        self.start_jobs(jobs, batch);
    }

    /// Each item's shot date (see `organize::shot_date`), from the index.
    pub fn shot_dates(&self, targets: &[ItemObject]) -> Vec<Option<chrono::NaiveDate>> {
        let index = self.index.borrow();
        targets
            .iter()
            .map(|o| {
                o.with_item(|i| {
                    organize::shot_date(i.files.iter().map(|f| {
                        index
                            .get(&f.path)
                            .ok()
                            .flatten()
                            .and_then(|r| r.capture_time)
                    }))
                })
            })
            .collect()
    }

    /// The targets not already in a running move/copy; `None` (after telling the user, if
    /// that was the reason) when nothing is left.
    fn drop_transferring(&self, targets: Vec<ItemObject>) -> Option<Vec<ItemObject>> {
        let total = targets.len();
        let kept: Vec<ItemObject> = targets
            .into_iter()
            .filter(|o| !self.is_transferring(o))
            .collect();
        if kept.is_empty() {
            if total > 0 {
                self.toast("Already being moved");
            }
            return None;
        }
        Some(kept)
    }

    /// Asks how to handle name clashes (if any), then runs the batch.
    fn start_jobs(self: &Rc<Self>, jobs: Vec<TransferJob>, batch: BatchInfo) {
        let conflicts = jobs
            .iter()
            .filter(|j| fileops::has_conflict(&j.sources, &j.dest))
            .count();
        if conflicts == 0 {
            self.accept_batch(jobs, batch, ConflictPolicy::Skip);
            return;
        }
        let place = if batch.organize {
            format!("their dated folders under “{}”", batch.target.display())
        } else {
            format!("“{}”", batch.target.display())
        };
        let body = format!(
            "{conflicts} of {} item{} already exist in {place}.",
            jobs.len(),
            if jobs.len() == 1 { "" } else { "s" },
        );
        let dialog = adw::AlertDialog::new(Some("Files Already Exist"), Some(&body));
        dialog.add_responses(&[
            ("cancel", "_Cancel"),
            ("skip", "_Skip Existing"),
            ("rename", "_Keep Both"),
        ]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        let s = self.clone();
        dialog.connect_response(None, move |_, response| {
            let policy = match response {
                "skip" => ConflictPolicy::Skip,
                "rename" => ConflictPolicy::Rename,
                _ => return,
            };
            s.accept_batch(jobs.clone(), batch.clone(), policy);
        });
        dialog.present(Some(&self.widgets().window));
    }

    /// Marks the batch's ids as `transferring` (so a second shortcut press or a rating
    /// change on the same items is rejected/skipped instead of racing the transfer), then
    /// waits for any of their paths still queued for an XMP write before actually running.
    fn accept_batch(
        self: &Rc<Self>,
        jobs: Vec<TransferJob>,
        batch: BatchInfo,
        policy: ConflictPolicy,
    ) {
        {
            let mut transferring = self.transferring.borrow_mut();
            for job in &jobs {
                transferring.add(job.id, job.sources.clone());
            }
        }
        self.batches_active.set(self.batches_active.get() + 1);
        self.transfer_items_left
            .set(self.transfer_items_left.get() + jobs.len());
        self.wait_then_run(jobs, batch, policy);
    }

    /// Runs the batch immediately if none of its source paths are queued for an XMP write;
    /// otherwise re-checks on a short timer until they clear. This closes the race where a
    /// rating just applied hasn't been written to the sidecar yet: moving/copying before
    /// the write lands can either leave the rating out of the transferred sidecar or let
    /// the writer recreate an orphaned sidecar at the old (now-vacated) path.
    fn wait_then_run(
        self: &Rc<Self>,
        jobs: Vec<TransferJob>,
        batch: BatchInfo,
        policy: ConflictPolicy,
    ) {
        if batch_ready(&jobs, &self.xmp_inflight.borrow()) {
            self.run_transfer(jobs, batch, policy);
            return;
        }
        let s = self.clone();
        let pending = RefCell::new(Some(PendingBatch {
            jobs,
            batch,
            policy,
        }));
        glib::timeout_add_local(Duration::from_millis(50), move || {
            let ready = pending
                .borrow()
                .as_ref()
                .is_some_and(|b| batch_ready(&b.jobs, &s.xmp_inflight.borrow()));
            if !ready {
                return glib::ControlFlow::Continue;
            }
            if let Some(b) = pending.borrow_mut().take() {
                s.run_transfer(b.jobs, b.batch, b.policy);
            }
            glib::ControlFlow::Break
        });
    }

    fn run_transfer(
        self: &Rc<Self>,
        jobs: Vec<TransferJob>,
        batch: BatchInfo,
        policy: ConflictPolicy,
    ) {
        // Organizing spreads items over dated folders: those are not "recent destinations".
        if !batch.organize {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let _ = self
                .index
                .borrow()
                .add_recent_destination(&batch.target, now);
        }
        self.toast(&batch.started_text(jobs.len()));
        let kind = batch.kind;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("pb-fileops".into())
            .spawn(move || {
                let (mut done, mut skipped, mut failed) = (0, 0, 0);
                let mut first_failure: Option<String> = None;
                let mut rollback_failures: Vec<String> = Vec::new();
                for job in jobs {
                    // A panic must still end the batch with `TransfersFinished`: closing
                    // the window waits for it.
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if batch.organize {
                            fileops::transfer_item_creating_dest(
                                &job.sources,
                                &job.dest,
                                kind,
                                policy,
                            )
                        } else {
                            fileops::transfer_item(&job.sources, &job.dest, kind, policy)
                        }
                    }))
                    .unwrap_or_else(|_| {
                        TransferOutcome::Failed("internal error during transfer".into())
                    });
                    match &outcome {
                        TransferOutcome::Done(_) => done += 1,
                        TransferOutcome::Skipped => skipped += 1,
                        TransferOutcome::Failed(e) => {
                            eprintln!("transfer failed: {e}");
                            failed += 1;
                            if first_failure.is_none() {
                                first_failure = Some(e.clone());
                            }
                            if e.contains(ROLLBACK_INCOMPLETE) {
                                rollback_failures.push(e.clone());
                            }
                        }
                    }
                    let _ = tx.send_blocking(Msg::Transferred {
                        id: job.id,
                        kind,
                        outcome,
                    });
                }
                let _ = tx.send_blocking(Msg::TransfersFinished {
                    batch,
                    done,
                    skipped,
                    failed,
                    first_failure,
                    rollback_failures,
                });
            })
            .expect("failed to spawn file operation thread");
    }

    fn on_transferred(&self, id: ItemId, kind: OpKind, outcome: TransferOutcome) {
        // Every outcome (Done, Skipped, Failed) ends this item's part of the batch.
        self.transferring.borrow_mut().finish(id);
        self.transfer_items_left
            .set(self.transfer_items_left.get().saturating_sub(1));
        let TransferOutcome::Done(pairs) = outcome else {
            return;
        };
        let media: Vec<&(PathBuf, PathBuf)> = pairs
            .iter()
            .filter(|(src, _)| !xmp::is_sidecar(src))
            .collect();
        match kind {
            OpKind::Move => {
                {
                    let mut index = self.index.borrow_mut();
                    for (src, dst) in &media {
                        match index.rename_path(src, dst) {
                            Ok(()) => {
                                // A rating applied after this transfer's plan was built (or
                                // whose write raced the move) can leave the re-keyed row
                                // dirty: make sure the destination sidecar gets it too.
                                if let Ok(Some(rec)) = index.get(dst)
                                    && rec.xmp_dirty
                                {
                                    self.write_xmp(rec.path.clone(), rec.rating, rec.rejected);
                                }
                            }
                            Err(e) => eprintln!("index: {e}"),
                        }
                    }
                }
                self.remove_object(id);
            }
            OpKind::Copy => {
                let copies: Vec<FileRecord> = {
                    let mut index = self.index.borrow_mut();
                    let copies: Vec<FileRecord> = media
                        .iter()
                        .filter_map(|(src, dst)| {
                            let mut rec = index.get(src).ok().flatten()?;
                            rec.path = dst.clone();
                            Some(rec)
                        })
                        .collect();
                    // A row already at a destination is for a file that is gone (a copy
                    // never overwrites): replace it, keeping none of its stale rating.
                    if let Err(e) = index.replace_many(&copies) {
                        eprintln!("index: {e}");
                    }
                    copies
                };
                // As for a move: a rating not yet in the source sidecar when it was copied
                // (a failed or raced write) must still reach the destination sidecar.
                for rec in copies.into_iter().filter(|r| r.xmp_dirty) {
                    self.write_xmp(rec.path, rec.rating, rec.rejected);
                }
            }
        }
    }

    /// truncated to ~120 chars so a long "rollback incomplete: …" clause doesn't blow up the toast.
    fn truncate_message(msg: &str) -> String {
        const LIMIT: usize = 120;
        if msg.chars().count() <= LIMIT {
            return msg.to_string();
        }
        let mut truncated: String = msg.chars().take(LIMIT).collect();
        truncated.push('…');
        truncated
    }

    #[allow(clippy::too_many_arguments)]
    fn on_transfers_finished(
        self: &Rc<Self>,
        batch: &BatchInfo,
        done: usize,
        skipped: usize,
        failed: usize,
        first_failure: Option<String>,
        rollback_failures: Vec<String>,
    ) {
        self.batches_active
            .set(self.batches_active.get().saturating_sub(1));
        let mut text = batch.finished_text(done, skipped);
        if failed > 0 {
            text.push_str(&format!(", {failed} failed"));
            if let Some(msg) = first_failure {
                text.push_str(&format!(": {}", Self::truncate_message(&msg)));
            }
        }
        self.toast(&text);
        if !rollback_failures.is_empty() {
            let dialog = adw::AlertDialog::new(
                Some("Some Files Could Not Be Restored"),
                Some(&rollback_failures.join("\n\n")),
            );
            dialog.add_response("ok", "_OK");
            dialog.present(Some(&self.widgets().window));
        }
    }

    // ---- shutdown --------------------------------------------------------

    /// Window close request. Refused while a move/copy is still running (closing would
    /// kill its thread mid-item); otherwise saves pending state and lets the close proceed.
    pub fn close_requested(&self) -> glib::Propagation {
        if self.batches_active.get() > 0 {
            let n = self.transfer_items_left.get().max(1);
            self.toast(&format!(
                "Finishing {n} transfer{}…",
                if n == 1 { "" } else { "s" }
            ));
            return glib::Propagation::Stop;
        }
        self.shutdown();
        glib::Propagation::Proceed
    }

    /// Flushes the debounced config save and waits for queued sidecar writes to land.
    fn shutdown(&self) {
        let pending_save = self.config_save_source.borrow_mut().take();
        if let Some(source) = pending_save {
            source.remove();
            if let Err(e) = self.config.borrow().save_to(&Config::default_path()) {
                eprintln!("couldn't save config: {e}");
            }
        }
        // Dropping the only sender ends the writer's loop once its queue is drained.
        drop(self.xmp_tx.borrow_mut().take());
        let writer = self.xmp_writer.borrow_mut().take();
        if let Some(writer) = writer
            && writer.join().is_err()
        {
            eprintln!("XMP writer thread panicked");
        }
        // The main loop stops after this, so results still queued would never mark their
        // rows synced: they would stay dirty and, on the next launch, the old rating would
        // be written over any edit made in another tool meanwhile. This runs on the main
        // thread, so the message task cannot handle any of these concurrently.
        while let Ok(msg) = self.msg_rx.try_recv() {
            if let Msg::XmpWritten { path, result } = msg {
                apply_xmp_result(
                    &self.index.borrow(),
                    &mut self.xmp_inflight.borrow_mut(),
                    &path,
                    result,
                );
            }
        }
    }
}

/// Records a finished sidecar write in the index. Only the last queued write for a path marks
/// it synced, so an earlier write cannot hide a newer pending one; a failure leaves the row
/// dirty for a retry. Returns true if the write failed.
fn apply_xmp_result(
    index: &Index,
    inflight: &mut HashMap<PathBuf, u32>,
    path: &std::path::Path,
    result: Result<i64, String>,
) -> bool {
    let last_for_path = {
        let n = inflight.entry(path.to_path_buf()).or_insert(1);
        *n -= 1;
        let last = *n == 0;
        if last {
            inflight.remove(path);
        }
        last
    };
    match result {
        Ok(mtime) => {
            if last_for_path {
                let _ = index.mark_xmp_synced(path, mtime);
            }
            false
        }
        Err(e) => {
            eprintln!("XMP write failed for {}: {e}", path.display());
            let _ = index.mark_xmp_dirty(path);
            true
        }
    }
}

/// True if any file of an item was added, removed, or changed size or mtime.
fn files_changed(old: &[MediaFile], new: &[MediaFile]) -> bool {
    old != new
}

#[derive(Debug, PartialEq, Eq)]
enum ViewerFallback {
    Keep,
    Show(u32),
    Close,
}

/// What the viewer does after the visible list changed: keep its item if still visible,
/// else show whatever now sits at its last position (clamped), or close if nothing is left.
fn viewer_fallback(current_visible: bool, last_pos: Option<u32>, n: u32) -> ViewerFallback {
    if current_visible {
        ViewerFallback::Keep
    } else if n == 0 {
        ViewerFallback::Close
    } else {
        ViewerFallback::Show(last_pos.unwrap_or(0).min(n - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs(paths: Vec<&str>) -> Vec<TransferJob> {
        vec![TransferJob {
            id: 1,
            sources: paths.into_iter().map(PathBuf::from).collect(),
            dest: PathBuf::from("/dest"),
        }]
    }

    fn rated(index: &Index, path: &str) {
        let file = MediaFile {
            path: PathBuf::from(path),
            kind: crate::media::FileKind::Jpeg,
            size: 10,
            mtime_ns: 100,
        };
        index.set_rating(&file, 4, false).unwrap();
    }

    fn row(index: &Index, path: &str) -> FileRecord {
        index.get(std::path::Path::new(path)).unwrap().unwrap()
    }

    #[test]
    fn xmp_result_marks_row_synced() {
        let index = Index::open_in_memory().unwrap();
        rated(&index, "/p/a.jpg");
        let mut inflight = HashMap::from([(PathBuf::from("/p/a.jpg"), 1)]);
        let failed = apply_xmp_result(
            &index,
            &mut inflight,
            std::path::Path::new("/p/a.jpg"),
            Ok(777),
        );
        assert!(!failed);
        let r = row(&index, "/p/a.jpg");
        assert_eq!((r.xmp_dirty, r.xmp_synced_at), (false, Some(777)));
        assert!(inflight.is_empty());
    }

    #[test]
    fn only_the_last_queued_xmp_write_marks_synced() {
        let index = Index::open_in_memory().unwrap();
        rated(&index, "/p/a.jpg");
        let path = std::path::Path::new("/p/a.jpg");
        let mut inflight = HashMap::from([(path.to_path_buf(), 2)]);
        apply_xmp_result(&index, &mut inflight, path, Ok(1));
        assert!(
            row(&index, "/p/a.jpg").xmp_dirty,
            "a newer write is still queued"
        );
        apply_xmp_result(&index, &mut inflight, path, Ok(2));
        let r = row(&index, "/p/a.jpg");
        assert_eq!((r.xmp_dirty, r.xmp_synced_at), (false, Some(2)));
    }

    #[test]
    fn failed_xmp_write_leaves_row_dirty() {
        let index = Index::open_in_memory().unwrap();
        rated(&index, "/p/a.jpg");
        let path = std::path::Path::new("/p/a.jpg");
        let mut inflight = HashMap::from([(path.to_path_buf(), 1)]);
        let failed = apply_xmp_result(&index, &mut inflight, path, Err("disk full".into()));
        assert!(failed);
        assert!(row(&index, "/p/a.jpg").xmp_dirty);
        assert!(inflight.is_empty());
    }

    fn mf(path: &str, size: u64, mtime_ns: i64) -> MediaFile {
        MediaFile {
            path: PathBuf::from(path),
            kind: crate::media::FileKind::from_path(std::path::Path::new(path)).unwrap(),
            size,
            mtime_ns,
        }
    }

    #[test]
    fn files_changed_detects_size_mtime_and_pair_members() {
        let pair = [mf("/p/a.CR3", 10, 1), mf("/p/a.JPG", 5, 1)];
        assert!(!files_changed(&pair, &pair.clone()));
        let raw_edited = [mf("/p/a.CR3", 10, 2), mf("/p/a.JPG", 5, 1)];
        assert!(files_changed(&pair, &raw_edited), "only the RAW changed");
        let resized = [mf("/p/a.CR3", 10, 1), mf("/p/a.JPG", 6, 1)];
        assert!(files_changed(&pair, &resized));
        assert!(files_changed(&pair, &pair[..1]));
    }

    #[test]
    fn viewer_keeps_a_visible_item() {
        assert_eq!(viewer_fallback(true, Some(3), 10), ViewerFallback::Keep);
        assert_eq!(viewer_fallback(true, Some(3), 0), ViewerFallback::Keep);
    }

    #[test]
    fn viewer_moves_to_the_item_now_at_its_position() {
        assert_eq!(viewer_fallback(false, Some(3), 9), ViewerFallback::Show(3));
        assert_eq!(viewer_fallback(false, None, 5), ViewerFallback::Show(0));
    }

    #[test]
    fn viewer_clamps_when_the_last_item_left() {
        assert_eq!(viewer_fallback(false, Some(9), 9), ViewerFallback::Show(8));
        assert_eq!(viewer_fallback(false, Some(40), 3), ViewerFallback::Show(2));
    }

    #[test]
    fn viewer_closes_when_the_list_is_empty() {
        assert_eq!(viewer_fallback(false, Some(0), 0), ViewerFallback::Close);
    }

    #[test]
    fn in_flight_paths_last_until_their_item_finishes() {
        let p = |s: &str| PathBuf::from(s);
        let mut t = InFlight::default();
        t.add(1, vec![p("/a/IMG_1.CR3"), p("/a/IMG_1.JPG")]);
        t.add(2, vec![p("/a/IMG_2.JPG")]);
        assert_eq!(t.paths().len(), 3);
        // A reopened folder gives the same file a new object: matching is by path.
        assert!(t.contains_any([std::path::Path::new("/a/IMG_1.JPG")].into_iter()));
        assert!(!t.contains_any([std::path::Path::new("/a/IMG_3.JPG")].into_iter()));
        t.finish(1);
        assert!(!t.contains_any([std::path::Path::new("/a/IMG_1.CR3")].into_iter()));
        assert!(t.contains_any([std::path::Path::new("/a/IMG_2.JPG")].into_iter()));
        assert_eq!(t.paths(), HashSet::from([p("/a/IMG_2.JPG")]));
    }

    #[test]
    fn finishing_an_unknown_item_is_harmless() {
        let mut t = InFlight::default();
        t.add(7, vec![PathBuf::from("/a/x.jpg")]);
        t.finish(99);
        assert_eq!(t.paths().len(), 1);
        t.finish(7);
        t.finish(7);
        assert!(t.paths().is_empty());
    }

    fn batch(kind: OpKind, organize: bool, undated: usize) -> BatchInfo {
        BatchInfo {
            kind,
            target: PathBuf::from("/srv/photos"),
            organize,
            undated,
        }
    }

    #[test]
    fn move_and_copy_messages_are_unchanged() {
        let b = batch(OpKind::Move, false, 0);
        assert_eq!(b.started_text(1), "Moving 1 item to /srv/photos…");
        assert_eq!(
            b.finished_text(3, 1),
            "Moved 3 items to /srv/photos, 1 skipped"
        );
        let c = batch(OpKind::Copy, false, 0);
        assert_eq!(c.started_text(2), "Copying 2 items to /srv/photos…");
    }

    #[test]
    fn organize_messages_name_the_root_and_undated_items() {
        let b = batch(OpKind::Move, true, 3);
        assert_eq!(
            b.started_text(39),
            "Moving 39 items into dated folders under /srv/photos…"
        );
        assert_eq!(
            b.finished_text(39, 0),
            "Moved 39 items into dated folders under /srv/photos, 3 without a shot date"
        );
        let c = batch(OpKind::Copy, true, 0);
        assert_eq!(
            c.finished_text(1, 2),
            "Copied 1 item into dated folders under /srv/photos, 2 skipped"
        );
    }

    fn job(path: &str, rating: u8) -> XmpJob {
        XmpJob {
            path: PathBuf::from(path),
            rating,
            rejected: false,
        }
    }

    fn next_result(rx: &async_channel::Receiver<Msg>) -> (PathBuf, Result<i64, String>) {
        match rx.recv_blocking().expect("writer reports every job") {
            Msg::XmpWritten { path, result } => (path, result),
            _ => panic!("unexpected message"),
        }
    }

    #[test]
    fn xmp_writer_survives_a_panicking_write() {
        let (tx, rx) = async_channel::unbounded();
        let (jobs, handle) = spawn_xmp_writer_with(tx, |job| {
            if job.rating == 9 {
                panic!("exiv2 blew up");
            }
            Ok(i64::from(job.rating))
        });
        jobs.send(job("/p/bad.jpg", 9)).unwrap();
        jobs.send(job("/p/good.jpg", 3)).unwrap();
        let (path, result) = next_result(&rx);
        assert_eq!(path, PathBuf::from("/p/bad.jpg"));
        assert!(result.unwrap_err().contains("internal error"));
        assert_eq!(next_result(&rx), (PathBuf::from("/p/good.jpg"), Ok(3)));
        drop(jobs);
        handle.join().expect("the writer thread did not die");
    }

    #[test]
    fn xmp_writer_reports_results_in_order() {
        let (tx, rx) = async_channel::unbounded();
        let (jobs, handle) = spawn_xmp_writer_with(tx, |job| {
            if job.rating == 0 {
                Err("disk full".into())
            } else {
                Ok(1)
            }
        });
        for (p, r) in [("/p/a.jpg", 2), ("/p/b.jpg", 0), ("/p/c.jpg", 5)] {
            jobs.send(job(p, r)).unwrap();
        }
        assert_eq!(next_result(&rx), (PathBuf::from("/p/a.jpg"), Ok(1)));
        assert_eq!(
            next_result(&rx),
            (PathBuf::from("/p/b.jpg"), Err("disk full".into()))
        );
        assert_eq!(next_result(&rx), (PathBuf::from("/p/c.jpg"), Ok(1)));
        drop(jobs);
        handle.join().unwrap();
    }

    #[test]
    fn batch_ready_waits_for_any_inflight_source_path() {
        let mut inflight = HashMap::new();
        inflight.insert(PathBuf::from("/a/IMG_1.CR3"), 1);
        let batch = jobs(vec!["/a/IMG_1.CR3", "/a/IMG_1.JPG"]);
        assert!(!batch_ready(&batch, &inflight));

        inflight.remove(&PathBuf::from("/a/IMG_1.CR3"));
        assert!(batch_ready(&batch, &inflight));
    }

    #[test]
    fn batch_ready_ignores_unrelated_inflight_paths() {
        let mut inflight = HashMap::new();
        inflight.insert(PathBuf::from("/other/IMG_9.CR3"), 1);
        let batch = jobs(vec!["/a/IMG_1.CR3", "/a/IMG_1.JPG"]);
        assert!(batch_ready(&batch, &inflight));
    }

    #[test]
    fn batch_ready_is_true_for_empty_batch() {
        assert!(batch_ready(&[], &HashMap::new()));
    }
}
