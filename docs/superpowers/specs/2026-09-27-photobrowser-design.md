# Photobrowser — Design Spec

Date: 2026-09-27
Status: Approved 2026-09-27; amended by the implementation plan

## 1. Purpose

A fast, keyboard-driven GTK 4 desktop application for **culling** a folder of
photos (JPEG, PNG, TIFF, RAW) before editing in darktable / RawTherapee.

The user browses thumbnails and full-size images, assigns 1–5 star ratings and
reject flags, filters/sorts, and moves or copies the selected images to another
folder. It is not an editor.

**Success criteria**

- Browsing feels instant (see performance targets, §5.1).
- Ratings written by the app are visible in darktable (XMP sidecars).
- RAW+JPEG pairs behave as one item.
- No file is ever lost or silently overwritten by a move/copy.

**Platform:** Linux (Fedora 44 primary), single user, one folder at a time
(non-recursive).

## 2. Technology

- **Language:** Rust (stable).
- **UI:** gtk4-rs + libadwaita (GTK ≥ 4.22).
- **JPEG decoding:** libjpeg-turbo via the `turbojpeg` crate (DCT-domain scaling
  1/2, 1/4, 1/8).
- **RAW:** embedded previews via exiv2 (`rexiv2` / gexiv2); full decode via
  LibRaw through a small in-crate FFI wrapper (`libraw` module, `bindgen` or
  hand-written `extern "C"` for the handful of functions needed).
- **PNG / TIFF:** `image` crate.
- **Resizing:** `fast_image_resize` (SIMD).
- **Metadata / XMP:** gexiv2 via `rexiv2`.
- **Index:** SQLite via `rusqlite` (bundled feature off; use system sqlite).
- **Concurrency:** worker thread pool with a priority queue (`crossbeam-channel`
  + custom scheduler, or `rayon` where priorities are not needed).
- **Build:** cargo. System packages: `rust cargo gtk4-devel libadwaita-devel
  LibRaw-devel gexiv2-devel libjpeg-turbo-devel sqlite-devel`.

## 3. Features (v1)

### 3.1 Supported files

- JPEG: `.jpg .jpeg`
- PNG: `.png`
- TIFF: `.tif .tiff`
- RAW: `.cr2 .cr3 .nef .arw .raf .orf .rw2 .dng .pef .srw`
- Extension matching is case-insensitive.

### 3.2 RAW+JPEG pairing

Files in the same folder with the same base name (case-insensitive), where one
is RAW and one is JPEG, form a single **item**. The item's display image is the
JPEG (faster); the RAW is used for the full decode on demand. Ratings, reject
flags, moves and copies apply to all files of the item. A base name with
multiple RAWs or multiple JPEGs (e.g. `.jpg` and `.JPG`) is not paired; each
file is its own item.

### 3.3 Ratings and rejects

- Rating: 0 (unrated) to 5.
- Reject: a separate boolean flag. A rejected item keeps its star rating
  internally but is written to XMP as `xmp:Rating = -1` (darktable convention).
  Un-rejecting restores the previous star rating.
- Stored in the local index immediately; written to XMP asynchronously.

### 3.4 XMP sidecars

- Naming: `<filename with extension>.xmp`, e.g. `IMG_001.CR3.xmp`
  (darktable convention). For a pair, **both** files get their sidecar updated.
- Only `Xmp.xmp.Rating` is written. All other existing content is preserved
  (the sidecar is opened, modified and saved via exiv2, never regenerated).
- If no sidecar exists, a minimal valid XMP packet is created.
- On folder scan, if a sidecar's mtime is newer than the index record's
  `xmp_synced_at`, its rating is imported into the index (the sidecar wins).
- Failed writes are marked `xmp_dirty` in the index and retried on next open.
- RawTherapee `.pp3` rank is out of scope; during implementation, verify
  whether RawTherapee reads `xmp:Rating` from sidecars and document the result
  in the README.

### 3.5 Filtering and sorting

- Minimum rating: 0–5 (0 = show all).
- Reject filter: *show all* / *hide rejects* (default) / *only rejects*.
- Type filter: *All* / *RAW* (items containing a RAW) / *JPEG* (items
  containing a JPEG) / *Other* (PNG, TIFF).
- Sort: capture date (EXIF `DateTimeOriginal`, falling back to file mtime) or
  filename; ascending or descending. Ties broken by filename.
- An item re-rated so it no longer matches the filter stays visible until the
  user navigates away from it.

### 3.6 File operations

- **Move** and **Copy** selected items to a chosen folder.
- Each item's files and their sidecars are transferred as one unit.
- Destination chooser: GTK folder dialog plus a list of the 10 most recently
  used destinations (stored in the index).
- `Ctrl+Shift+M` repeats a move to the last destination without a dialog.
- Name conflicts: one dialog per batch, before anything is transferred:
  **Skip Existing** / **Keep Both** (append `_1`, `_2`, … to all files of the
  item consistently) / **Cancel**. The choice applies to every conflicting
  item in the batch. Never overwrite silently.
- Same filesystem: `rename(2)`. Different filesystem: copy → fsync → verify
  size → delete source.
- Per-item atomicity: if any file of an item fails, files of that item already
  transferred are rolled back (moves are reverted, copies deleted).
- Operations run in the background with a progress toast; the index is updated
  on completion (moved items are removed from the current view; index rows are
  re-keyed to the new path).

### 3.7 Out of scope for v1

Trash, bulk rename, color labels, EXIF-field filters, editing/rotation,
recursive folders, RawTherapee `.pp3` writing, multiple windows.

## 4. Architecture

Single binary crate `photobrowser` with modules:

| Module | Responsibility | Depends on |
|---|---|---|
| `catalog` | Scan a folder, classify files, group pairs into `Item`s, hold the item list | fs, `index` |
| `index` | SQLite store at `$XDG_CACHE_HOME/photobrowser/index.db` | rusqlite |
| `xmp` | Read/write rating in sidecars, preserving other content | rexiv2 |
| `decode` | File → RGBA pixels at a requested max size, orientation applied | turbojpeg, `libraw`, rexiv2, image, fast_image_resize |
| `libraw` | Minimal safe wrapper over LibRaw C API | LibRaw (FFI) |
| `cache` | In-memory LRU of textures + on-disk thumbnail cache | `decode` |
| `jobs` | Priority worker pool with cancellation | crossbeam |
| `fileops` | Move/copy items with conflict handling and rollback | std::fs |
| `ui` | Window, grid view, full view, filter bar, dialogs, shortcuts | gtk4, libadwaita |

Non-UI modules (`catalog`, `index`, `xmp`, `decode`, `cache`, `jobs`,
`fileops`) have no GTK dependency except `cache`'s texture layer, so they are
unit-testable without a display. `cache` is split into a GTK-free pixel cache
and a thin texture adapter.

### 4.1 Core types

```rust
enum FileKind { Jpeg, Png, Tiff, Raw }

struct MediaFile { path: PathBuf, kind: FileKind, size: u64, mtime_ns: i64 }

struct Item {
    id: ItemId,              // stable within a session
    base_name: String,
    files: Vec<MediaFile>,   // 1 or 2 (pair)
    sort_time: i64,          // capture time (unix s), else display file mtime
    sort_name: String,       // lower-cased display file name
    rating: u8,              // 0..=5
    rejected: bool,
}
```

`Item::display_file()` → JPEG if present, else the single file.
`Item::raw_file()` → RAW if present.

### 4.2 Index schema

```sql
CREATE TABLE files (
  path BLOB PRIMARY KEY,           -- raw path bytes (non-UTF-8 safe)
  folder BLOB NOT NULL,            -- parent directory, indexed
  size INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  capture_time INTEGER,          -- unix seconds, NULL if unknown
  rating INTEGER NOT NULL DEFAULT 0,
  rejected INTEGER NOT NULL DEFAULT 0,
  xmp_synced_at INTEGER,         -- mtime of sidecar at last sync
  xmp_dirty INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE recent_destinations (path BLOB PRIMARY KEY, used_at INTEGER);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);  -- schema_version
```

Rating/reject are stored per file; for a pair both rows are kept equal. A
schema version mismatch or open failure → delete and rebuild (sidecars are the
source of truth for ratings).

### 4.3 Data flow

1. **Open folder:** load index rows for the folder → build items → show grid
   immediately. In the background, `catalog` rescans: new/changed files get
   EXIF capture time read, sidecars newer than `xmp_synced_at` are imported,
   removed files are dropped. Changes are applied to the model on the main
   thread.
2. **Thumbnails:** a grid cell bound to an item requests its thumbnail from
   `cache`. Memory hit → texture. Miss → disk cache lookup job → on miss, decode
   job → result sent over a channel to the main thread → texture created →
   cell updated if still bound to that item.
3. **Rating:** UI updates the item and the index synchronously (single SQLite
   write, < 1 ms), then enqueues an XMP write job.
4. **Folder monitor:** `gio::FileMonitor` events are debounced (250 ms) and
   trigger an incremental rescan.

## 5. Performance

### 5.1 Targets (typical laptop, SSD, 24 MP images)

- Re-opening a 2,000-image folder: grid with cached thumbnails < 200 ms.
- New folder: grid with placeholders immediately; thumbnails stream in.
- Full view navigation to a prefetched image: < 16 ms.
- Grid scrolling: 60 fps with no main-thread decode work.

### 5.2 Decode strategy

| Format | Thumbnail (256 px) | Full view |
|---|---|---|
| JPEG | turbojpeg at the smallest DCT scale ≥ 256 px (EXIF thumbnails are skipped: usually 160 px and blurry at grid sizes) | turbojpeg at the smallest DCT scale ≥ screen size |
| RAW | Largest embedded preview (exiv2, then LibRaw thumbnail), downscaled | Same preview; `Z`/100% → full LibRaw decode |
| PNG/TIFF | `image` decode + downscale | `image` decode + downscale to screen |

- A RAW with no preview (or preview < 1024 px for full view) falls back to
  LibRaw half-size decode.
- EXIF orientation is applied during decode for all formats.
- Downscaling uses `fast_image_resize` (Lanczos3 for full view, bilinear for
  thumbnails).

### 5.3 Caching

- **Disk:** `$XDG_CACHE_HOME/photobrowser/thumbs/<hash>.jpg` (JPEG q85:
  libjpeg-turbo encodes/decodes it in < 1 ms; the `image` crate's WebP encoder
  is lossless-only), 256 px long edge, where `hash = blake3(path ‖ mtime_ns ‖ size)`. Stale entries are
  unreachable by construction; a size-capped cleanup (default 2 GB, oldest
  first) runs at startup in the background.
- **Memory:** LRU of ~300 thumbnail textures; LRU of ~6 screen-sized full-view
  images (current ±2 plus one spare); full LibRaw decode kept for current and
  previous image only. Limits configurable in preferences.

### 5.4 Job scheduling

- Workers = `max(1, num_cpus - 1)`.
- Priorities (high → low): current full view → visible thumbnails →
  prefetch ±2 → background disk-cache warm-up. (GridView already binds rows
  just outside the viewport, so they count as visible.)
- XMP writes run on one dedicated thread (ordered, never concurrent per file);
  move/copy runs on its own thread; neither uses the decode pool.
- Every job carries a generation/cancellation token checked before start;
  jobs for items scrolled out of range or for a closed folder are dropped.
- Results cross to the main thread via `async_channel` +
  `glib::MainContext::spawn_local`; `gdk::MemoryTexture` is created on the main
  thread.

## 6. UI

### 6.1 Window

`adw::ApplicationWindow` with a header bar: **Open Folder**, filter controls
(min-rating selector, reject filter, type filter, sort menu), and an item
counter `shown / total`. Content is an `adw::ViewStack`-like switch between
grid view and full view.

### 6.2 Grid view

- `gtk::GridView` over a `gtk::FilterListModel` + `gtk::SortListModel` over a
  `gio::ListStore` of item GObjects, with `gtk::MultiSelection`.
- Cell: thumbnail, star row, reject marker, `RAW+JPG` badge for pairs,
  "broken image" placeholder with error tooltip on decode failure.
- Thumbnail display size 128–512 px via `Ctrl+scroll` or `+`/`-`.
- Selection: click, `Ctrl`/`Shift`+click, `Ctrl+A`, `Shift+arrows`.

### 6.3 Full view

- Entered with `Enter` / double-click; `Esc` returns to the grid with the
  current item selected and scrolled into view.
- Navigation: arrows, `Space` / `Backspace`.
- `Z` / double-click toggles fit ↔ 100% (triggers full RAW decode when needed);
  drag to pan at 100%.
- `I` toggles an info overlay: filename(s), rating, camera, lens, shutter,
  aperture, ISO, focal length.
- Optional filmstrip at the bottom (toggle `F`).

### 6.4 Keyboard shortcuts

Apply to the current item in full view, or to the selection in grid view.

| Key | Action |
|---|---|
| `1`–`5` | Set rating |
| `0` | Clear rating |
| `X` | Toggle reject |
| `M` | Move to folder… |
| `C` | Copy to folder… |
| `Ctrl+Shift+M` | Move to last destination |
| `Ctrl+F` | Focus filter bar |
| `Ctrl+O` | Open folder |

Preference: *auto-advance after rating* (default off).

### 6.5 Preferences

Stored as plain TOML (no GSettings schema to install) at
`$XDG_CONFIG_HOME/photobrowser/config.toml`: auto-advance, memory cache
limits, disk cache cap, last opened folder.

## 7. Error handling

| Situation | Behavior |
|---|---|
| Corrupt/undecodable file | Broken placeholder with error tooltip; no crash, no effect on other items |
| RAW without usable preview | LibRaw half-size fallback |
| XMP write fails | Rating kept in index, `xmp_dirty = 1`, toast "Couldn't write XMP for N files", retried on next folder open |
| Sidecar changed externally | Imported on rescan (sidecar newer than `xmp_synced_at` wins) |
| Destination name conflict | One Skip Existing / Keep Both / Cancel prompt per batch; never overwrite |
| Cross-filesystem move failure | Original kept; partial item transfers rolled back |
| Index corrupt / schema mismatch | Delete and rebuild from scan + sidecars |
| Folder deleted/unmounted while open | Toast, grid cleared, return to "Open Folder" state |

Worker threads must not panic the app: decode calls are wrapped in
`catch_unwind` and turned into per-item errors. LibRaw calls are guarded
against null/invalid handles in the FFI wrapper.

## 8. Testing

- **Unit tests** (`cargo test`, no display needed):
  - pairing: mixed-case extensions, orphans, duplicate JPEG variants;
  - filter/sort logic including the "stays visible until navigating away" rule
    (tested at the model-logic level);
  - XMP round-trip; a real darktable sidecar fixture keeps all other fields;
  - reject ↔ `xmp:Rating = -1` mapping and restoration of the star rating;
  - fileops: conflicts (skip/rename), rollback after an injected failure,
    sidecars moving with their files;
  - index invalidation on mtime/size change; schema-mismatch rebuild.
- **Fixture tests**: JPEG (plain, rotated), PNG and TIFF files are generated
  in the tests; a real darktable sidecar lives in `tests/fixtures/`. RAW tests
  run against a folder of the user's own RAW files named by `PB_RAW_SAMPLES`
  (skipped, with a message, when unset).
- **Benchmarks** (`criterion`): thumbnail decode per format; scan of a
  generated 2,000-file folder.
- **Manual acceptance** on a real folder of the user's RAW files, checking
  the performance targets and that ratings appear in darktable.
