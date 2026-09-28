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

use super::item_object::ItemObject;
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
        kind: OpKind,
        dest: PathBuf,
        done: usize,
        skipped: usize,
        failed: usize,
        first_failure: Option<String>,
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
}

pub struct AppState {
    pub config: RefCell<Config>,
    pub index: RefCell<Index>,
    pub pool: JobPool,
    pub disk: Arc<DiskCache>,
    pub tx: async_channel::Sender<Msg>,
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
    full_cache: RefCell<LruCache<(ItemId, Zoom), gdk::Texture>>,
    /// In-flight full-size decodes: the priority they were queued at and their cancel token.
    pending_full: RefCell<HashMap<(ItemId, Zoom), (Priority, CancelToken)>>,
    exif_cache: RefCell<HashMap<ItemId, ExifInfo>>,
    folder: RefCell<Option<PathBuf>>,
    /// Bumped on every folder change; results from older generations are dropped.
    generation: Cell<u64>,
    next_id: Cell<ItemId>,
    first_scan_done: Cell<bool>,
    xmp_tx: mpsc::Sender<XmpJob>,
    xmp_inflight: RefCell<HashMap<PathBuf, u32>>,
    xmp_failures: Cell<u32>,
    /// Pending debounced config-file write, if any (see `zoom_thumbs`).
    config_save_source: RefCell<Option<glib::SourceId>>,
    /// Viewer position to resume at once an in-flight transfer finishes.
    viewer_resume: Cell<Option<u32>>,
    /// Ids currently accepted into a move/copy batch (spawned or waiting on XMP writes to
    /// drain): `rate()` and `start_transfer()` both skip them to avoid racing the transfer.
    transferring: RefCell<HashSet<ItemId>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    rescan_source: RefCell<Option<glib::SourceId>>,
    /// A rescan came in while one was already running: run once more when it finishes,
    /// instead of starting a second scan thread that could race the first.
    rescan_pending: Cell<bool>,
    scan_running: Cell<bool>,
}

fn capacity(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n.max(1)).unwrap()
}

/// A move/copy batch that was accepted (ids already marked `transferring`) but is still
/// waiting for its source paths to clear `xmp_inflight` before the transfer thread spawns.
struct PendingBatch {
    jobs: Vec<(ItemId, Vec<PathBuf>)>,
    dest: PathBuf,
    kind: OpKind,
    policy: ConflictPolicy,
}

/// True once none of a batch's media paths still have a queued/in-flight XMP write.
///
/// Spawning the transfer while a write is in flight can race the sidecar writer thread:
/// the transfer's plan may be built before a sidecar exists (so it isn't moved, and the
/// writer then recreates it at the old, now-orphaned path), or the writer's save can land
/// at the old path after the file itself was already renamed away. Waiting until every
/// source path is clear avoids both races.
fn batch_ready(jobs: &[(ItemId, Vec<PathBuf>)], inflight: &HashMap<PathBuf, u32>) -> bool {
    jobs.iter()
        .all(|(_, paths)| paths.iter().all(|p| !inflight.contains_key(p)))
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

/// One writer thread: sidecar writes are serialised and ordered.
fn spawn_xmp_writer(tx: async_channel::Sender<Msg>) -> mpsc::Sender<XmpJob> {
    let (job_tx, job_rx) = mpsc::channel::<XmpJob>();
    std::thread::Builder::new()
        .name("pb-xmp".into())
        .spawn(move || {
            for job in job_rx {
                let result = xmp::write_rating(&job.path, job.rating, job.rejected)
                    .map_err(|e| e.to_string());
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
    job_tx
}

impl AppState {
    pub fn new(tx: async_channel::Sender<Msg>) -> Rc<AppState> {
        let config = Config::load_from(&Config::default_path());
        let index = Index::open(&Index::default_path())
            .or_else(|e| {
                eprintln!("index unavailable ({e}); using an in-memory index");
                Index::open_in_memory()
            })
            .expect("SQLite is unusable");
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
        // +2 leaves room for 100 % decodes of the current and previous image.
        let full_cap = config.full_cache_items + 2;
        let xmp_tx = spawn_xmp_writer(tx.clone());

        Rc::new(AppState {
            config: RefCell::new(config),
            index: RefCell::new(index),
            pool: JobPool::new(JobPool::default_threads()),
            disk: Arc::new(disk),
            tx,
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
            pending_full: RefCell::default(),
            exif_cache: RefCell::default(),
            folder: RefCell::new(None),
            generation: Cell::new(0),
            next_id: Cell::new(0),
            first_scan_done: Cell::new(false),
            xmp_tx,
            xmp_inflight: RefCell::default(),
            xmp_failures: Cell::new(0),
            config_save_source: RefCell::new(None),
            viewer_resume: Cell::new(None),
            transferring: RefCell::default(),
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
                kind,
                dest,
                done,
                skipped,
                failed,
                first_failure,
            } => self.on_transfers_finished(kind, &dest, done, skipped, failed, first_failure),
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
        self.filter_state.borrow_mut().pinned.clear();
        // In-flight transfers (spawned, or still waiting on pending XMP writes) keep
        // running: they operate on disk paths, independent of which folder is open here.
        // Their ids are never reused, so it's enough to just forget them.
        self.transferring.borrow_mut().clear();
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
        let scan = match result {
            Ok(scan) => scan,
            Err(e) => {
                self.folder_lost(&format!("Couldn't read the folder: {e}"));
                return;
            }
        };
        if self.rescan_pending.replace(false) {
            self.rescan();
        }
        {
            let mut index = self.index.borrow_mut();
            if let Err(e) = index.upsert_many(&scan.upserts) {
                eprintln!("index update failed: {e}");
            }
            if let Err(e) = index.remove_paths(&scan.removed) {
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
        for update in diff.updated {
            let Some(obj) = self.object(update.item.id) else {
                continue;
            };
            let mut item = update.item;
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
            && let Some(pos) = (0..self.selection.n_items()).find(|&i| {
                self.selection
                    .item(i)
                    .and_downcast::<ItemObject>()
                    .is_some_and(|o| o.id() == id)
            })
        {
            return Some(pos);
        }
        let set = self.selection.selection();
        (!set.is_empty()).then(|| set.minimum())
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
        self.selection.select_item(pos, true);

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

        let cached = self
            .full_cache
            .borrow_mut()
            .get(&(obj.id(), Zoom::Fit))
            .cloned();
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
        let cached = self.full_cache.borrow_mut().get(&(obj.id(), zoom)).cloned();
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

    pub fn request_full(&self, obj: &ItemObject, zoom: Zoom, priority: Priority) {
        let key = (obj.id(), zoom);
        if self.full_cache.borrow().contains(&key) {
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
        if let Some((_, token)) = pending {
            token.cancel();
        }
        if generation != self.generation.get() {
            return;
        }
        let viewer = self.widgets().viewer.clone();
        let is_current = viewer.current_id() == Some(id);
        match result {
            Ok(img) => {
                let texture = texture_from(img);
                self.full_cache
                    .borrow_mut()
                    .push((id, zoom), texture.clone());
                if is_current && viewer.zoom() == zoom {
                    viewer.set_texture(&texture);
                }
            }
            Err(e) if is_current => self.toast(&format!("Couldn't open image: {e}")),
            Err(_) => {}
        }
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
        let targets: Vec<ItemObject> = {
            let transferring = self.transferring.borrow();
            self.rating_targets()
                .into_iter()
                .filter(|o| !transferring.contains(&o.id()))
                .collect()
        };
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
        *self
            .xmp_inflight
            .borrow_mut()
            .entry(path.clone())
            .or_insert(0) += 1;
        let _ = self.xmp_tx.send(XmpJob {
            path,
            rating,
            rejected,
        });
    }

    fn on_xmp_written(self: &Rc<Self>, path: PathBuf, result: Result<i64, String>) {
        let last_for_path = {
            let mut inflight = self.xmp_inflight.borrow_mut();
            let n = inflight.entry(path.clone()).or_insert(1);
            *n -= 1;
            let last = *n == 0;
            if last {
                inflight.remove(&path);
            }
            last
        };
        match result {
            Ok(mtime) if last_for_path => {
                let _ = self.index.borrow().mark_xmp_synced(&path, mtime);
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("XMP write failed for {}: {e}", path.display());
                let _ = self.index.borrow().mark_xmp_dirty(&path);
                self.report_xmp_failure();
            }
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
        for rec in dirty {
            let in_flight = self.xmp_inflight.borrow().contains_key(&rec.path);
            if !in_flight {
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
        let (targets, dropped_transferring) = {
            let transferring = self.transferring.borrow();
            let mut kept = Vec::with_capacity(targets.len());
            let mut dropped = false;
            for o in targets {
                if transferring.contains(&o.id()) {
                    dropped = true;
                } else {
                    kept.push(o);
                }
            }
            (kept, dropped)
        };
        if targets.is_empty() {
            if dropped_transferring {
                self.toast("Already being moved");
            }
            return;
        }
        let jobs: Vec<(ItemId, Vec<PathBuf>)> = targets
            .iter()
            .map(|o| o.with_item(|i| (i.id, i.files.iter().map(|f| f.path.clone()).collect())))
            .collect();
        let conflicts = jobs
            .iter()
            .filter(|(_, src)| fileops::has_conflict(src, &dest))
            .count();
        if conflicts == 0 {
            self.accept_batch(jobs, dest, kind, ConflictPolicy::Skip);
            return;
        }
        let body = format!(
            "{conflicts} of {} item{} already exist in “{}”.",
            jobs.len(),
            if jobs.len() == 1 { "" } else { "s" },
            dest.display()
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
            s.accept_batch(jobs.clone(), dest.clone(), kind, policy);
        });
        dialog.present(Some(&self.widgets().window));
    }

    /// Marks the batch's ids as `transferring` (so a second shortcut press or a rating
    /// change on the same items is rejected/skipped instead of racing the transfer), then
    /// waits for any of their paths still queued for an XMP write before actually running.
    fn accept_batch(
        self: &Rc<Self>,
        jobs: Vec<(ItemId, Vec<PathBuf>)>,
        dest: PathBuf,
        kind: OpKind,
        policy: ConflictPolicy,
    ) {
        {
            let mut transferring = self.transferring.borrow_mut();
            for (id, _) in &jobs {
                transferring.insert(*id);
            }
        }
        self.wait_then_run(jobs, dest, kind, policy);
    }

    /// Runs the batch immediately if none of its source paths are queued for an XMP write;
    /// otherwise re-checks on a short timer until they clear. This closes the race where a
    /// rating just applied hasn't been written to the sidecar yet: moving/copying before
    /// the write lands can either leave the rating out of the transferred sidecar or let
    /// the writer recreate an orphaned sidecar at the old (now-vacated) path.
    fn wait_then_run(
        self: &Rc<Self>,
        jobs: Vec<(ItemId, Vec<PathBuf>)>,
        dest: PathBuf,
        kind: OpKind,
        policy: ConflictPolicy,
    ) {
        if batch_ready(&jobs, &self.xmp_inflight.borrow()) {
            self.run_transfer(jobs, dest, kind, policy);
            return;
        }
        let s = self.clone();
        let pending = RefCell::new(Some(PendingBatch {
            jobs,
            dest,
            kind,
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
                s.run_transfer(b.jobs, b.dest, b.kind, b.policy);
            }
            glib::ControlFlow::Break
        });
    }

    fn run_transfer(
        self: &Rc<Self>,
        jobs: Vec<(ItemId, Vec<PathBuf>)>,
        dest: PathBuf,
        kind: OpKind,
        policy: ConflictPolicy,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let _ = self.index.borrow().add_recent_destination(&dest, now);
        self.viewer_resume.set(if self.viewer_active() {
            self.current_position()
        } else {
            None
        });
        let verb = match kind {
            OpKind::Move => "Moving",
            OpKind::Copy => "Copying",
        };
        self.toast(&format!(
            "{verb} {} item{}…",
            jobs.len(),
            if jobs.len() == 1 { "" } else { "s" }
        ));
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("pb-fileops".into())
            .spawn(move || {
                let (mut done, mut skipped, mut failed) = (0, 0, 0);
                let mut first_failure: Option<String> = None;
                for (id, sources) in jobs {
                    let outcome = fileops::transfer_item(&sources, &dest, kind, policy);
                    match &outcome {
                        TransferOutcome::Done(_) => done += 1,
                        TransferOutcome::Skipped => skipped += 1,
                        TransferOutcome::Failed(e) => {
                            eprintln!("transfer failed: {e}");
                            failed += 1;
                            if first_failure.is_none() {
                                first_failure = Some(e.clone());
                            }
                        }
                    }
                    let _ = tx.send_blocking(Msg::Transferred { id, kind, outcome });
                }
                let _ = tx.send_blocking(Msg::TransfersFinished {
                    kind,
                    dest,
                    done,
                    skipped,
                    failed,
                    first_failure,
                });
            })
            .expect("failed to spawn file operation thread");
    }

    fn on_transferred(&self, id: ItemId, kind: OpKind, outcome: TransferOutcome) {
        // Every outcome (Done, Skipped, Failed) ends this item's part of the batch.
        self.transferring.borrow_mut().remove(&id);
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
                let mut index = self.index.borrow_mut();
                let copies: Vec<FileRecord> = media
                    .iter()
                    .filter_map(|(src, dst)| {
                        let mut rec = index.get(src).ok().flatten()?;
                        rec.path = dst.clone();
                        Some(rec)
                    })
                    .collect();
                if let Err(e) = index.upsert_many(&copies) {
                    eprintln!("index: {e}");
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

    fn on_transfers_finished(
        self: &Rc<Self>,
        kind: OpKind,
        dest: &std::path::Path,
        done: usize,
        skipped: usize,
        failed: usize,
        first_failure: Option<String>,
    ) {
        let verb = match kind {
            OpKind::Move => "Moved",
            OpKind::Copy => "Copied",
        };
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut text = format!(
            "{verb} {done} item{} to “{name}”",
            if done == 1 { "" } else { "s" }
        );
        if skipped > 0 {
            text.push_str(&format!(", {skipped} skipped"));
        }
        if failed > 0 {
            text.push_str(&format!(", {failed} failed"));
            if let Some(msg) = first_failure {
                text.push_str(&format!(": {}", Self::truncate_message(&msg)));
            }
        }
        self.toast(&text);

        if let Some(pos) = self.viewer_resume.take() {
            let n = self.selection.n_items();
            if n == 0 {
                self.close_viewer();
            } else {
                // `current_position()` still finds the removed object while the viewer holds
                // its id, so check membership in `objects` directly instead.
                let current_gone = self
                    .widgets()
                    .viewer
                    .current_id()
                    .is_some_and(|id| self.object(id).is_none());
                if current_gone {
                    self.show_at(pos.min(n - 1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jobs(paths: Vec<&str>) -> Vec<(ItemId, Vec<PathBuf>)> {
        vec![(1, paths.into_iter().map(PathBuf::from).collect())]
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
