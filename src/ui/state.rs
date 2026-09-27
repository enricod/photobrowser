use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use lru::LruCache;

use super::item_object::ItemObject;
use crate::catalog::diff::diff_items;
use crate::catalog::sync::{self, ScanResult};
use crate::config::Config;
use crate::decode::{self, Quality, Rgba, THUMB_EDGE};
use crate::filter::{self, FilterState, SortState};
use crate::index::{FileRecord, Index};
use crate::jobs::{CancelToken, JobPool, Priority};
use crate::media::{Item, ItemId, MediaFile};
use crate::thumbcache::DiskCache;

/// Results sent from worker threads to the GTK main thread.
pub enum Msg {
    Thumb { id: ItemId, generation: u64, result: Result<Rgba, String> },
    Scanned { generation: u64, result: Result<ScanResult, String> },
}

pub struct Widgets {
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub stack: gtk::Stack,
    pub grid: gtk::GridView,
    pub counter: gtk::Label,
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
    pending_thumbs: RefCell<HashMap<ItemId, CancelToken>>,
    thumb_lru: RefCell<LruCache<ItemId, ()>>,
    folder: RefCell<Option<PathBuf>>,
    /// Bumped on every folder change; results from older generations are dropped.
    generation: Cell<u64>,
    next_id: Cell<ItemId>,
    first_scan_done: Cell<bool>,
}

fn capacity(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n.max(1)).unwrap()
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
            thumb_lru: RefCell::new(LruCache::new(capacity(thumb_cap))),
            folder: RefCell::new(None),
            generation: Cell::new(0),
            next_id: Cell::new(0),
            first_scan_done: Cell::new(false),
        })
    }

    pub fn widgets(&self) -> &Widgets {
        self.widgets.get().expect("widgets are set during window construction")
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
            w.counter
                .set_label(&format!("{} / {}", self.selection.n_items(), self.store.n_items()));
        }
    }

    pub fn handle(self: &Rc<Self>, msg: Msg) {
        match msg {
            Msg::Thumb { id, generation, result } => self.on_thumb(id, generation, result),
            Msg::Scanned { generation, result } => self.on_scanned(generation, result),
        }
    }

    // ---- folders -------------------------------------------------------------

    pub fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Open Folder").modal(true).build();
        if let Some(folder) = self.folder() {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let s = self.clone();
        dialog.select_folder(Some(&self.widgets().window), gio::Cancellable::NONE, move |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                s.open_folder(path);
            }
        });
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
        self.widgets().window.set_title(Some(&format!("{name} — Photobrowser")));

        let known = self.index.borrow().load_folder(&folder).unwrap_or_default();
        let mut items = sync::items_from_records(&known);
        for item in &mut items {
            item.id = self.alloc_id();
        }
        self.add_objects(items);
        self.widgets().stack.set_visible_child_name("grid");
        self.start_scan(folder, known);
    }

    /// Forgets the current folder: cancels queued work, clears models and caches.
    fn reset(&self) {
        self.generation.set(self.generation.get() + 1);
        self.first_scan_done.set(false);
        self.pool.clear();
        for (_, token) in self.pending_thumbs.borrow_mut().drain() {
            token.cancel();
        }
        // Empty the model first: its cells unbind (and touch the LRU) while items still exist.
        self.store.remove_all();
        self.thumb_lru.borrow_mut().clear();
        self.objects.borrow_mut().clear();
        self.filter_state.borrow_mut().pinned.clear();
        *self.folder.borrow_mut() = None;
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
        let pending = self.pending_thumbs.borrow_mut().remove(&id);
        if let Some(token) = pending {
            token.cancel();
        }
        self.thumb_lru.borrow_mut().pop(&id);
    }

    fn start_scan(&self, folder: PathBuf, known: HashMap<PathBuf, FileRecord>) {
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

    pub fn rescan(&self) {
        let Some(folder) = self.folder() else { return };
        let known = self.index.borrow().load_folder(&folder).unwrap_or_default();
        self.start_scan(folder, known);
    }

    fn on_scanned(self: &Rc<Self>, generation: u64, result: Result<ScanResult, String>) {
        if generation != self.generation.get() {
            return;
        }
        let scan = match result {
            Ok(scan) => scan,
            Err(e) => {
                self.folder_lost(&format!("Couldn't read the folder: {e}"));
                return;
            }
        };
        {
            let mut index = self.index.borrow_mut();
            if let Err(e) = index.upsert_many(&scan.upserts) {
                eprintln!("index update failed: {e}");
            }
            if let Err(e) = index.remove_paths(&scan.removed) {
                eprintln!("index cleanup failed: {e}");
            }
        }

        let old: Vec<Item> = self.objects.borrow().values().map(ItemObject::item).collect();
        let diff = diff_items(&old, scan.items, || self.alloc_id());
        for id in diff.removed {
            self.remove_object(id);
        }
        for update in diff.updated {
            let Some(obj) = self.object(update.item.id) else { continue };
            let mut item = update.item;
            // The index is the truth for ratings: it keeps pending local edits.
            let rec = self.index.borrow().get(&item.display_file().path).ok().flatten();
            if let Some(rec) = rec {
                item.rating = rec.rating;
                item.rejected = rec.rejected;
            }
            obj.set_item(item);
            if update.display_changed {
                obj.set_thumbnail(None);
                obj.set_error(None);
                self.thumb_lru.borrow_mut().pop(&obj.id());
                if self.bound.borrow().contains_key(&obj.id()) {
                    self.request_thumb(&obj, Priority::Visible);
                }
            }
        }
        let warm: Vec<MediaFile> = if self.first_scan_done.replace(true) {
            diff.added.iter().map(|i| i.display_file().clone()).collect()
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
        let pending = self.pending_thumbs.borrow_mut().remove(&id);
        if let Some(token) = pending {
            token.cancel();
        }
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
        self.pending_thumbs.borrow_mut().insert(id, token.clone());
        let file = obj.with_item(|i| i.display_file().clone());
        let (disk, tx, generation) = (self.disk.clone(), self.tx.clone(), self.generation.get());
        self.pool.submit(priority, token, move || {
            let result = match disk.load(&file) {
                Some(img) => Ok(img),
                None => decode::guarded(|| decode::decode_for_display(&file, THUMB_EDGE, Quality::Fast))
                    .inspect(|img| {
                        let _ = disk.store(&file, img);
                    })
                    .map_err(|e| e.to_string()),
            };
            let _ = tx.send_blocking(Msg::Thumb { id, generation, result });
        });
    }

    fn on_thumb(&self, id: ItemId, generation: u64, result: Result<Rgba, String>) {
        self.pending_thumbs.borrow_mut().remove(&id);
        if generation != self.generation.get() {
            return;
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
            self.pool.submit(Priority::Background, CancelToken::new(), move || {
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
}
