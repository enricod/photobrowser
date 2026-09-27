# Photobrowser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a fast, keyboard-driven GTK 4 photo culling app in Rust that browses a folder of JPEG/PNG/TIFF/RAW files, rates them 1–5 (plus reject), filters/sorts, and moves/copies items (RAW+JPEG pairs as one unit) with darktable-compatible XMP sidecars.

**Architecture:** A single binary crate. GTK-free core modules (`media`, `catalog`, `index`, `xmp`, `exif`, `filter`, `jobs`, `decode`, `libraw`, `thumbcache`, `fileops`, `config`) are unit-tested without a display. The `ui` module wires them to gtk4-rs/libadwaita: a `gio::ListStore` of `ItemObject`s → `FilterListModel` → `SortListModel` → `MultiSelection`, shared by a `GridView` and a filmstrip `ListView`. Decoding runs on a priority worker pool; results reach the GTK main thread over an `async_channel` and become `gdk::MemoryTexture`s there.

**Tech Stack:** Rust (edition 2024, Fedora rustc ≥ 1.94), gtk4-rs 0.11 (`gtk4` renamed `gtk`), libadwaita-rs 0.9 (`adw`), rexiv2 0.10 (gexiv2 0.14), turbojpeg 1.5 (system libturbojpeg), LibRaw 0.22 via bindgen, image 0.25, fast_image_resize 6, rusqlite 0.40 (system SQLite), lru, blake3, async-channel, serde+toml, thiserror, dirs, chrono. Dev: tempfile, criterion.

**Spec:** `docs/superpowers/specs/2026-09-27-photobrowser-design.md`

## Global Constraints

- Platform: Linux (Fedora 44), GTK ≥ 4.22, libadwaita ≥ 1.5 API level (system has 1.9); cargo features `gtk4/v4_12`, `libadwaita/v1_5`.
- Supported extensions (case-insensitive): JPEG `.jpg .jpeg`; PNG `.png`; TIFF `.tif .tiff`; RAW `.cr2 .cr3 .nef .arw .raf .orf .rw2 .dng .pef .srw`.
- Folder browsing is non-recursive; hidden files (leading `.`) are ignored.
- Pairing: same folder + same base name (case-insensitive), exactly one RAW + exactly one JPEG → one item; anything else → one item per file.
- Rating 0–5; reject is separate; XMP `Xmp.xmp.Rating = -1` means rejected (darktable convention); un-rejecting restores the stars.
- Sidecar name: `<filename with extension>.xmp` (e.g. `IMG_001.CR3.xmp`); for a pair, both sidecars are written. Only `Xmp.xmp.Rating` is changed; all other sidecar content is preserved.
- Index: SQLite at `$XDG_CACHE_HOME/photobrowser/index.db`; sidecars are the source of truth; a corrupt or wrong-schema index is deleted and rebuilt.
- Thumbnail disk cache: `$XDG_CACHE_HOME/photobrowser/thumbs/`, 256 px long edge, key = blake3(path ‖ mtime_ns ‖ size), cap 2 GB default, oldest removed first.
- Config: `$XDG_CONFIG_HOME/photobrowser/config.toml`.
- Move/copy never overwrites silently; each item (all files + sidecars) is transferred atomically with rollback.
- Performance targets: re-open 2,000-image folder < 200 ms to grid with cached thumbnails; prefetched full-view navigation < 16 ms; no decoding on the GTK main thread.
- Worker count = `max(1, available_parallelism − 1)`; priorities high→low: Current, Visible, Prefetch, Background.
- Never call exiv2 before `exif::init()`; every public function that touches exiv2 calls it.
- Commit after every task; commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Non-UTF-8 or unusual filenames** (bytes like `0xFF`, spaces, accents): pairing, the index, and move/copy must handle them exactly — pinned by `non_utf8_names_pair` (Task 2), `non_utf8_path_roundtrip` (Task 3), `moves_non_utf8_names` (Task 11).
2. **Corrupt, truncated or zero-byte image files** (half-copied from a card): decode must return an error, never panic or abort the app — pinned by `garbage_jpeg_is_error`, `truncated_jpeg_does_not_panic`, `corrupt_png_is_error`, `guarded_turns_panic_into_error` (Task 8).
3. **Empty or malformed sidecars** (0-byte file, non-XMP garbage): reading must not break the scan; writing must never destroy a non-empty sidecar it cannot parse — pinned by `empty_sidecar_is_replaced_on_write`, `garbage_sidecar_is_left_untouched` (Task 4) and `malformed_sidecar_does_not_break_scan` (Task 5).
4. **Moving into the folder that is already open / the item's own folder**: must be a no-op, not a rename-duplicate — pinned by `move_into_same_folder_is_noop` (Task 11).
5. **A file edited after its thumbnail was cached** (e.g. JPEG exported over the original): the stale thumbnail must not be shown — pinned by `key_changes_with_mtime_and_size` (Task 10) and `mtime_change_marks_display_changed` (Task 6).

---

## File Structure

```
Cargo.toml                    crate manifest, deps, profiles, bench target
build.rs                      bindgen for LibRaw (libraw_r)
README.md                     build/run/shortcuts/compatibility notes
src/main.rs                   calls ui::run()
src/lib.rs                    module list
src/media.rs                  FileKind, MediaFile, Item, ItemId, mtime_ns()
src/catalog/mod.rs            re-exports
src/catalog/pairing.rs        group files → items (RAW+JPEG pairing)
src/catalog/scan.rs           list media files in a folder
src/catalog/sync.rs           scan + index reconciliation + sidecar import
src/catalog/diff.rs           old items vs rescanned items → ItemDiff
src/catalog/rating.rs         RatingChange + apply()
src/index.rs                  SQLite index
src/exif.rs                   exiv2 init, capture time, orientation, ExifInfo
src/xmp.rs                    sidecar read/write
src/filter.rs                 FilterState, SortState, compare()
src/jobs.rs                   priority worker pool + CancelToken
src/decode/mod.rs             DecodeError, decode_for_display(), decode_actual_size(), guarded()
src/decode/pixels.rs          Rgba, fit_within, resize_to_fit, apply_orientation
src/decode/jpeg.rs            turbojpeg scaled decode
src/decode/generic.rs         PNG/TIFF via image crate
src/decode/raw.rs             RAW previews + full decode
src/libraw/mod.rs             safe LibRaw wrapper
src/libraw/wrapper.h          bindgen header
src/thumbcache.rs             on-disk JPEG thumbnail cache
src/fileops.rs                move/copy with conflicts + rollback
src/config.rs                 TOML preferences
src/testutil.rs               (cfg(test)) image/EXIF fixture helpers
src/ui/mod.rs                 run(), APP_ID
src/ui/style.rs               CSS + thumbnail size provider
src/ui/item_object.rs         ItemObject GObject + stars_text()
src/ui/state.rs               AppState: models, caches, message handling
src/ui/window.rs              window construction
src/ui/grid.rs                thumbnail factory + GridView
src/ui/keys.rs                keyboard shortcuts
src/ui/filterbar.rs           filter/sort dropdowns
src/ui/viewer.rs              full-size viewer + info_text()
src/ui/dialogs.rs             destination + conflict dialogs
src/ui/prefs.rs               preferences dialog
tests/fixtures/darktable.xmp  real-world darktable sidecar
benches/pipeline.rs           criterion benchmarks
```

---

### Task 1: Toolchain and project scaffold

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `src/main.rs`, `src/lib.rs`, `src/ui/mod.rs`

**Interfaces:**
- Consumes: nothing
- Produces: `photobrowser::ui::run() -> gtk::glib::ExitCode`; crate names `gtk` (gtk4) and `adw` (libadwaita) usable everywhere.

- [ ] **Step 1: Install system packages** (needs sudo; the user runs it with `! ` in Claude Code)

```bash
sudo dnf install -y rust cargo rustfmt clippy gtk4-devel libadwaita-devel \
  LibRaw-devel libgexiv2_0.14-devel turbojpeg-devel sqlite-devel clang-devel pkgconf-pkg-config
```

Note: `libgexiv2_0.14-devel` (pkg-config name `gexiv2`) is required — `rexiv2`/`gexiv2-sys` probe `gexiv2`, while Fedora's default `libgexiv2-devel` is 0.16 and ships `gexiv2-0.16.pc`.

- [ ] **Step 2: Verify the libraries are visible**

Run: `pkg-config --modversion gtk4 libadwaita-1 libraw_r gexiv2 libturbojpeg sqlite3 && rustc --version`
Expected: six version lines (gtk4 ≥ 4.22, libadwaita-1 ≥ 1.5, libraw_r 0.22.x, gexiv2 0.14.x, libturbojpeg 3.x, sqlite3 3.x) and `rustc 1.9x`.

- [ ] **Step 3: Create the crate and add dependencies**

```bash
cd /home/enrico/Projects/photobrowser
cargo init --name photobrowser --edition 2024
cargo add gtk4 --rename gtk --features v4_12
cargo add libadwaita --rename adw --features v1_5
cargo add rexiv2
cargo add turbojpeg --no-default-features --features pkg-config
cargo add image --no-default-features --features png,tiff,jpeg
cargo add fast_image_resize rusqlite blake3 lru async-channel thiserror dirs toml
cargo add chrono --no-default-features --features std
cargo add serde --features derive
cargo add --build bindgen pkg-config
cargo add --dev tempfile criterion
```

`cargo add` picks the newest compatible versions (gtk4 0.11 / libadwaita 0.9 are a matched pair). If `cargo add libadwaita` picks a version whose `gtk4` requirement differs from the `gtk4` just added, run `cargo tree -i gtk4` and align the `gtk4` version to what libadwaita requires.

- [ ] **Step 4: Add profiles to `Cargo.toml`**

Append:

```toml
[profile.dev.package."*"]
opt-level = 2          # decoders/resizers are unusably slow unoptimised

[profile.release]
lto = "thin"
codegen-units = 1
```

- [ ] **Step 5: Write `.gitignore`**

```
/target
```

- [ ] **Step 6: Write `src/lib.rs`**

```rust
pub mod ui;
```

- [ ] **Step 7: Write `src/main.rs`**

```rust
fn main() -> gtk::glib::ExitCode {
    photobrowser::ui::run()
}
```

- [ ] **Step 8: Write `src/ui/mod.rs`**

```rust
use adw::prelude::*;

pub const APP_ID: &str = "io.github.enricod.PhotoBrowser";

pub fn run() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Photobrowser")
            .default_width(1400)
            .default_height(900)
            .build();
        window.present();
    });
    app.run_with_args::<&str>(&[])
}
```

- [ ] **Step 9: Build and run**

Run: `cargo build && cargo test`
Expected: builds; `test result: ok. 0 passed`.
Run: `cargo run` — an empty libadwaita window titled "Photobrowser" opens; close it.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src
git commit -m "chore: scaffold GTK4/libadwaita Rust app

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Media types and RAW+JPEG pairing

**Files:**
- Create: `src/media.rs`, `src/catalog/mod.rs`, `src/catalog/pairing.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `media::FileKind { Jpeg, Png, Tiff, Raw }` with `FileKind::from_path(&Path) -> Option<FileKind>`
  - `media::MediaFile { path: PathBuf, kind: FileKind, size: u64, mtime_ns: i64 }`
  - `media::ItemId = u64` (0 = not yet assigned)
  - `media::Item { id, base_name: String, files: Vec<MediaFile>, sort_time: i64, sort_name: String, rating: u8, rejected: bool }` with `Item::new(Vec<MediaFile>)`, `display_file() -> &MediaFile`, `raw_file() -> Option<&MediaFile>`, `is_pair() -> bool`, `display_name() -> String`, `file_names() -> String`
  - `media::mtime_ns(&std::fs::Metadata) -> i64`
  - `catalog::pairing::group_into_items(Vec<MediaFile>) -> Vec<Item>` (ids = 0; in a pair the RAW is `files[0]`)

- [ ] **Step 1: Write `src/media.rs` with failing tests**

```rust
use std::path::{Path, PathBuf};

pub type ItemId = u64;

const RAW_EXTS: &[&str] = &["cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "dng", "pef", "srw"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Jpeg,
    Png,
    Tiff,
    Raw,
}

impl FileKind {
    pub fn from_path(path: &Path) -> Option<FileKind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some(FileKind::Jpeg),
            "png" => Some(FileKind::Png),
            "tif" | "tiff" => Some(FileKind::Tiff),
            e if RAW_EXTS.contains(&e) => Some(FileKind::Raw),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ns: i64,
}

/// One browsable unit: a single file, or a RAW+JPEG pair (RAW first).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    pub base_name: String,
    pub files: Vec<MediaFile>,
    /// Capture time (unix seconds), falling back to the display file's mtime.
    pub sort_time: i64,
    /// Lower-cased display file name, precomputed for sorting.
    pub sort_name: String,
    pub rating: u8,
    pub rejected: bool,
}

impl Item {
    pub fn new(files: Vec<MediaFile>) -> Item {
        assert!(!files.is_empty(), "an item needs at least one file");
        let base_name = files[0]
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut item = Item {
            id: 0,
            base_name,
            files,
            sort_time: 0,
            sort_name: String::new(),
            rating: 0,
            rejected: false,
        };
        item.sort_name = item.display_name().to_lowercase();
        item
    }

    /// The file shown on screen: the JPEG of a pair, otherwise the only file.
    pub fn display_file(&self) -> &MediaFile {
        self.files
            .iter()
            .find(|f| f.kind == FileKind::Jpeg)
            .unwrap_or(&self.files[0])
    }

    pub fn raw_file(&self) -> Option<&MediaFile> {
        self.files.iter().find(|f| f.kind == FileKind::Raw)
    }

    pub fn is_pair(&self) -> bool {
        self.files.len() > 1
    }

    pub fn display_name(&self) -> String {
        file_name(&self.display_file().path)
    }

    /// All file names, e.g. "IMG_1.CR3 + IMG_1.JPG".
    pub fn file_names(&self) -> String {
        self.files
            .iter()
            .map(|f| file_name(&f.path))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mf(p: &str) -> MediaFile {
        MediaFile {
            path: PathBuf::from(p),
            kind: FileKind::from_path(Path::new(p)).unwrap(),
            size: 1,
            mtime_ns: 0,
        }
    }

    #[test]
    fn classifies_extensions() {
        assert_eq!(FileKind::from_path(Path::new("a.JPG")), Some(FileKind::Jpeg));
        assert_eq!(FileKind::from_path(Path::new("a.jpeg")), Some(FileKind::Jpeg));
        assert_eq!(FileKind::from_path(Path::new("a.png")), Some(FileKind::Png));
        assert_eq!(FileKind::from_path(Path::new("a.TIFF")), Some(FileKind::Tiff));
        assert_eq!(FileKind::from_path(Path::new("a.NEF")), Some(FileKind::Raw));
        assert_eq!(FileKind::from_path(Path::new("a.cr3")), Some(FileKind::Raw));
        assert_eq!(FileKind::from_path(Path::new("a.txt")), None);
        assert_eq!(FileKind::from_path(Path::new("noext")), None);
    }

    #[test]
    fn display_file_prefers_jpeg() {
        let item = Item::new(vec![mf("/p/IMG_1.CR3"), mf("/p/IMG_1.JPG")]);
        assert_eq!(item.display_file().path, PathBuf::from("/p/IMG_1.JPG"));
        assert_eq!(item.raw_file().unwrap().path, PathBuf::from("/p/IMG_1.CR3"));
        assert!(item.is_pair());
        assert_eq!(item.sort_name, "img_1.jpg");
        assert_eq!(item.file_names(), "IMG_1.CR3 + IMG_1.JPG");
        assert_eq!(item.base_name, "IMG_1");
    }
}
```

- [ ] **Step 2: Write `src/catalog/pairing.rs` with tests**

```rust
use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::media::{FileKind, Item, MediaFile};

/// Groups files into items. Exactly one RAW + one JPEG with the same folder and
/// case-insensitive stem become a pair (RAW first); everything else is single.
/// Returned items have `id == 0`.
pub fn group_into_items(files: Vec<MediaFile>) -> Vec<Item> {
    let mut groups: BTreeMap<(PathBuf, Vec<u8>), Vec<MediaFile>> = BTreeMap::new();
    for f in files {
        let dir = f.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = f
            .path
            .file_stem()
            .map(|s| s.as_bytes().to_ascii_lowercase())
            .unwrap_or_default();
        groups.entry((dir, stem)).or_default().push(f);
    }

    let mut items = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a, b| a.path.cmp(&b.path));
        let raws = group.iter().filter(|f| f.kind == FileKind::Raw).count();
        let jpegs = group.iter().filter(|f| f.kind == FileKind::Jpeg).count();
        if group.len() == 2 && raws == 1 && jpegs == 1 {
            group.sort_by_key(|f| f.kind != FileKind::Raw);
            items.push(Item::new(group));
        } else {
            items.extend(group.into_iter().map(|f| Item::new(vec![f])));
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn mf(p: &str) -> MediaFile {
        mf_path(PathBuf::from(p))
    }

    fn mf_path(path: PathBuf) -> MediaFile {
        let kind = FileKind::from_path(&path).unwrap();
        MediaFile { path, kind, size: 1, mtime_ns: 0 }
    }

    fn names(items: &[Item]) -> Vec<String> {
        items.iter().map(|i| i.file_names()).collect()
    }

    #[test]
    fn pairs_raw_and_jpeg_with_same_stem() {
        let items = group_into_items(vec![mf("/p/IMG_1.JPG"), mf("/p/IMG_1.CR3")]);
        assert_eq!(names(&items), ["IMG_1.CR3 + IMG_1.JPG"]);
        assert_eq!(items[0].files[0].kind, FileKind::Raw);
    }

    #[test]
    fn pairs_case_insensitively() {
        let items = group_into_items(vec![mf("/p/img_001.CR3"), mf("/p/IMG_001.jpg")]);
        assert_eq!(items.len(), 1);
        assert!(items[0].is_pair());
    }

    #[test]
    fn orphans_are_single_items() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/B.JPG")]);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| !i.is_pair()));
    }

    #[test]
    fn duplicate_jpeg_variants_are_not_paired() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/A.jpg"), mf("/p/A.JPG")]);
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn png_does_not_pair_with_raw() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/A.png")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn two_raws_do_not_pair() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/p/A.DNG")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn same_stem_in_different_dirs_not_paired() {
        let items = group_into_items(vec![mf("/p/A.CR3"), mf("/q/A.JPG")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn non_utf8_names_pair() {
        let dir = PathBuf::from("/p");
        let raw = dir.join(OsStr::from_bytes(b"\xffphoto.cr3"));
        let jpg = dir.join(OsStr::from_bytes(b"\xffPHOTO.JPG"));
        let items = group_into_items(vec![mf_path(raw), mf_path(jpg)]);
        assert_eq!(items.len(), 1);
        assert!(items[0].is_pair());
    }
}
```

- [ ] **Step 3: Write `src/catalog/mod.rs` and register modules**

`src/catalog/mod.rs`:

```rust
pub mod pairing;
```

`src/lib.rs`:

```rust
pub mod catalog;
pub mod media;
pub mod ui;
```

- [ ] **Step 4: Run tests**

Run: `cargo test media catalog`
Expected: PASS — 10 tests (`classifies_extensions`, `display_file_prefers_jpeg`, 8 pairing tests).

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat: media types and RAW+JPEG pairing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: SQLite index

**Files:**
- Create: `src/index.rs`
- Modify: `src/lib.rs` (add `pub mod index;`)

**Interfaces:**
- Consumes: `media::MediaFile`
- Produces:
  - `index::FileRecord { path: PathBuf, size: u64, mtime_ns: i64, capture_time: Option<i64>, rating: u8, rejected: bool, xmp_synced_at: Option<i64>, xmp_dirty: bool }` + `FileRecord::new(path, size, mtime_ns)`
  - `index::Index` with: `default_path() -> PathBuf`, `open(&Path) -> Result<Index>`, `open_in_memory() -> Result<Index>`, `load_folder(&Path) -> Result<HashMap<PathBuf, FileRecord>>`, `get(&Path) -> Result<Option<FileRecord>>`, `upsert_many(&mut self, &[FileRecord]) -> Result<()>`, `remove_paths(&mut self, &[PathBuf]) -> Result<()>`, `set_rating(&MediaFile, u8, bool) -> Result<()>`, `mark_xmp_synced(&Path, i64) -> Result<()>`, `mark_xmp_dirty(&Path) -> Result<()>`, `dirty_records(&Path) -> Result<Vec<FileRecord>>`, `rename_path(&mut self, &Path, &Path) -> Result<()>`, `add_recent_destination(&Path, i64) -> Result<()>`, `recent_destinations() -> Result<Vec<PathBuf>>`
  - `index::IndexError`, `index::Result<T>`
- Upsert rule: a row with `xmp_dirty = 1` keeps its rating/rejected/xmp_synced_at (a pending local edit wins over a scan).

- [ ] **Step 1: Write the failing tests** (bottom of `src/index.rs`; implementation in step 3)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FileKind;
    use std::ffi::OsStr;

    fn rec(p: &str) -> FileRecord {
        FileRecord::new(PathBuf::from(p), 10, 100)
    }

    fn media(p: &str) -> MediaFile {
        MediaFile { path: PathBuf::from(p), kind: FileKind::Jpeg, size: 10, mtime_ns: 100 }
    }

    #[test]
    fn upsert_and_load_folder_roundtrip() {
        let mut idx = Index::open_in_memory().unwrap();
        let mut a = rec("/p/a.jpg");
        a.capture_time = Some(1234);
        a.rating = 3;
        idx.upsert_many(&[a.clone(), rec("/p/sub/b.jpg"), rec("/q/c.jpg")]).unwrap();
        let loaded = idx.load_folder(Path::new("/p")).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[Path::new("/p/a.jpg")], a);
    }

    #[test]
    fn set_rating_marks_dirty_and_mark_synced_clears() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.upsert_many(&[rec("/p/a.jpg")]).unwrap();
        idx.set_rating(&media("/p/a.jpg"), 4, false).unwrap();
        let r = idx.get(Path::new("/p/a.jpg")).unwrap().unwrap();
        assert_eq!((r.rating, r.rejected, r.xmp_dirty), (4, false, true));
        idx.mark_xmp_synced(Path::new("/p/a.jpg"), 555).unwrap();
        let r = idx.get(Path::new("/p/a.jpg")).unwrap().unwrap();
        assert_eq!((r.xmp_dirty, r.xmp_synced_at), (false, Some(555)));
        idx.mark_xmp_dirty(Path::new("/p/a.jpg")).unwrap();
        assert_eq!(idx.dirty_records(Path::new("/p")).unwrap().len(), 1);
    }

    #[test]
    fn set_rating_inserts_missing_row() {
        let idx = Index::open_in_memory().unwrap();
        idx.set_rating(&media("/p/new.jpg"), 2, true).unwrap();
        let r = idx.get(Path::new("/p/new.jpg")).unwrap().unwrap();
        assert_eq!((r.rating, r.rejected, r.xmp_dirty, r.size), (2, true, true, 10));
    }

    #[test]
    fn upsert_does_not_clobber_dirty_rating() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.upsert_many(&[rec("/p/a.jpg")]).unwrap();
        idx.set_rating(&media("/p/a.jpg"), 5, false).unwrap();
        let mut scanned = rec("/p/a.jpg");
        scanned.size = 99;
        scanned.rating = 1;
        idx.upsert_many(&[scanned]).unwrap();
        let r = idx.get(Path::new("/p/a.jpg")).unwrap().unwrap();
        assert_eq!((r.size, r.rating, r.xmp_dirty), (99, 5, true));
    }

    #[test]
    fn rename_path_moves_record_to_new_folder() {
        let mut idx = Index::open_in_memory().unwrap();
        let mut a = rec("/p/a.jpg");
        a.rating = 2;
        idx.upsert_many(&[a]).unwrap();
        idx.rename_path(Path::new("/p/a.jpg"), Path::new("/q/a.jpg")).unwrap();
        assert!(idx.load_folder(Path::new("/p")).unwrap().is_empty());
        assert_eq!(idx.load_folder(Path::new("/q")).unwrap()[Path::new("/q/a.jpg")].rating, 2);
    }

    #[test]
    fn remove_paths_deletes_rows() {
        let mut idx = Index::open_in_memory().unwrap();
        idx.upsert_many(&[rec("/p/a.jpg"), rec("/p/b.jpg")]).unwrap();
        idx.remove_paths(&[PathBuf::from("/p/a.jpg")]).unwrap();
        assert_eq!(idx.load_folder(Path::new("/p")).unwrap().len(), 1);
    }

    #[test]
    fn recent_destinations_order_and_cap() {
        let idx = Index::open_in_memory().unwrap();
        for i in 0..12 {
            idx.add_recent_destination(Path::new(&format!("/d/{i}")), i).unwrap();
        }
        idx.add_recent_destination(Path::new("/d/3"), 100).unwrap();
        let recent = idx.recent_destinations().unwrap();
        assert_eq!(recent.len(), 10);
        assert_eq!(recent[0], PathBuf::from("/d/3"));
        assert_eq!(recent[1], PathBuf::from("/d/11"));
    }

    #[test]
    fn garbage_file_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        std::fs::write(&path, b"this is not a database at all, just junk bytes").unwrap();
        let idx = Index::open(&path).unwrap();
        assert!(idx.load_folder(Path::new("/p")).unwrap().is_empty());
    }

    #[test]
    fn schema_mismatch_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.db");
        {
            let mut idx = Index::open(&path).unwrap();
            idx.upsert_many(&[rec("/p/a.jpg")]).unwrap();
            idx.conn
                .execute("UPDATE meta SET value='0' WHERE key='schema_version'", [])
                .unwrap();
        }
        let idx = Index::open(&path).unwrap();
        assert!(idx.load_folder(Path::new("/p")).unwrap().is_empty());
    }

    #[test]
    fn non_utf8_path_roundtrip() {
        let mut idx = Index::open_in_memory().unwrap();
        let path = Path::new("/p").join(OsStr::from_bytes(b"\xff\xfe.jpg"));
        idx.upsert_many(&[FileRecord::new(path.clone(), 1, 1)]).unwrap();
        let loaded = idx.load_folder(Path::new("/p")).unwrap();
        assert!(loaded.contains_key(&path));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test index`
Expected: FAIL to compile (`Index`, `FileRecord` not defined).

- [ ] **Step 3: Write the implementation (top of `src/index.rs`)**

```rust
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::media::MediaFile;

pub const SCHEMA_VERSION: &str = "1";
const MAX_RECENT: i64 = 10;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("index schema version mismatch")]
    SchemaMismatch,
}

pub type Result<T> = std::result::Result<T, IndexError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
    pub capture_time: Option<i64>,
    pub rating: u8,
    pub rejected: bool,
    /// Sidecar mtime (ns) at the last successful read or write.
    pub xmp_synced_at: Option<i64>,
    /// A local rating change has not been written to the sidecar yet.
    pub xmp_dirty: bool,
}

impl FileRecord {
    pub fn new(path: PathBuf, size: u64, mtime_ns: i64) -> FileRecord {
        FileRecord {
            path,
            size,
            mtime_ns,
            capture_time: None,
            rating: 0,
            rejected: false,
            xmp_synced_at: None,
            xmp_dirty: false,
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files (
  path BLOB PRIMARY KEY,
  folder BLOB NOT NULL,
  size INTEGER NOT NULL,
  mtime_ns INTEGER NOT NULL,
  capture_time INTEGER,
  rating INTEGER NOT NULL DEFAULT 0,
  rejected INTEGER NOT NULL DEFAULT 0,
  xmp_synced_at INTEGER,
  xmp_dirty INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS files_folder ON files(folder);
CREATE TABLE IF NOT EXISTS recent_destinations (path BLOB PRIMARY KEY, used_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

const COLUMNS: &str = "path, size, mtime_ns, capture_time, rating, rejected, xmp_synced_at, xmp_dirty";

const UPSERT: &str = "
INSERT INTO files (path, folder, size, mtime_ns, capture_time, rating, rejected, xmp_synced_at, xmp_dirty)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
ON CONFLICT(path) DO UPDATE SET
  folder = excluded.folder,
  size = excluded.size,
  mtime_ns = excluded.mtime_ns,
  capture_time = excluded.capture_time,
  rating = CASE WHEN files.xmp_dirty = 1 THEN files.rating ELSE excluded.rating END,
  rejected = CASE WHEN files.xmp_dirty = 1 THEN files.rejected ELSE excluded.rejected END,
  xmp_synced_at = CASE WHEN files.xmp_dirty = 1 THEN files.xmp_synced_at ELSE excluded.xmp_synced_at END,
  xmp_dirty = MAX(files.xmp_dirty, excluded.xmp_dirty)
";

fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

fn folder_bytes(p: &Path) -> &[u8] {
    bytes(p.parent().unwrap_or(Path::new("")))
}

fn path_from(v: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(v))
}

fn row_to_record(r: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        path: path_from(r.get(0)?),
        size: r.get::<_, i64>(1)? as u64,
        mtime_ns: r.get(2)?,
        capture_time: r.get(3)?,
        rating: r.get(4)?,
        rejected: r.get(5)?,
        xmp_synced_at: r.get(6)?,
        xmp_dirty: r.get(7)?,
    })
}

pub struct Index {
    conn: Connection,
}

impl Index {
    pub fn default_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("photobrowser")
            .join("index.db")
    }

    /// Opens (or creates) the index; a corrupt or outdated file is deleted and rebuilt.
    pub fn open(path: &Path) -> Result<Index> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match Self::try_open(path) {
            Ok(index) => Ok(index),
            Err(_) => {
                for suffix in ["", "-wal", "-shm"] {
                    let mut p = path.as_os_str().to_owned();
                    p.push(suffix);
                    let _ = std::fs::remove_file(p);
                }
                Self::try_open(path)
            }
        }
    }

    fn try_open(path: &Path) -> Result<Index> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Index> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Index> {
        conn.execute_batch(SCHEMA)?;
        let version: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
            .optional()?;
        match version.as_deref() {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                    [SCHEMA_VERSION],
                )?;
            }
            Some(v) if v == SCHEMA_VERSION => {}
            Some(_) => return Err(IndexError::SchemaMismatch),
        }
        Ok(Index { conn })
    }

    pub fn load_folder(&self, folder: &Path) -> Result<HashMap<PathBuf, FileRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM files WHERE folder = ?1"))?;
        let rows = stmt.query_map([bytes(folder)], row_to_record)?;
        let mut map = HashMap::new();
        for row in rows {
            let rec = row?;
            map.insert(rec.path.clone(), rec);
        }
        Ok(map)
    }

    pub fn get(&self, path: &Path) -> Result<Option<FileRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM files WHERE path = ?1"))?;
        Ok(stmt.query_row([bytes(path)], row_to_record).optional()?)
    }

    pub fn upsert_many(&mut self, recs: &[FileRecord]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(UPSERT)?;
            for r in recs {
                stmt.execute(params![
                    bytes(&r.path),
                    folder_bytes(&r.path),
                    r.size as i64,
                    r.mtime_ns,
                    r.capture_time,
                    r.rating,
                    r.rejected,
                    r.xmp_synced_at,
                    r.xmp_dirty,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_paths(&mut self, paths: &[PathBuf]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM files WHERE path = ?1")?;
            for p in paths {
                stmt.execute([bytes(p)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Records a local rating change (inserting the row if the scan has not yet).
    pub fn set_rating(&self, file: &MediaFile, rating: u8, rejected: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO files (path, folder, size, mtime_ns, rating, rejected, xmp_dirty)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT(path) DO UPDATE SET
               rating = excluded.rating, rejected = excluded.rejected, xmp_dirty = 1",
            params![
                bytes(&file.path),
                folder_bytes(&file.path),
                file.size as i64,
                file.mtime_ns,
                rating,
                rejected
            ],
        )?;
        Ok(())
    }

    pub fn mark_xmp_synced(&self, path: &Path, sidecar_mtime_ns: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET xmp_dirty = 0, xmp_synced_at = ?2 WHERE path = ?1",
            params![bytes(path), sidecar_mtime_ns],
        )?;
        Ok(())
    }

    pub fn mark_xmp_dirty(&self, path: &Path) -> Result<()> {
        self.conn
            .execute("UPDATE files SET xmp_dirty = 1 WHERE path = ?1", [bytes(path)])?;
        Ok(())
    }

    pub fn dirty_records(&self, folder: &Path) -> Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM files WHERE folder = ?1 AND xmp_dirty = 1"
        ))?;
        let rows = stmt.query_map([bytes(folder)], row_to_record)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn rename_path(&mut self, from: &Path, to: &Path) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM files WHERE path = ?1", [bytes(to)])?;
        tx.execute(
            "UPDATE files SET path = ?2, folder = ?3 WHERE path = ?1",
            params![bytes(from), bytes(to), folder_bytes(to)],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_recent_destination(&self, dir: &Path, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO recent_destinations (path, used_at) VALUES (?1, ?2)",
            params![bytes(dir), now],
        )?;
        self.conn.execute(
            "DELETE FROM recent_destinations WHERE path NOT IN
               (SELECT path FROM recent_destinations ORDER BY used_at DESC LIMIT ?1)",
            [MAX_RECENT],
        )?;
        Ok(())
    }

    pub fn recent_destinations(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path FROM recent_destinations ORDER BY used_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(path_from)
            .collect())
    }
}
```

Add `pub mod index;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test index`
Expected: PASS — 10 tests.

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat: SQLite index with rebuild-on-corruption

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: EXIF helpers and XMP sidecars

**Files:**
- Create: `src/exif.rs`, `src/xmp.rs`, `src/testutil.rs`, `tests/fixtures/darktable.xmp`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `media::mtime_ns`
- Produces:
  - `exif::init()` (idempotent), `exif::parse_exif_datetime(&str) -> Option<i64>`, `exif::orientation(&rexiv2::Metadata) -> u8` (1–8), `exif::capture_time(&Path) -> Option<i64>`, `exif::ExifInfo { camera, lens, exposure: Option<String>, aperture: Option<f64>, iso: Option<i32>, focal_length: Option<f64> }` + `ExifInfo::summary() -> String`, `exif::read_info(&Path) -> ExifInfo`, `exif::format_exposure(i32, i32) -> String`
  - `xmp::XmpRating { Stars(u8), Rejected }`, `xmp::sidecar_path(&Path) -> PathBuf`, `xmp::is_sidecar(&Path) -> bool`, `xmp::to_xmp_value(u8, bool) -> i32`, `xmp::from_xmp_value(i32) -> XmpRating`, `xmp::sidecar_mtime_ns(&Path) -> Option<i64>`, `xmp::read_rating(&Path) -> Result<Option<XmpRating>, XmpError>`, `xmp::write_rating(&Path, u8, bool) -> Result<i64, XmpError>` (returns the sidecar's new mtime in ns). The `&Path` arguments are **media file** paths, not sidecar paths.
  - `testutil::{write_jpeg, write_two_tone_jpeg, write_png, write_tiff, set_exif}` (cfg(test) only)

- [ ] **Step 1: Write the darktable fixture `tests/fixtures/darktable.xmp`**

```xml
<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="XMP Core 4.4.0-Exiv2">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:darktable="http://darktable.sf.net/"
   exif:DateTimeOriginal="2026:09:01 10:15:30"
   xmp:Rating="2"
   xmpMM:DerivedFrom="IMG_0001.CR3"
   darktable:import_timestamp="63890000000000000"
   darktable:xmp_version="5"
   darktable:raw_params="0"
   darktable:auto_presets_applied="1"
   darktable:history_end="1">
   <darktable:history>
    <rdf:Seq>
     <rdf:li
      darktable:num="0"
      darktable:operation="exposure"
      darktable:enabled="1"
      darktable:modversion="6"
      darktable:params="gz02eJxjYIAAAAAIAAE="
      darktable:multi_name=""
      darktable:multi_priority="0"/>
    </rdf:Seq>
   </darktable:history>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>
```

(The first line's `begin="…"` contains the U+FEFF byte-order mark; keep it.)

- [ ] **Step 2: Write `src/testutil.rs`**

```rust
//! Fixture helpers for unit tests. Images are generated, never checked in.
use std::path::Path;

pub fn write_jpeg(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
        .save_with_format(path, image::ImageFormat::Jpeg)
        .unwrap();
}

/// Left half red, right half green.
pub fn write_two_tone_jpeg(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, _| {
        if x < w / 2 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 255, 0]) }
    })
    .save_with_format(path, image::ImageFormat::Jpeg)
    .unwrap();
}

pub fn write_png(path: &Path, w: u32, h: u32, rgba: [u8; 4]) {
    image::RgbaImage::from_pixel(w, h, image::Rgba(rgba))
        .save_with_format(path, image::ImageFormat::Png)
        .unwrap();
}

pub fn write_tiff(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb))
        .save_with_format(path, image::ImageFormat::Tiff)
        .unwrap();
}

/// Writes one EXIF tag into an existing image file.
pub fn set_exif(path: &Path, tag: &str, value: &str) {
    crate::exif::init();
    let meta = rexiv2::Metadata::new_from_path(path).unwrap();
    meta.set_tag_string(tag, value).unwrap();
    meta.save_to_file(path).unwrap();
}
```

- [ ] **Step 3: Write `src/exif.rs` with tests**

```rust
use std::path::Path;
use std::sync::Once;

static INIT: Once = Once::new();

/// Initialises exiv2 once per process. Call before any rexiv2 use.
pub fn init() {
    INIT.call_once(|| {
        rexiv2::initialize().expect("failed to initialise exiv2");
        rexiv2::set_log_level(rexiv2::LogLevel::MUTE);
        let _ = rexiv2::register_xmp_namespace("http://darktable.sf.net/", "darktable");
    });
}

/// Parses EXIF "YYYY:MM:DD HH:MM:SS" as a naive timestamp (treated as UTC; used only for sorting).
pub fn parse_exif_datetime(s: &str) -> Option<i64> {
    let s = s.trim().trim_end_matches('\0');
    chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

/// EXIF orientation 1–8 (1 when missing or invalid).
pub fn orientation(meta: &rexiv2::Metadata) -> u8 {
    let v = meta.get_tag_numeric("Exif.Image.Orientation");
    if (1..=8).contains(&v) { v as u8 } else { 1 }
}

pub fn capture_time(path: &Path) -> Option<i64> {
    init();
    let meta = rexiv2::Metadata::new_from_path(path).ok()?;
    ["Exif.Photo.DateTimeOriginal", "Exif.Image.DateTime"]
        .iter()
        .find_map(|tag| meta.get_tag_string(tag).ok().and_then(|s| parse_exif_datetime(&s)))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExifInfo {
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub exposure: Option<String>,
    pub aperture: Option<f64>,
    pub iso: Option<i32>,
    pub focal_length: Option<f64>,
}

impl ExifInfo {
    /// e.g. "Canon EOS R6 · RF24-105mm · 1/250 s · f/4 · ISO 400 · 50 mm"
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.camera.clone());
        parts.extend(self.lens.clone());
        parts.extend(self.exposure.clone());
        parts.extend(self.aperture.map(|a| format!("f/{}", trim_float(a))));
        parts.extend(self.iso.map(|i| format!("ISO {i}")));
        parts.extend(self.focal_length.map(|f| format!("{f:.0} mm")));
        parts.join(" · ")
    }
}

pub fn read_info(path: &Path) -> ExifInfo {
    init();
    let Ok(meta) = rexiv2::Metadata::new_from_path(path) else {
        return ExifInfo::default();
    };
    let s = |tag: &str| {
        meta.get_tag_string(tag)
            .ok()
            .map(|v| v.trim().trim_end_matches('\0').to_string())
            .filter(|v| !v.is_empty())
    };
    let camera = match (s("Exif.Image.Make"), s("Exif.Image.Model")) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (None, model) => model,
        (make, None) => make,
    };
    ExifInfo {
        camera,
        lens: s("Exif.Photo.LensModel"),
        exposure: meta
            .get_exposure_time()
            .map(|r| format_exposure(*r.numer(), *r.denom())),
        aperture: meta.get_fnumber(),
        iso: meta.get_iso_speed(),
        focal_length: meta.get_focal_length(),
    }
}

pub fn format_exposure(num: i32, den: i32) -> String {
    if num <= 0 || den <= 0 {
        return "?".into();
    }
    let secs = num as f64 / den as f64;
    if secs >= 1.0 {
        format!("{} s", trim_float(secs))
    } else {
        format!("1/{} s", (1.0 / secs).round() as i64)
    }
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{set_exif, write_jpeg};

    #[test]
    fn parses_exif_datetime() {
        assert_eq!(parse_exif_datetime("2026:09:01 10:15:30"), Some(1_788_257_730));
        assert_eq!(parse_exif_datetime("2026:09:01 10:15:30\0"), Some(1_788_257_730));
        assert_eq!(parse_exif_datetime("garbage"), None);
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
    }

    #[test]
    fn formats_exposure() {
        assert_eq!(format_exposure(1, 250), "1/250 s");
        assert_eq!(format_exposure(10, 2500), "1/250 s");
        assert_eq!(format_exposure(2, 1), "2 s");
        assert_eq!(format_exposure(13, 10), "1.3 s");
        assert_eq!(format_exposure(0, 1), "?");
    }

    #[test]
    fn reads_capture_time_and_orientation_from_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        write_jpeg(&path, 16, 16, [0, 0, 255]);
        set_exif(&path, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        set_exif(&path, "Exif.Image.Orientation", "6");
        assert_eq!(capture_time(&path), Some(1_788_257_730));
        let meta = rexiv2::Metadata::new_from_path(&path).unwrap();
        assert_eq!(orientation(&meta), 6);
    }

    #[test]
    fn missing_metadata_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.cr3");
        std::fs::write(&path, b"not an image").unwrap();
        assert_eq!(capture_time(&path), None);
        assert_eq!(read_info(&path), ExifInfo::default());
    }

    #[test]
    fn summary_joins_known_fields() {
        let info = ExifInfo {
            camera: Some("Canon EOS R6".into()),
            exposure: Some("1/250 s".into()),
            aperture: Some(4.0),
            iso: Some(400),
            ..Default::default()
        };
        assert_eq!(info.summary(), "Canon EOS R6 · 1/250 s · f/4 · ISO 400");
    }
}
```

Verify the expected timestamp once: `date -u -d '2026-09-01 10:15:30' +%s` → `1788257730`. If it differs, use the printed value in the tests.

- [ ] **Step 4: Write `src/xmp.rs` with tests**

```rust
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::media::mtime_ns;

pub const RATING_TAG: &str = "Xmp.xmp.Rating";

const EMPTY_PACKET: &str = concat!(
    "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
    " <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
    "  <rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"/>\n",
    " </rdf:RDF>\n",
    "</x:xmpmeta>\n",
    "<?xpacket end=\"w\"?>\n",
);

#[derive(Debug, thiserror::Error)]
pub enum XmpError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("exiv2: {0}")]
    Exiv2(#[from] rexiv2::Rexiv2Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmpRating {
    Stars(u8),
    Rejected,
}

/// `IMG_001.CR3` → `IMG_001.CR3.xmp` (darktable naming).
pub fn sidecar_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".xmp");
    PathBuf::from(s)
}

pub fn is_sidecar(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

pub fn to_xmp_value(rating: u8, rejected: bool) -> i32 {
    if rejected { -1 } else { rating.min(5) as i32 }
}

pub fn from_xmp_value(v: i32) -> XmpRating {
    if v < 0 { XmpRating::Rejected } else { XmpRating::Stars(v.min(5) as u8) }
}

pub fn sidecar_mtime_ns(file: &Path) -> Option<i64> {
    fs::metadata(sidecar_path(file)).ok().map(|m| mtime_ns(&m))
}

/// Reads the rating from `file`'s sidecar. `Ok(None)`: no sidecar or no rating in it.
pub fn read_rating(file: &Path) -> Result<Option<XmpRating>, XmpError> {
    crate::exif::init();
    let sidecar = sidecar_path(file);
    if !sidecar.exists() {
        return Ok(None);
    }
    let meta = rexiv2::Metadata::new_from_path(&sidecar)?;
    if !meta.has_tag(RATING_TAG) {
        return Ok(None);
    }
    let raw = meta.get_tag_string(RATING_TAG)?;
    Ok(raw.trim().parse::<i32>().ok().map(from_xmp_value))
}

/// Writes the rating into `file`'s sidecar, preserving everything else in it.
/// Creates the sidecar if missing or empty; refuses to touch one it cannot parse.
/// Returns the sidecar's mtime (ns) after writing.
pub fn write_rating(file: &Path, rating: u8, rejected: bool) -> Result<i64, XmpError> {
    crate::exif::init();
    let sidecar = sidecar_path(file);
    let needs_packet = match fs::metadata(&sidecar) {
        Ok(m) => m.len() == 0,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
    };
    if needs_packet {
        fs::write(&sidecar, EMPTY_PACKET)?;
    }
    let meta = rexiv2::Metadata::new_from_path(&sidecar)?;
    meta.set_tag_string(RATING_TAG, &to_xmp_value(rating, rejected).to_string())?;
    meta.save_to_file(&sidecar)?;
    Ok(mtime_ns(&fs::metadata(&sidecar)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/darktable.xmp");

    #[test]
    fn sidecar_path_appends_xmp() {
        assert_eq!(sidecar_path(Path::new("/p/IMG_1.CR3")), PathBuf::from("/p/IMG_1.CR3.xmp"));
        assert!(is_sidecar(Path::new("/p/IMG_1.CR3.xmp")));
        assert!(!is_sidecar(Path::new("/p/IMG_1.CR3")));
    }

    #[test]
    fn maps_values() {
        assert_eq!(to_xmp_value(3, false), 3);
        assert_eq!(to_xmp_value(3, true), -1);
        assert_eq!(to_xmp_value(9, false), 5);
        assert_eq!(from_xmp_value(-1), XmpRating::Rejected);
        assert_eq!(from_xmp_value(4), XmpRating::Stars(4));
        assert_eq!(from_xmp_value(7), XmpRating::Stars(5));
    }

    #[test]
    fn missing_sidecar_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_rating(&dir.path().join("a.jpg")).unwrap(), None);
    }

    #[test]
    fn write_creates_sidecar_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_1.CR3");
        let mtime = write_rating(&file, 4, false).unwrap();
        assert_eq!(sidecar_mtime_ns(&file), Some(mtime));
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(4)));
    }

    #[test]
    fn reject_roundtrips_as_minus_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_1.CR3");
        write_rating(&file, 3, true).unwrap();
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Rejected));
        let content = fs::read_to_string(sidecar_path(&file)).unwrap();
        assert!(content.contains("-1"), "{content}");
    }

    #[test]
    fn preserves_darktable_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("IMG_0001.CR3");
        fs::copy(FIXTURE, sidecar_path(&file)).unwrap();
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(2)));

        write_rating(&file, 5, false).unwrap();

        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(5)));
        let meta = rexiv2::Metadata::new_from_path(sidecar_path(&file)).unwrap();
        assert_eq!(meta.get_tag_string("Xmp.darktable.history_end").unwrap(), "1");
        assert_eq!(meta.get_tag_string("Xmp.xmpMM.DerivedFrom").unwrap(), "IMG_0001.CR3");
        let content = fs::read_to_string(sidecar_path(&file)).unwrap();
        assert!(content.contains("exposure"), "history lost:\n{content}");
        assert!(content.contains("gz02eJxjYIAAAAAIAAE="), "params lost:\n{content}");
    }

    #[test]
    fn sidecar_without_rating_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jpg");
        fs::write(sidecar_path(&file), EMPTY_PACKET).unwrap();
        assert_eq!(read_rating(&file).unwrap(), None);
    }

    #[test]
    fn empty_sidecar_is_replaced_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jpg");
        fs::write(sidecar_path(&file), b"").unwrap();
        assert!(read_rating(&file).is_err() || read_rating(&file).unwrap().is_none());
        write_rating(&file, 2, false).unwrap();
        assert_eq!(read_rating(&file).unwrap(), Some(XmpRating::Stars(2)));
    }

    #[test]
    fn garbage_sidecar_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.jpg");
        fs::write(sidecar_path(&file), b"definitely not xmp").unwrap();
        assert!(read_rating(&file).is_err());
        assert!(write_rating(&file, 2, false).is_err());
        assert_eq!(fs::read(sidecar_path(&file)).unwrap(), b"definitely not xmp");
    }
}
```

- [ ] **Step 5: Register modules in `src/lib.rs`**

```rust
pub mod catalog;
pub mod exif;
pub mod index;
pub mod media;
pub mod ui;
pub mod xmp;

#[cfg(test)]
mod testutil;
```

- [ ] **Step 6: Run tests**

Run: `cargo test exif xmp`
Expected: PASS — 5 exif tests, 9 xmp tests.
If `preserves_darktable_content` fails because darktable history is dropped, exiv2 did not know the `darktable` namespace at parse time: check that `exif::init()` runs before `new_from_path` (it does in `read_rating`/`write_rating`) and that `register_xmp_namespace` returned `Ok` (temporarily `.expect()` it). Do not weaken the assertion.
If `rexiv2::Rexiv2Error` has a different name, run `cargo doc -p rexiv2 --open` and use the crate's error type.

- [ ] **Step 7: Commit**

```bash
git add src tests
git commit -m "feat: EXIF helpers and darktable-compatible XMP sidecars

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Folder scan and index reconciliation

**Files:**
- Create: `src/catalog/scan.rs`, `src/catalog/sync.rs`
- Modify: `src/catalog/mod.rs`

**Interfaces:**
- Consumes: `media::*`, `pairing::group_into_items`, `index::FileRecord`, `exif::capture_time`, `xmp::{sidecar_mtime_ns, read_rating, XmpRating}`
- Produces:
  - `catalog::scan::list_media_files(&Path) -> io::Result<Vec<MediaFile>>`
  - `catalog::sync::ScanResult { items: Vec<Item>, upserts: Vec<FileRecord>, removed: Vec<PathBuf> }` (items have `id == 0`)
  - `catalog::sync::scan_folder(&Path, &HashMap<PathBuf, FileRecord>) -> io::Result<ScanResult>` — runs on a background thread
  - `catalog::sync::items_from_records(&HashMap<PathBuf, FileRecord>) -> Vec<Item>` — instant grid from the index (no disk access)
  - `catalog::sync::reconcile_item(&mut Item, &mut HashMap<PathBuf, FileRecord>) -> Vec<PathBuf>`

Rules: unchanged file (same size+mtime) reuses its record; a sidecar newer than `xmp_synced_at` is imported unless the record is dirty; a reject from the sidecar keeps the stored stars; in a pair, the record with (dirty, latest sync) wins, ties → RAW, and the other record is updated + marked dirty.

- [ ] **Step 1: Write `src/catalog/scan.rs` with tests**

```rust
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::media::{mtime_ns, FileKind, MediaFile};

/// Lists supported, non-hidden regular files directly inside `folder` (symlinks followed).
pub fn list_media_files(folder: &Path) -> io::Result<Vec<MediaFile>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(folder)? {
        let Ok(entry) = entry else { continue };
        if entry.file_name().as_bytes().first() == Some(&b'.') {
            continue;
        }
        let path = entry.path();
        let Some(kind) = FileKind::from_path(&path) else { continue };
        let Ok(meta) = fs::metadata(&path) else { continue };
        if !meta.is_file() {
            continue;
        }
        out.push(MediaFile { path, kind, size: meta.len(), mtime_ns: mtime_ns(&meta) });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_supported_visible_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.jpg", "b.CR3", ".hidden.jpg", "notes.txt", "c.png.xmp"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::create_dir(dir.path().join("sub.jpg")).unwrap();
        let mut names: Vec<_> = list_media_files(dir.path())
            .unwrap()
            .into_iter()
            .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jpg", "b.CR3"]);
    }

    #[test]
    fn missing_folder_is_error() {
        assert!(list_media_files(Path::new("/definitely/not/here")).is_err());
    }
}
```

- [ ] **Step 2: Write the failing sync tests** (bottom of `src/catalog/sync.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp;
    use std::fs;

    fn touch(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, content).unwrap();
        p
    }

    fn known_from(result: &ScanResult) -> HashMap<PathBuf, FileRecord> {
        result.upserts.iter().map(|r| (r.path.clone(), r.clone())).collect()
    }

    #[test]
    fn new_folder_scan_creates_records() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.jpg", b"x");
        touch(dir.path(), "b.CR3", b"x");
        touch(dir.path(), "b.JPG", b"x");
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.upserts.len(), 3);
        assert!(r.removed.is_empty());
    }

    #[test]
    fn unchanged_files_are_not_upserted() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let second = scan_folder(dir.path(), &known_from(&first)).unwrap();
        assert!(second.upserts.is_empty());
        assert_eq!(second.items.len(), 1);
    }

    #[test]
    fn modified_file_is_upserted_keeping_rating() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 4;
        fs::write(&a, b"longer content").unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.upserts.len(), 1);
        assert_eq!(second.upserts[0].size, 14);
        assert_eq!(second.upserts[0].rating, 4);
        assert_eq!(second.items[0].rating, 4);
    }

    #[test]
    fn removed_files_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        fs::remove_file(&a).unwrap();
        let second = scan_folder(dir.path(), &known_from(&first)).unwrap();
        assert_eq!(second.removed, vec![a]);
        assert!(second.items.is_empty());
    }

    #[test]
    fn sidecar_newer_than_sync_is_imported() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        xmp::write_rating(&a, 5, false).unwrap();
        let second = scan_folder(dir.path(), &known_from(&first)).unwrap();
        assert_eq!(second.items[0].rating, 5);
        assert_eq!(second.upserts[0].xmp_synced_at, xmp::sidecar_mtime_ns(&a));
    }

    #[test]
    fn dirty_record_ignores_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        let rec = known.get_mut(&a).unwrap();
        rec.rating = 2;
        rec.xmp_dirty = true;
        xmp::write_rating(&a, 5, false).unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.items[0].rating, 2);
    }

    #[test]
    fn rejected_sidecar_keeps_stars() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        known.get_mut(&a).unwrap().rating = 3;
        xmp::write_rating(&a, 0, true).unwrap();
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!((second.items[0].rating, second.items[0].rejected), (3, true));
    }

    #[test]
    fn pair_disagreement_resolved_by_latest_sync() {
        let dir = tempfile::tempdir().unwrap();
        let raw = touch(dir.path(), "b.CR3", b"x");
        let jpg = touch(dir.path(), "b.JPG", b"x");
        let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mut known = known_from(&first);
        {
            let r = known.get_mut(&raw).unwrap();
            r.rating = 2;
            r.xmp_synced_at = Some(10);
        }
        {
            let j = known.get_mut(&jpg).unwrap();
            j.rating = 4;
            j.xmp_synced_at = Some(20);
        }
        let second = scan_folder(dir.path(), &known).unwrap();
        assert_eq!(second.items[0].rating, 4);
        let raw_up = second.upserts.iter().find(|r| r.path == raw).unwrap();
        assert_eq!((raw_up.rating, raw_up.xmp_dirty), (4, true));
    }

    #[test]
    fn sort_time_falls_back_to_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        let mtime_s = crate::media::mtime_ns(&fs::metadata(&a).unwrap()) / 1_000_000_000;
        assert_eq!(r.items[0].sort_time, mtime_s);
    }

    #[test]
    fn capture_time_is_used_for_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        crate::testutil::write_jpeg(&a, 8, 8, [1, 2, 3]);
        crate::testutil::set_exif(&a, "Exif.Photo.DateTimeOriginal", "2026:09:01 10:15:30");
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!(r.items[0].sort_time, 1_788_257_730);
    }

    #[test]
    fn malformed_sidecar_does_not_break_scan() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "a.jpg", b"x");
        fs::write(xmp::sidecar_path(&a), b"garbage").unwrap();
        let r = scan_folder(dir.path(), &HashMap::new()).unwrap();
        assert_eq!((r.items[0].rating, r.items[0].rejected), (0, false));
    }

    #[test]
    fn items_from_records_builds_pairs() {
        let mut records = HashMap::new();
        for p in ["/p/b.CR3", "/p/b.JPG", "/p/c.png"] {
            let mut r = FileRecord::new(PathBuf::from(p), 1, 5_000_000_000);
            r.rating = 3;
            records.insert(r.path.clone(), r);
        }
        let items = items_from_records(&records);
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|i| i.is_pair() && i.rating == 3));
        assert!(items.iter().all(|i| i.sort_time == 5));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test catalog::sync`
Expected: FAIL to compile (`scan_folder` not defined).

- [ ] **Step 4: Implement (top of `src/catalog/sync.rs`)**

```rust
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::catalog::pairing::group_into_items;
use crate::catalog::scan::list_media_files;
use crate::exif;
use crate::index::FileRecord;
use crate::media::{FileKind, Item, MediaFile};
use crate::xmp::{self, XmpRating};

#[derive(Debug, Default)]
pub struct ScanResult {
    /// Items with `id == 0`; ids are assigned on the main thread.
    pub items: Vec<Item>,
    /// Records that are new or changed and must be written to the index.
    pub upserts: Vec<FileRecord>,
    /// Paths known to the index that no longer exist.
    pub removed: Vec<PathBuf>,
}

pub fn scan_folder(folder: &Path, known: &HashMap<PathBuf, FileRecord>) -> io::Result<ScanResult> {
    let files = list_media_files(folder)?;
    let mut records: HashMap<PathBuf, FileRecord> = HashMap::with_capacity(files.len());
    let mut changed: HashSet<PathBuf> = HashSet::new();

    for f in &files {
        let mut rec = match known.get(&f.path) {
            Some(old) if old.size == f.size && old.mtime_ns == f.mtime_ns => old.clone(),
            Some(old) => {
                changed.insert(f.path.clone());
                FileRecord {
                    size: f.size,
                    mtime_ns: f.mtime_ns,
                    capture_time: exif::capture_time(&f.path),
                    ..old.clone()
                }
            }
            None => {
                changed.insert(f.path.clone());
                FileRecord {
                    capture_time: exif::capture_time(&f.path),
                    ..FileRecord::new(f.path.clone(), f.size, f.mtime_ns)
                }
            }
        };
        if import_sidecar(&mut rec) {
            changed.insert(f.path.clone());
        }
        records.insert(f.path.clone(), rec);
    }

    let mut items = group_into_items(files);
    for item in &mut items {
        changed.extend(reconcile_item(item, &mut records));
    }
    let removed = known
        .keys()
        .filter(|p| !records.contains_key(*p))
        .cloned()
        .collect();
    let upserts = changed.into_iter().filter_map(|p| records.remove(&p)).collect();
    Ok(ScanResult { items, upserts, removed })
}

/// Imports the sidecar's rating when it changed since the last sync.
/// Returns true if `rec` was modified.
fn import_sidecar(rec: &mut FileRecord) -> bool {
    if rec.xmp_dirty {
        return false; // a local change is waiting to be written: local wins
    }
    let Some(mtime) = xmp::sidecar_mtime_ns(&rec.path) else { return false };
    if rec.xmp_synced_at.is_some_and(|t| t >= mtime) {
        return false;
    }
    rec.xmp_synced_at = Some(mtime);
    match xmp::read_rating(&rec.path) {
        Ok(Some(XmpRating::Stars(n))) => {
            rec.rating = n;
            rec.rejected = false;
        }
        Ok(Some(XmpRating::Rejected)) => rec.rejected = true,
        Ok(None) | Err(_) => {}
    }
    true
}

/// Fills the item's rating, reject flag and sort time from its records and makes
/// the records of a pair agree. Returns paths whose records were changed.
pub fn reconcile_item(item: &mut Item, records: &mut HashMap<PathBuf, FileRecord>) -> Vec<PathBuf> {
    let mut best: Option<(bool, i64, u8, bool)> = None;
    for f in &item.files {
        if let Some(r) = records.get(&f.path) {
            let key = (r.xmp_dirty, r.xmp_synced_at.unwrap_or(i64::MIN));
            if best.is_none_or(|(d, t, _, _)| key > (d, t)) {
                best = Some((key.0, key.1, r.rating, r.rejected));
            }
        }
    }
    let (rating, rejected) = best.map(|(_, _, r, j)| (r, j)).unwrap_or((0, false));
    item.rating = rating;
    item.rejected = rejected;

    let mut changed = Vec::new();
    for f in &item.files {
        if let Some(rec) = records.get_mut(&f.path) {
            if rec.rating != rating || rec.rejected != rejected {
                rec.rating = rating;
                rec.rejected = rejected;
                rec.xmp_dirty = true;
                changed.push(f.path.clone());
            }
        }
    }

    item.sort_time = item
        .files
        .iter()
        .filter_map(|f| records.get(&f.path).and_then(|r| r.capture_time))
        .min()
        .unwrap_or(item.display_file().mtime_ns / 1_000_000_000);
    changed
}

/// Builds items from index records alone (no disk access) for an instant first paint.
pub fn items_from_records(records: &HashMap<PathBuf, FileRecord>) -> Vec<Item> {
    let files: Vec<MediaFile> = records
        .values()
        .filter_map(|r| {
            Some(MediaFile {
                path: r.path.clone(),
                kind: FileKind::from_path(&r.path)?,
                size: r.size,
                mtime_ns: r.mtime_ns,
            })
        })
        .collect();
    let mut items = group_into_items(files);
    let mut scratch = records.clone();
    for item in &mut items {
        reconcile_item(item, &mut scratch);
    }
    items
}
```

`Option::is_none_or` is stable since Rust 1.82.

Update `src/catalog/mod.rs`:

```rust
pub mod pairing;
pub mod scan;
pub mod sync;
```

- [ ] **Step 5: Run tests**

Run: `cargo test catalog`
Expected: PASS — 8 pairing + 2 scan + 12 sync tests.

- [ ] **Step 6: Commit**

```bash
git add src
git commit -m "feat: folder scan with index reconciliation and sidecar import

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6: Rating changes, item diff, filtering and sorting

**Files:**
- Create: `src/catalog/rating.rs`, `src/catalog/diff.rs`, `src/filter.rs`
- Modify: `src/catalog/mod.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: `media::{Item, ItemId, FileKind}`
- Produces:
  - `catalog::rating::RatingChange { Set(u8), ToggleReject }`, `catalog::rating::apply(&mut Item, RatingChange)` — setting stars clears the reject flag; toggling reject keeps the stars.
  - `catalog::diff::ItemUpdate { item: Item, display_changed: bool }`, `catalog::diff::ItemDiff { removed: Vec<ItemId>, added: Vec<Item>, updated: Vec<ItemUpdate> }`, `catalog::diff::diff_items(&[Item], Vec<Item>, impl FnMut() -> ItemId) -> ItemDiff` — items are matched by their exact file-path list; matched items keep the old id; unmatched new items get `alloc_id()`.
  - `filter::{RejectFilter, TypeFilter, SortKey}` (defaults: `HideRejects`, `All`, `CaptureTime`), `filter::FilterState { min_rating: u8, reject, kind, pinned: HashSet<ItemId> }` + `matches(&Item) -> bool`, `filter::SortState { key, descending }`, `filter::compare(&Item, &Item, SortState) -> Ordering`.

- [ ] **Step 1: Write `src/catalog/rating.rs` with tests**

```rust
use crate::media::Item;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatingChange {
    /// 0 clears the rating. Setting stars always un-rejects (XMP has one field for both).
    Set(u8),
    ToggleReject,
}

pub fn apply(item: &mut Item, change: RatingChange) {
    match change {
        RatingChange::Set(n) => {
            item.rating = n.min(5);
            item.rejected = false;
        }
        RatingChange::ToggleReject => item.rejected = !item.rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{FileKind, MediaFile};
    use std::path::PathBuf;

    fn item() -> Item {
        Item::new(vec![MediaFile {
            path: PathBuf::from("/p/a.jpg"),
            kind: FileKind::Jpeg,
            size: 1,
            mtime_ns: 0,
        }])
    }

    #[test]
    fn set_rating_clamps_and_unrejects() {
        let mut i = item();
        i.rejected = true;
        apply(&mut i, RatingChange::Set(9));
        assert_eq!((i.rating, i.rejected), (5, false));
    }

    #[test]
    fn toggle_reject_keeps_stars() {
        let mut i = item();
        apply(&mut i, RatingChange::Set(3));
        apply(&mut i, RatingChange::ToggleReject);
        assert_eq!((i.rating, i.rejected), (3, true));
        apply(&mut i, RatingChange::ToggleReject);
        assert_eq!((i.rating, i.rejected), (3, false));
    }
}
```

- [ ] **Step 2: Write `src/catalog/diff.rs` with tests**

```rust
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::media::{Item, ItemId};

#[derive(Debug)]
pub struct ItemUpdate {
    /// The rescanned item carrying its existing id.
    pub item: Item,
    /// The display file's size or mtime changed: the thumbnail must be reloaded.
    pub display_changed: bool,
}

#[derive(Debug, Default)]
pub struct ItemDiff {
    pub removed: Vec<ItemId>,
    pub added: Vec<Item>,
    pub updated: Vec<ItemUpdate>,
}

fn key(item: &Item) -> Vec<PathBuf> {
    item.files.iter().map(|f| f.path.clone()).collect()
}

pub fn diff_items(old: &[Item], new: Vec<Item>, mut alloc_id: impl FnMut() -> ItemId) -> ItemDiff {
    let old_by_key: HashMap<Vec<PathBuf>, &Item> = old.iter().map(|i| (key(i), i)).collect();
    let mut seen = HashSet::new();
    let mut diff = ItemDiff::default();
    for mut item in new {
        match old_by_key.get(&key(&item)) {
            Some(old_item) => {
                seen.insert(old_item.id);
                item.id = old_item.id;
                if item != **old_item {
                    let display_changed = item.display_file() != old_item.display_file();
                    diff.updated.push(ItemUpdate { item, display_changed });
                }
            }
            None => {
                item.id = alloc_id();
                diff.added.push(item);
            }
        }
    }
    diff.removed = old.iter().filter(|i| !seen.contains(&i.id)).map(|i| i.id).collect();
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::pairing::group_into_items;
    use crate::media::{FileKind, MediaFile};
    use std::path::Path;

    fn mf(p: &str, mtime_ns: i64) -> MediaFile {
        MediaFile {
            path: PathBuf::from(p),
            kind: FileKind::from_path(Path::new(p)).unwrap(),
            size: 1,
            mtime_ns,
        }
    }

    fn with_ids(mut items: Vec<Item>) -> Vec<Item> {
        for (n, i) in items.iter_mut().enumerate() {
            i.id = n as ItemId + 1;
        }
        items
    }

    fn counter() -> impl FnMut() -> ItemId {
        let mut next = 100;
        move || {
            next += 1;
            next
        }
    }

    #[test]
    fn unchanged_items_produce_empty_diff() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let d = diff_items(&old, group_into_items(vec![mf("/p/a.jpg", 0)]), counter());
        assert!(d.added.is_empty() && d.removed.is_empty() && d.updated.is_empty());
    }

    #[test]
    fn new_file_is_added_with_fresh_id() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let new = group_into_items(vec![mf("/p/a.jpg", 0), mf("/p/b.jpg", 0)]);
        let d = diff_items(&old, new, counter());
        assert_eq!(d.added.len(), 1);
        assert_eq!(d.added[0].id, 101);
    }

    #[test]
    fn missing_file_is_removed() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0), mf("/p/b.jpg", 0)]));
        let d = diff_items(&old, group_into_items(vec![mf("/p/a.jpg", 0)]), counter());
        assert_eq!(d.removed, vec![2]);
    }

    #[test]
    fn rating_change_is_update_without_display_change() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let mut new = group_into_items(vec![mf("/p/a.jpg", 0)]);
        new[0].rating = 4;
        let d = diff_items(&old, new, counter());
        assert_eq!(d.updated.len(), 1);
        assert_eq!(d.updated[0].item.id, 1);
        assert!(!d.updated[0].display_changed);
    }

    #[test]
    fn mtime_change_marks_display_changed() {
        let old = with_ids(group_into_items(vec![mf("/p/a.jpg", 0)]));
        let d = diff_items(&old, group_into_items(vec![mf("/p/a.jpg", 7)]), counter());
        assert!(d.updated[0].display_changed);
    }

    #[test]
    fn single_becoming_pair_is_remove_plus_add() {
        let old = with_ids(group_into_items(vec![mf("/p/a.CR3", 0)]));
        let new = group_into_items(vec![mf("/p/a.CR3", 0), mf("/p/a.JPG", 0)]);
        let d = diff_items(&old, new, counter());
        assert_eq!(d.removed, vec![1]);
        assert_eq!(d.added.len(), 1);
        assert!(d.added[0].is_pair());
    }
}
```

- [ ] **Step 3: Write `src/filter.rs` with tests**

```rust
use std::cmp::Ordering;
use std::collections::HashSet;

use crate::media::{FileKind, Item, ItemId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RejectFilter {
    ShowAll,
    #[default]
    HideRejects,
    OnlyRejects,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeFilter {
    #[default]
    All,
    /// Items containing a RAW file.
    Raw,
    /// Items containing a JPEG file.
    Jpeg,
    /// PNG and TIFF.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    CaptureTime,
    FileName,
}

#[derive(Debug, Clone, Default)]
pub struct FilterState {
    /// 0 = show all ratings. Ignored for `OnlyRejects` and for rejected items.
    pub min_rating: u8,
    pub reject: RejectFilter,
    pub kind: TypeFilter,
    /// Items re-rated since the user last navigated: always shown.
    pub pinned: HashSet<ItemId>,
}

impl FilterState {
    pub fn matches(&self, item: &Item) -> bool {
        if self.pinned.contains(&item.id) {
            return true;
        }
        let has = |k: FileKind| item.files.iter().any(|f| f.kind == k);
        let kind_ok = match self.kind {
            TypeFilter::All => true,
            TypeFilter::Raw => has(FileKind::Raw),
            TypeFilter::Jpeg => has(FileKind::Jpeg),
            TypeFilter::Other => has(FileKind::Png) || has(FileKind::Tiff),
        };
        if !kind_ok {
            return false;
        }
        match self.reject {
            RejectFilter::OnlyRejects => item.rejected,
            RejectFilter::HideRejects => !item.rejected && item.rating >= self.min_rating,
            RejectFilter::ShowAll => item.rejected || item.rating >= self.min_rating,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortState {
    pub key: SortKey,
    pub descending: bool,
}

/// Total order: key, then file name, then id. `descending` reverses the whole order.
pub fn compare(a: &Item, b: &Item, sort: SortState) -> Ordering {
    let by_name = || a.sort_name.cmp(&b.sort_name);
    let ord = match sort.key {
        SortKey::CaptureTime => a.sort_time.cmp(&b.sort_time).then_with(by_name),
        SortKey::FileName => by_name(),
    }
    .then(a.id.cmp(&b.id));
    if sort.descending { ord.reverse() } else { ord }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::pairing::group_into_items;
    use crate::media::MediaFile;
    use std::path::{Path, PathBuf};

    fn item(paths: &[&str], rating: u8, rejected: bool) -> Item {
        let files = paths
            .iter()
            .map(|p| MediaFile {
                path: PathBuf::from(p),
                kind: FileKind::from_path(Path::new(p)).unwrap(),
                size: 1,
                mtime_ns: 0,
            })
            .collect();
        let mut i = group_into_items(files).remove(0);
        i.rating = rating;
        i.rejected = rejected;
        i
    }

    #[test]
    fn default_hides_rejects() {
        let f = FilterState::default();
        assert!(f.matches(&item(&["/p/a.jpg"], 0, false)));
        assert!(!f.matches(&item(&["/p/a.jpg"], 5, true)));
    }

    #[test]
    fn min_rating_filters() {
        let f = FilterState { min_rating: 3, ..Default::default() };
        assert!(!f.matches(&item(&["/p/a.jpg"], 2, false)));
        assert!(f.matches(&item(&["/p/a.jpg"], 3, false)));
    }

    #[test]
    fn only_rejects_ignores_min_rating() {
        let f = FilterState { min_rating: 5, reject: RejectFilter::OnlyRejects, ..Default::default() };
        assert!(f.matches(&item(&["/p/a.jpg"], 0, true)));
        assert!(!f.matches(&item(&["/p/a.jpg"], 5, false)));
    }

    #[test]
    fn show_all_includes_rejects() {
        let f = FilterState { min_rating: 4, reject: RejectFilter::ShowAll, ..Default::default() };
        assert!(f.matches(&item(&["/p/a.jpg"], 0, true)));
        assert!(!f.matches(&item(&["/p/a.jpg"], 1, false)));
    }

    #[test]
    fn type_filters_match_pairs_by_member() {
        let pair = item(&["/p/a.CR3", "/p/a.JPG"], 0, false);
        let png = item(&["/p/b.png"], 0, false);
        let raw = FilterState { kind: TypeFilter::Raw, ..Default::default() };
        let jpeg = FilterState { kind: TypeFilter::Jpeg, ..Default::default() };
        let other = FilterState { kind: TypeFilter::Other, ..Default::default() };
        assert!(raw.matches(&pair) && jpeg.matches(&pair) && !other.matches(&pair));
        assert!(!raw.matches(&png) && !jpeg.matches(&png) && other.matches(&png));
    }

    #[test]
    fn pinned_items_bypass_filters() {
        let mut i = item(&["/p/a.jpg"], 1, true);
        i.id = 7;
        let mut f = FilterState { min_rating: 5, ..Default::default() };
        assert!(!f.matches(&i));
        f.pinned.insert(7);
        assert!(f.matches(&i));
    }

    #[test]
    fn sorts_by_capture_time_then_name() {
        let mut a = item(&["/p/b.jpg"], 0, false);
        let mut b = item(&["/p/a.jpg"], 0, false);
        a.sort_time = 10;
        b.sort_time = 10;
        let s = SortState::default();
        assert_eq!(compare(&a, &b, s), Ordering::Greater); // same time → a.jpg first
        b.sort_time = 20;
        assert_eq!(compare(&a, &b, s), Ordering::Less);
    }

    #[test]
    fn filename_sort_is_case_insensitive_and_reversible() {
        let a = item(&["/p/B.jpg"], 0, false);
        let b = item(&["/p/a.jpg"], 0, false);
        let asc = SortState { key: SortKey::FileName, descending: false };
        let desc = SortState { key: SortKey::FileName, descending: true };
        assert_eq!(compare(&a, &b, asc), Ordering::Greater);
        assert_eq!(compare(&a, &b, desc), Ordering::Less);
    }
}
```

- [ ] **Step 4: Register modules**

`src/catalog/mod.rs`:

```rust
pub mod diff;
pub mod pairing;
pub mod rating;
pub mod scan;
pub mod sync;
```

`src/lib.rs`: add `pub mod filter;` (keep the list alphabetical).

- [ ] **Step 5: Run tests**

Run: `cargo test rating diff filter`
Expected: PASS — 2 rating + 6 diff + 8 filter tests.

- [ ] **Step 6: Commit**

```bash
git add src
git commit -m "feat: rating changes, rescan diff, filtering and sorting

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Priority job pool

**Files:**
- Create: `src/jobs.rs`
- Modify: `src/lib.rs` (add `pub mod jobs;`)

**Interfaces:**
- Consumes: nothing
- Produces:
  - `jobs::Priority { Background, Prefetch, Visible, Current }` (ordered low → high)
  - `jobs::CancelToken` (`Clone + Default + Send + Sync`): `new()`, `cancel()`, `is_cancelled()`
  - `jobs::JobPool`: `new(threads: usize)`, `default_threads() -> usize`, `submit(&self, Priority, CancelToken, impl FnOnce() + Send + 'static)`, `queued(&self) -> usize`, `clear(&self)` (drops queued jobs). Jobs of equal priority run FIFO. A panicking job does not kill its worker. Dropping the pool joins the workers.

- [ ] **Step 1: Write the failing tests** (bottom of `src/jobs.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Occupies the single worker until the returned sender is used.
    fn block_worker(pool: &JobPool) -> mpsc::Sender<()> {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (started_tx, started_rx) = mpsc::channel::<()>();
        pool.submit(Priority::Current, CancelToken::new(), move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release_tx
    }

    #[test]
    fn runs_higher_priority_first_and_fifo_within_priority() {
        let pool = JobPool::new(1);
        let release = block_worker(&pool);
        let (tx, rx) = mpsc::channel();
        for (p, name) in [
            (Priority::Background, "bg"),
            (Priority::Visible, "vis1"),
            (Priority::Prefetch, "pre"),
            (Priority::Visible, "vis2"),
            (Priority::Current, "cur"),
        ] {
            let tx = tx.clone();
            pool.submit(p, CancelToken::new(), move || tx.send(name).unwrap());
        }
        release.send(()).unwrap();
        let order: Vec<_> = (0..5).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(order, ["cur", "vis1", "vis2", "pre", "bg"]);
    }

    #[test]
    fn cancelled_jobs_do_not_run() {
        let pool = JobPool::new(1);
        let release = block_worker(&pool);
        let (tx, rx) = mpsc::channel();
        let token = CancelToken::new();
        let tx1 = tx.clone();
        pool.submit(Priority::Visible, token.clone(), move || tx1.send("cancelled").unwrap());
        pool.submit(Priority::Background, CancelToken::new(), move || tx.send("kept").unwrap());
        token.cancel();
        release.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "kept");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn panicking_job_does_not_kill_worker() {
        let pool = JobPool::new(1);
        let (tx, rx) = mpsc::channel();
        pool.submit(Priority::Visible, CancelToken::new(), || panic!("boom"));
        pool.submit(Priority::Background, CancelToken::new(), move || tx.send(1).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    }

    #[test]
    fn clear_drops_queued_jobs() {
        let pool = JobPool::new(1);
        let release = block_worker(&pool);
        let (tx, rx) = mpsc::channel::<()>();
        pool.submit(Priority::Visible, CancelToken::new(), move || tx.send(()).unwrap());
        assert_eq!(pool.queued(), 1);
        pool.clear();
        assert_eq!(pool.queued(), 0);
        release.send(()).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn default_threads_at_least_one() {
        assert!(JobPool::default_threads() >= 1);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test jobs`
Expected: FAIL to compile.

- [ ] **Step 3: Implement (top of `src/jobs.rs`)**

```rust
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Background,
    Prefetch,
    Visible,
    Current,
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, AtomicOrdering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(AtomicOrdering::Relaxed)
    }
}

type Work = Box<dyn FnOnce() + Send + 'static>;

struct Job {
    priority: Priority,
    seq: u64,
    token: CancelToken,
    work: Work,
}

impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}

impl Eq for Job {}

impl Ord for Job {
    /// Max-heap: higher priority first; within a priority, lower seq (older) first.
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Job {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct Queue {
    heap: BinaryHeap<Job>,
    next_seq: u64,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

pub struct JobPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl JobPool {
    pub fn new(threads: usize) -> JobPool {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { heap: BinaryHeap::new(), next_seq: 0, shutdown: false }),
            ready: Condvar::new(),
        });
        let workers = (0..threads.max(1))
            .map(|i| {
                let shared = shared.clone();
                std::thread::Builder::new()
                    .name(format!("pb-worker-{i}"))
                    .spawn(move || worker_loop(&shared))
                    .expect("failed to spawn worker thread")
            })
            .collect();
        JobPool { shared, workers }
    }

    /// One core is left for the GTK main thread.
    pub fn default_threads() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1)
    }

    pub fn submit(&self, priority: Priority, token: CancelToken, work: impl FnOnce() + Send + 'static) {
        let mut q = self.shared.queue.lock().unwrap();
        let seq = q.next_seq;
        q.next_seq += 1;
        q.heap.push(Job { priority, seq, token, work: Box::new(work) });
        drop(q);
        self.shared.ready.notify_one();
    }

    pub fn queued(&self) -> usize {
        self.shared.queue.lock().unwrap().heap.len()
    }

    pub fn clear(&self) {
        self.shared.queue.lock().unwrap().heap.clear();
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(job) = q.heap.pop() {
                    break job;
                }
                q = shared.ready.wait(q).unwrap();
            }
        };
        if job.token.is_cancelled() {
            continue;
        }
        let _ = catch_unwind(AssertUnwindSafe(job.work));
    }
}

impl Drop for JobPool {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().shutdown = true;
        self.shared.ready.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
```

Add `pub mod jobs;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test jobs`
Expected: PASS — 5 tests (the panic test prints a panic message to stderr; that is expected).

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat: priority job pool with cancellation

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Pixel utilities and JPEG/PNG/TIFF decoding

**Files:**
- Create: `src/decode/mod.rs`, `src/decode/pixels.rs`, `src/decode/jpeg.rs`, `src/decode/generic.rs`
- Modify: `src/lib.rs` (add `pub mod decode;`)

**Interfaces:**
- Consumes: `media::{MediaFile, FileKind, Item}`, `exif::{init, orientation}`
- Produces:
  - `decode::Rgba { width: u32, height: u32, data: Vec<u8> }` (RGBA8, stride = width×4) with `new`, `pixel(x, y) -> [u8; 4]`, `long_edge()`
  - `decode::Quality { Fast, High }`
  - `decode::pixels::{fit_within(u32, u32, u32) -> (u32, u32), resize_to_fit(Rgba, u32, Quality) -> Rgba, apply_orientation(Rgba, u8) -> Rgba}` — `max_edge == 0` means "no limit"; never upscales.
  - `decode::jpeg::{decode_jpeg(&[u8], target_edge: u32) -> Result<Rgba, DecodeError>, dimensions(&[u8]) -> Option<(u32, u32)>}` — decodes at the smallest DCT scale whose long edge is still ≥ `target_edge`.
  - `decode::DecodeError { Io, Jpeg(String), Image(String), Raw(String), Panic }`
  - `decode::THUMB_EDGE: u32 = 256`
  - `decode::decode_for_display(&MediaFile, max_edge: u32, Quality) -> Result<Rgba, DecodeError>` — result long edge ≤ `max_edge` (unless 0), EXIF orientation applied.
  - `decode::guarded(impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError>` — converts panics to `DecodeError::Panic`.

- [ ] **Step 1: Write `src/decode/pixels.rs` with tests**

```rust
use fast_image_resize::images::Image as FrImage;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Rgba {
        assert_eq!(data.len(), width as usize * height as usize * 4, "RGBA buffer size");
        Rgba { width, height, data }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }

    pub fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    /// Bilinear: thumbnails.
    Fast,
    /// Lanczos3: full view.
    High,
}

/// Scales (width, height) so the long edge is at most `max_edge`; 0 = unlimited; never upscales.
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if max_edge == 0 || width.max(height) <= max_edge {
        return (width, height);
    }
    let scale = |short: u32, long: u32| ((short as u64 * max_edge as u64) / long as u64).max(1) as u32;
    if width >= height {
        (max_edge, scale(height, width))
    } else {
        (scale(width, height), max_edge)
    }
}

pub fn resize_to_fit(img: Rgba, max_edge: u32, quality: Quality) -> Rgba {
    let (w, h) = fit_within(img.width, img.height, max_edge);
    if (w, h) == (img.width, img.height) {
        return img;
    }
    let src = FrImage::from_vec_u8(img.width, img.height, img.data, PixelType::U8x4)
        .expect("buffer matches dimensions");
    let mut dst = FrImage::new(w, h, PixelType::U8x4);
    let filter = match quality {
        Quality::Fast => FilterType::Bilinear,
        Quality::High => FilterType::Lanczos3,
    };
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(filter))
        .use_alpha(false);
    Resizer::new()
        .resize(&src, &mut dst, &options)
        .expect("same pixel type");
    Rgba::new(w, h, dst.into_vec())
}

/// Applies EXIF orientation (1–8) so the result is upright.
pub fn apply_orientation(img: Rgba, orientation: u8) -> Rgba {
    if !(2..=8).contains(&orientation) {
        return img;
    }
    let (w, h) = (img.width as usize, img.height as usize);
    let (dw, dh) = if orientation >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; img.data.len()];
    for dy in 0..dh {
        for dx in 0..dw {
            let (sx, sy) = match orientation {
                2 => (w - 1 - dx, dy),
                3 => (w - 1 - dx, h - 1 - dy),
                4 => (dx, h - 1 - dy),
                5 => (dy, dx),
                6 => (dy, h - 1 - dx),
                7 => (w - 1 - dy, h - 1 - dx),
                8 => (w - 1 - dy, dx),
                _ => unreachable!(),
            };
            let s = (sy * w + sx) * 4;
            let d = (dy * dw + dx) * 4;
            out[d..d + 4].copy_from_slice(&img.data[s..s + 4]);
        }
    }
    Rgba::new(dw as u32, dh as u32, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    /// 2×1 image: red at x=0, green at x=1.
    fn red_green() -> Rgba {
        Rgba::new(2, 1, [RED, GREEN].concat())
    }

    #[test]
    fn fit_within_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_within(6000, 4000, 256), (256, 170));
        assert_eq!(fit_within(4000, 6000, 256), (170, 256));
        assert_eq!(fit_within(100, 50, 256), (100, 50));
        assert_eq!(fit_within(6000, 4000, 0), (6000, 4000));
        assert_eq!(fit_within(10000, 1, 100), (100, 1));
    }

    #[test]
    fn resize_produces_requested_size() {
        let img = Rgba::new(400, 200, vec![128; 400 * 200 * 4]);
        let out = resize_to_fit(img, 100, Quality::High);
        assert_eq!((out.width, out.height), (100, 50));
        assert_eq!(out.data.len(), 100 * 50 * 4);
    }

    #[test]
    fn orientation_1_is_identity() {
        assert_eq!(apply_orientation(red_green(), 1), red_green());
    }

    #[test]
    fn orientation_3_rotates_180() {
        let out = apply_orientation(red_green(), 3);
        assert_eq!((out.pixel(0, 0), out.pixel(1, 0)), (GREEN, RED));
    }

    #[test]
    fn orientation_6_rotates_clockwise() {
        let out = apply_orientation(red_green(), 6);
        assert_eq!((out.width, out.height), (1, 2));
        assert_eq!((out.pixel(0, 0), out.pixel(0, 1)), (RED, GREEN));
    }

    #[test]
    fn orientation_8_rotates_counter_clockwise() {
        let out = apply_orientation(red_green(), 8);
        assert_eq!((out.width, out.height), (1, 2));
        assert_eq!((out.pixel(0, 0), out.pixel(0, 1)), (GREEN, RED));
    }

    #[test]
    fn orientations_5_and_7_swap_dimensions() {
        let img = Rgba::new(3, 2, vec![0; 24]);
        for o in [5, 7] {
            let out = apply_orientation(img.clone(), o);
            assert_eq!((out.width, out.height), (2, 3));
        }
    }
}
```

- [ ] **Step 2: Write `src/decode/jpeg.rs` with tests**

```rust
use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor};

use super::{DecodeError, Rgba};

fn jerr(e: turbojpeg::Error) -> DecodeError {
    DecodeError::Jpeg(e.to_string())
}

/// Image dimensions from the JPEG header, without decoding.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let header = Decompressor::new().ok()?.read_header(data).ok()?;
    Some((header.width as u32, header.height as u32))
}

/// Smallest supported DCT downscale whose long edge still covers `target_edge` (0 = full size).
pub fn choose_scaling(width: usize, height: usize, target_edge: u32) -> ScalingFactor {
    let one = ScalingFactor::new(1, 1);
    if target_edge == 0 {
        return one;
    }
    let long = width.max(height);
    Decompressor::supported_scaling_factors()
        .into_iter()
        .filter(|f| f.num() <= f.denom())
        .filter(|f| f.scale(long) >= target_edge as usize)
        .min_by_key(|f| f.scale(long))
        .unwrap_or(one)
}

pub fn decode_jpeg(data: &[u8], target_edge: u32) -> Result<Rgba, DecodeError> {
    let mut d = Decompressor::new().map_err(jerr)?;
    let header = d.read_header(data).map_err(jerr)?;
    let factor = choose_scaling(header.width, header.height, target_edge);
    d.set_scaling_factor(factor).map_err(jerr)?;
    let scaled = header.scaled(factor);
    let (w, h) = (scaled.width, scaled.height);
    let mut pixels = vec![0u8; w * h * 4];
    d.decompress(
        data,
        Image { pixels: pixels.as_mut_slice(), width: w, pitch: w * 4, height: h, format: PixelFormat::RGBA },
    )
    .map_err(jerr)?;
    Ok(Rgba::new(w as u32, h as u32, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::write_jpeg;

    fn sample(w: u32, h: u32) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jpg");
        write_jpeg(&path, w, h, [200, 100, 50]);
        std::fs::read(path).unwrap()
    }

    #[test]
    fn decodes_full_size() {
        let img = decode_jpeg(&sample(800, 600), 0).unwrap();
        assert_eq!((img.width, img.height), (800, 600));
        let [r, g, b, a] = img.pixel(400, 300);
        assert!(r > 180 && g > 80 && g < 120 && b < 70 && a == 255);
    }

    #[test]
    fn decodes_at_reduced_scale() {
        let img = decode_jpeg(&sample(800, 600), 100).unwrap();
        assert!(img.long_edge() >= 100 && img.long_edge() < 200, "{}x{}", img.width, img.height);
    }

    #[test]
    fn reads_dimensions() {
        assert_eq!(dimensions(&sample(320, 240)), Some((320, 240)));
        assert_eq!(dimensions(b"nope"), None);
    }

    #[test]
    fn garbage_jpeg_is_error() {
        assert!(decode_jpeg(b"definitely not a jpeg", 0).is_err());
    }

    #[test]
    fn truncated_jpeg_does_not_panic() {
        let data = sample(800, 600);
        let _ = decode_jpeg(&data[..data.len() / 3], 256); // Ok (partial) or Err, never a panic
    }
}
```

If `ScalingFactor::num/denom/scale` or `DecompressHeader::scaled` have different names in the installed turbojpeg version, run `cargo doc -p turbojpeg --open` and use the equivalents; keep the same behaviour.

- [ ] **Step 3: Write `src/decode/generic.rs`**

```rust
use std::path::Path;

use super::{DecodeError, Rgba};

/// PNG and TIFF (8/16-bit, any channel layout) → RGBA8.
pub fn decode_with_image_crate(path: &Path) -> Result<Rgba, DecodeError> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .map_err(|e| DecodeError::Image(e.to_string()))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Ok(Rgba::new(w, h, img.into_raw()))
}
```

- [ ] **Step 4: Write `src/decode/mod.rs` with tests**

```rust
pub mod generic;
pub mod jpeg;
pub mod pixels;

use std::fs;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use crate::media::{FileKind, MediaFile};
pub use pixels::{Quality, Rgba};

/// Long edge of cached thumbnails, in pixels.
pub const THUMB_EDGE: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("JPEG: {0}")]
    Jpeg(String),
    #[error("image: {0}")]
    Image(String),
    #[error("RAW: {0}")]
    Raw(String),
    #[error("decoder crashed")]
    Panic,
}

/// Decodes `file` for display: long edge ≤ `max_edge` (0 = native), upright.
pub fn decode_for_display(file: &MediaFile, max_edge: u32, quality: Quality) -> Result<Rgba, DecodeError> {
    let (img, orientation) = match file.kind {
        FileKind::Jpeg => {
            let data = fs::read(&file.path)?;
            (jpeg::decode_jpeg(&data, max_edge)?, orientation_of_buffer(&data))
        }
        FileKind::Png | FileKind::Tiff => (
            generic::decode_with_image_crate(&file.path)?,
            orientation_of_path(&file.path),
        ),
        FileKind::Raw => {
            return Err(DecodeError::Raw("RAW decoding is added in Task 9".into()));
        }
    };
    let img = pixels::resize_to_fit(img, max_edge, quality);
    Ok(pixels::apply_orientation(img, orientation))
}

fn orientation_of_buffer(data: &[u8]) -> u8 {
    crate::exif::init();
    rexiv2::Metadata::new_from_buffer(data)
        .map(|m| crate::exif::orientation(&m))
        .unwrap_or(1)
}

fn orientation_of_path(path: &Path) -> u8 {
    crate::exif::init();
    rexiv2::Metadata::new_from_path(path)
        .map(|m| crate::exif::orientation(&m))
        .unwrap_or(1)
}

/// Runs a decoder, turning a panic inside it into `DecodeError::Panic`.
pub fn guarded<T>(f: impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(Err(DecodeError::Panic))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{set_exif, write_png, write_tiff, write_two_tone_jpeg};
    use std::path::PathBuf;

    fn media(path: PathBuf) -> MediaFile {
        let kind = FileKind::from_path(&path).unwrap();
        MediaFile { path, kind, size: 0, mtime_ns: 0 }
    }

    #[test]
    fn jpeg_thumbnail_fits_thumb_edge() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        write_two_tone_jpeg(&p, 1200, 800);
        let img = decode_for_display(&media(p), THUMB_EDGE, Quality::Fast).unwrap();
        assert_eq!((img.width, img.height), (256, 170));
    }

    #[test]
    fn jpeg_orientation_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        write_two_tone_jpeg(&p, 64, 32); // left red, right green
        set_exif(&p, "Exif.Image.Orientation", "6");
        let img = decode_for_display(&media(p), 0, Quality::High).unwrap();
        assert_eq!((img.width, img.height), (32, 64));
        let top = img.pixel(16, 4);
        let bottom = img.pixel(16, 60);
        assert!(top[0] > 200 && top[1] < 80, "top should be red: {top:?}");
        assert!(bottom[1] > 200 && bottom[0] < 80, "bottom should be green: {bottom:?}");
    }

    #[test]
    fn decodes_png_and_tiff() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        let tif = dir.path().join("a.tif");
        write_png(&png, 600, 300, [1, 2, 3, 255]);
        write_tiff(&tif, 300, 600, [4, 5, 6]);
        let a = decode_for_display(&media(png), 200, Quality::Fast).unwrap();
        let b = decode_for_display(&media(tif), 200, Quality::Fast).unwrap();
        assert_eq!((a.width, a.height), (200, 100));
        assert_eq!((b.width, b.height), (100, 200));
        assert_eq!(b.pixel(50, 50), [4, 5, 6, 255]);
    }

    #[test]
    fn corrupt_png_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.png");
        std::fs::write(&p, b"\x89PNG\r\n\x1a\nbroken").unwrap();
        assert!(decode_for_display(&media(p), 256, Quality::Fast).is_err());
    }

    #[test]
    fn zero_byte_jpeg_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty.jpg");
        std::fs::write(&p, b"").unwrap();
        assert!(decode_for_display(&media(p), 256, Quality::Fast).is_err());
    }

    #[test]
    fn guarded_turns_panic_into_error() {
        let r: Result<(), DecodeError> = guarded(|| panic!("decoder bug"));
        assert!(matches!(r, Err(DecodeError::Panic)));
    }
}
```

Add `pub mod decode;` to `src/lib.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test decode`
Expected: PASS — 7 pixels + 5 jpeg + 6 mod tests.
If `fast_image_resize` 6 moved `Image` or `use_alpha`, check `cargo doc -p fast_image_resize --open`; the behaviour (RGBA8 in, RGBA8 out, no alpha premultiplication) must stay.

- [ ] **Step 6: Commit**

```bash
git add src
git commit -m "feat: JPEG/PNG/TIFF decoding with DCT scaling and orientation

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: LibRaw wrapper and RAW decoding

**Files:**
- Create: `build.rs`, `src/libraw/wrapper.h`, `src/libraw/mod.rs`, `src/decode/raw.rs`
- Modify: `src/decode/mod.rs`, `src/lib.rs` (add `pub mod libraw;`)

**Interfaces:**
- Consumes: `decode::{Rgba, DecodeError, jpeg, pixels}`, `exif::{init, orientation}`, `media::Item`
- Produces:
  - `libraw::Raw`: `open(&Path) -> Result<Raw, LibRawError>`, `flip(&self) -> i32`, `thumbnail_jpeg(&mut self) -> Result<Option<Vec<u8>>, LibRawError>`, `decode(self, half_size: bool) -> Result<Rgba, LibRawError>` (already upright)
  - `libraw::flip_to_orientation(i32) -> u8`, `libraw::LibRawError`
  - `decode::raw::decode_raw_preview(&Path, target_edge: u32) -> Result<(Rgba, u8), DecodeError>` (image + EXIF orientation still to apply)
  - `decode::raw::decode_raw_full(&Path) -> Result<Rgba, DecodeError>`
  - `decode::decode_actual_size(&Item) -> Result<Rgba, DecodeError>` — 100% view: RAW of the item if present, else the display file at native size.
  - `impl From<LibRawError> for DecodeError`

Preview order: largest exiv2 JPEG preview → LibRaw embedded thumbnail → LibRaw half-size decode. A preview is accepted when its long edge ≥ `min(target_edge, 1024)` (or ≥ 1024 when `target_edge == 0`).

- [ ] **Step 1: Write `src/libraw/wrapper.h`**

```c
#include <libraw/libraw.h>
```

- [ ] **Step 2: Write `build.rs`**

```rust
use std::env;
use std::path::PathBuf;

fn main() {
    // libraw_r is the thread-safe build; each worker owns its own handle.
    let lib = pkg_config::Config::new()
        .atleast_version("0.21")
        .probe("libraw_r")
        .expect("LibRaw not found: sudo dnf install LibRaw-devel");

    println!("cargo:rerun-if-changed=src/libraw/wrapper.h");
    let mut builder = bindgen::Builder::default()
        .header("src/libraw/wrapper.h")
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_type("LibRaw_.*")
        .allowlist_var("LIBRAW_.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for path in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", path.display()));
    }
    builder
        .generate()
        .expect("bindgen failed on libraw.h")
        .write_to_file(PathBuf::from(env::var("OUT_DIR").unwrap()).join("libraw_bindings.rs"))
        .expect("could not write bindings");
}
```

- [ ] **Step 3: Write `src/libraw/mod.rs` with tests**

```rust
use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::decode::Rgba;

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    unsafe_op_in_unsafe_fn,
    clippy::all
)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/libraw_bindings.rs"));
}

#[derive(Debug, thiserror::Error)]
pub enum LibRawError {
    #[error("LibRaw could not be initialised")]
    Init,
    #[error("path contains a NUL byte")]
    BadPath,
    #[error("{0}")]
    Code(String),
    #[error("unexpected LibRaw output format")]
    Format,
}

fn check(code: c_int) -> Result<(), LibRawError> {
    if code == 0 {
        return Ok(());
    }
    let msg = unsafe { CStr::from_ptr(sys::libraw_strerror(code)) };
    Err(LibRawError::Code(msg.to_string_lossy().into_owned()))
}

/// LibRaw `sizes.flip` → EXIF orientation.
pub fn flip_to_orientation(flip: i32) -> u8 {
    match flip {
        3 => 3,
        5 => 8,
        6 => 6,
        _ => 1,
    }
}

/// An opened RAW file. Not shared between threads; moved into a worker is fine.
pub struct Raw {
    ptr: *mut sys::libraw_data_t,
}

// SAFETY: the handle is owned exclusively; libraw_r has no global mutable state.
unsafe impl Send for Raw {}

struct MemImage(*mut sys::libraw_processed_image_t);

impl Drop for MemImage {
    fn drop(&mut self) {
        unsafe { sys::libraw_dcraw_clear_mem(self.0) }
    }
}

impl Raw {
    pub fn open(path: &Path) -> Result<Raw, LibRawError> {
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| LibRawError::BadPath)?;
        let ptr = unsafe { sys::libraw_init(0) };
        if ptr.is_null() {
            return Err(LibRawError::Init);
        }
        let raw = Raw { ptr };
        check(unsafe { sys::libraw_open_file(raw.ptr, c_path.as_ptr()) })?;
        Ok(raw)
    }

    pub fn flip(&self) -> i32 {
        unsafe { (*self.ptr).sizes.flip }
    }

    /// The camera's embedded JPEG thumbnail, if it is a JPEG. Not rotated.
    pub fn thumbnail_jpeg(&mut self) -> Result<Option<Vec<u8>>, LibRawError> {
        unsafe {
            check(sys::libraw_unpack_thumb(self.ptr))?;
            let mut err: c_int = 0;
            let img = sys::libraw_dcraw_make_mem_thumb(self.ptr, &mut err);
            if img.is_null() {
                return Err(check(err).err().unwrap_or(LibRawError::Format));
            }
            let img = MemImage(img);
            let r = &*img.0;
            if r.type_ != sys::LibRaw_image_formats_LIBRAW_IMAGE_JPEG {
                return Ok(None);
            }
            let data = std::slice::from_raw_parts(r.data.as_ptr(), r.data_size as usize);
            Ok(Some(data.to_vec()))
        }
    }

    /// Demosaics the sensor data to 8-bit sRGB. Output is already upright.
    pub fn decode(self, half_size: bool) -> Result<Rgba, LibRawError> {
        unsafe {
            let params = &mut (*self.ptr).params;
            params.half_size = half_size as _;
            params.use_camera_wb = 1;
            params.output_bps = 8;
            check(sys::libraw_unpack(self.ptr))?;
            check(sys::libraw_dcraw_process(self.ptr))?;
            let mut err: c_int = 0;
            let img = sys::libraw_dcraw_make_mem_image(self.ptr, &mut err);
            if img.is_null() {
                return Err(check(err).err().unwrap_or(LibRawError::Format));
            }
            let img = MemImage(img);
            let r = &*img.0;
            if r.type_ != sys::LibRaw_image_formats_LIBRAW_IMAGE_BITMAP || r.colors != 3 || r.bits != 8 {
                return Err(LibRawError::Format);
            }
            let (w, h) = (r.width as u32, r.height as u32);
            let rgb = std::slice::from_raw_parts(r.data.as_ptr(), r.data_size as usize);
            let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
            for px in rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Ok(Rgba::new(w, h, rgba))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe { sys::libraw_close(self.ptr) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_flip_to_exif_orientation() {
        assert_eq!(flip_to_orientation(0), 1);
        assert_eq!(flip_to_orientation(3), 3);
        assert_eq!(flip_to_orientation(5), 8);
        assert_eq!(flip_to_orientation(6), 6);
    }

    #[test]
    fn missing_file_is_error() {
        assert!(Raw::open(Path::new("/definitely/missing.cr3")).is_err());
    }

    #[test]
    fn non_raw_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        crate::testutil::write_jpeg(&p, 16, 16, [0, 0, 0]);
        assert!(Raw::open(&p).and_then(|r| r.decode(true)).is_err());
    }
}
```

If bindgen names the enum constants differently (e.g. `LibRaw_image_formats::LIBRAW_IMAGE_JPEG`), open `target/debug/build/photobrowser-*/out/libraw_bindings.rs`, find `LIBRAW_IMAGE_JPEG`, and use the generated name.

- [ ] **Step 4: Write `src/decode/raw.rs` with sample-driven tests**

```rust
use std::path::Path;

use super::{jpeg, DecodeError, Rgba};
use crate::exif;
use crate::libraw::{flip_to_orientation, Raw};

/// Full view never uses a preview smaller than this.
const MIN_FULL_PREVIEW_EDGE: u32 = 1024;

fn needed_edge(target_edge: u32) -> u32 {
    if target_edge == 0 { MIN_FULL_PREVIEW_EDGE } else { target_edge.min(MIN_FULL_PREVIEW_EDGE) }
}

/// Best embedded preview for display, plus the EXIF orientation still to apply.
pub fn decode_raw_preview(path: &Path, target_edge: u32) -> Result<(Rgba, u8), DecodeError> {
    let needed = needed_edge(target_edge);

    exif::init();
    if let Ok(meta) = rexiv2::Metadata::new_from_path(path) {
        if let Some(data) = largest_exiv2_preview(&meta, needed) {
            if let Ok(img) = jpeg::decode_jpeg(&data, target_edge) {
                return Ok((img, exif::orientation(&meta)));
            }
        }
    }

    let mut raw = Raw::open(path)?;
    let orientation = flip_to_orientation(raw.flip());
    if let Ok(Some(data)) = raw.thumbnail_jpeg() {
        let big_enough = jpeg::dimensions(&data).is_some_and(|(w, h)| w.max(h) >= needed);
        if big_enough {
            if let Ok(img) = jpeg::decode_jpeg(&data, target_edge) {
                return Ok((img, orientation));
            }
        }
    }
    Ok((raw.decode(true)?, 1))
}

fn largest_exiv2_preview(meta: &rexiv2::Metadata, needed: u32) -> Option<Vec<u8>> {
    let previews = meta.get_preview_images()?;
    let best = previews
        .into_iter()
        .filter(|p| {
            p.get_extension()
                .map(|e| e.trim_start_matches('.').eq_ignore_ascii_case("jpg"))
                .unwrap_or(false)
        })
        .max_by_key(|p| p.get_width() as u64 * p.get_height() as u64)?;
    if best.get_width().max(best.get_height()) < needed {
        return None;
    }
    best.get_data().ok()
}

/// Full-resolution demosaic (slow: 0.5–2 s). Upright.
pub fn decode_raw_full(path: &Path) -> Result<Rgba, DecodeError> {
    Ok(Raw::open(path)?.decode(false)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FileKind;
    use std::path::PathBuf;

    /// RAW samples come from $PB_RAW_SAMPLES (a folder of your own RAW files).
    /// Without it these tests are skipped, loudly.
    fn samples() -> Vec<PathBuf> {
        let Ok(dir) = std::env::var("PB_RAW_SAMPLES") else {
            eprintln!("PB_RAW_SAMPLES not set: skipping RAW sample tests");
            return Vec::new();
        };
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| FileKind::from_path(p) == Some(FileKind::Raw))
            .collect()
    }

    #[test]
    fn needed_edge_rules() {
        assert_eq!(needed_edge(256), 256);
        assert_eq!(needed_edge(3840), 1024);
        assert_eq!(needed_edge(0), 1024);
    }

    #[test]
    fn previews_every_sample() {
        for p in samples() {
            let (img, orientation) = decode_raw_preview(&p, 256)
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert!(img.long_edge() >= 256, "{}: {}x{}", p.display(), img.width, img.height);
            assert!((1..=8).contains(&orientation));
        }
    }

    #[test]
    fn full_decodes_every_sample() {
        for p in samples() {
            let img = decode_raw_full(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert!(img.long_edge() > 1000, "{}", p.display());
        }
    }
}
```

- [ ] **Step 5: Wire RAW into `src/decode/mod.rs`**

Add `pub mod raw;` next to the other `pub mod` lines. Replace the `FileKind::Raw` arm of `decode_for_display`:

```rust
        FileKind::Raw => raw::decode_raw_preview(&file.path, max_edge)?,
```

Add below `decode_for_display`:

```rust
/// Native-resolution decode for the 100% view: the RAW of the item if it has one.
pub fn decode_actual_size(item: &crate::media::Item) -> Result<Rgba, DecodeError> {
    match item.raw_file() {
        Some(raw_file) => raw::decode_raw_full(&raw_file.path),
        None => decode_for_display(item.display_file(), 0, Quality::High),
    }
}

impl From<crate::libraw::LibRawError> for DecodeError {
    fn from(e: crate::libraw::LibRawError) -> Self {
        DecodeError::Raw(e.to_string())
    }
}
```

Add a test to the `tests` module of `src/decode/mod.rs`:

```rust
    #[test]
    fn fake_raw_is_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fake.CR3");
        std::fs::write(&p, b"not a raw file").unwrap();
        assert!(guarded(|| decode_for_display(&media(p), 256, Quality::Fast)).is_err());
    }
```

Add `pub mod libraw;` to `src/lib.rs`.

- [ ] **Step 6: Run tests**

Run: `cargo test libraw decode`
Expected: PASS (sample tests print "skipping" without `PB_RAW_SAMPLES`).
Then, with a folder of your own RAW files (ideally one per camera/format you shoot, including a portrait shot):
Run: `PB_RAW_SAMPLES=/path/to/raw-samples cargo test decode::raw -- --nocapture`
Expected: PASS for every sample. A failure names the file — investigate before continuing (e.g. CR3 needs exiv2 built with BMFF; the LibRaw fallback must still succeed).

- [ ] **Step 7: Commit**

```bash
git add build.rs src
git commit -m "feat: RAW previews via exiv2/LibRaw and full LibRaw decode

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Disk thumbnail cache

**Files:**
- Create: `src/thumbcache.rs`
- Modify: `src/lib.rs` (add `pub mod thumbcache;`)

**Interfaces:**
- Consumes: `media::MediaFile`, `decode::{Rgba, jpeg::decode_jpeg}`
- Produces:
  - `thumbcache::DiskCache` (`Send + Sync`): `default_dir() -> PathBuf`, `new(PathBuf, cap_bytes: u64) -> io::Result<DiskCache>`, `key(&MediaFile) -> String`, `contains(&self, &MediaFile) -> bool`, `load(&self, &MediaFile) -> Option<Rgba>`, `store(&self, &MediaFile, &Rgba) -> io::Result<()>`, `cleanup(&self) -> io::Result<u64>` (bytes removed)
  - `thumbcache::encode_jpeg(&Rgba) -> Result<Vec<u8>, turbojpeg::Error>` (quality 85, 4:2:0)

Thumbnails are stored as JPEG (not WebP as the spec draft said): libjpeg-turbo encodes and decodes a 256 px thumbnail in well under 1 ms and the files are ~15 KB; the `image` crate's WebP encoder is lossless-only and much larger.

- [ ] **Step 1: Write the failing tests** (bottom of `src/thumbcache.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::FileKind;
    use std::fs::File;
    use std::time::{Duration, UNIX_EPOCH};

    fn media(path: &str, size: u64, mtime_ns: i64) -> MediaFile {
        MediaFile { path: PathBuf::from(path), kind: FileKind::Jpeg, size, mtime_ns }
    }

    fn thumb() -> Rgba {
        Rgba::new(64, 48, vec![120; 64 * 48 * 4])
    }

    #[test]
    fn store_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let f = media("/p/a.jpg", 10, 20);
        assert!(!cache.contains(&f));
        assert!(cache.load(&f).is_none());
        cache.store(&f, &thumb()).unwrap();
        assert!(cache.contains(&f));
        let img = cache.load(&f).unwrap();
        assert_eq!((img.width, img.height), (64, 48));
    }

    #[test]
    fn key_changes_with_mtime_and_size() {
        let a = DiskCache::key(&media("/p/a.jpg", 10, 20));
        assert_ne!(a, DiskCache::key(&media("/p/a.jpg", 10, 21)));
        assert_ne!(a, DiskCache::key(&media("/p/a.jpg", 11, 20)));
        assert_ne!(a, DiskCache::key(&media("/p/b.jpg", 10, 20)));
        assert_eq!(a, DiskCache::key(&media("/p/a.jpg", 10, 20)));
    }

    #[test]
    fn corrupt_entry_loads_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), u64::MAX).unwrap();
        let f = media("/p/a.jpg", 1, 1);
        fs::write(cache.path_for(&f), b"junk").unwrap();
        assert!(cache.load(&f).is_none());
    }

    #[test]
    fn cleanup_removes_oldest_until_under_cap() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf(), 250).unwrap();
        for (i, name) in ["old.jpg", "mid.jpg", "new.jpg"].iter().enumerate() {
            let p = dir.path().join(name);
            fs::write(&p, vec![0u8; 100]).unwrap();
            File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1000 + i as u64))
                .unwrap();
        }
        assert_eq!(cache.cleanup().unwrap(), 100);
        assert!(!dir.path().join("old.jpg").exists());
        assert!(dir.path().join("mid.jpg").exists() && dir.path().join("new.jpg").exists());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test thumbcache`
Expected: FAIL to compile.

- [ ] **Step 3: Implement (top of `src/thumbcache.rs`)**

```rust
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::decode::jpeg::decode_jpeg;
use crate::decode::Rgba;
use crate::media::MediaFile;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct DiskCache {
    dir: PathBuf,
    cap_bytes: u64,
}

impl DiskCache {
    pub fn default_dir() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("photobrowser")
            .join("thumbs")
    }

    pub fn new(dir: PathBuf, cap_bytes: u64) -> io::Result<DiskCache> {
        fs::create_dir_all(&dir)?;
        Ok(DiskCache { dir, cap_bytes })
    }

    /// blake3(path ‖ mtime_ns ‖ size): any edit to the file yields a new key.
    pub fn key(file: &MediaFile) -> String {
        let mut h = blake3::Hasher::new();
        h.update(file.path.as_os_str().as_bytes());
        h.update(&file.mtime_ns.to_le_bytes());
        h.update(&file.size.to_le_bytes());
        h.finalize().to_hex().to_string()
    }

    fn path_for(&self, file: &MediaFile) -> PathBuf {
        self.dir.join(format!("{}.jpg", Self::key(file)))
    }

    pub fn contains(&self, file: &MediaFile) -> bool {
        self.path_for(file).exists()
    }

    pub fn load(&self, file: &MediaFile) -> Option<Rgba> {
        let data = fs::read(self.path_for(file)).ok()?;
        decode_jpeg(&data, 0).ok()
    }

    /// Writes atomically (temp file + rename) so readers never see a partial file.
    pub fn store(&self, file: &MediaFile, img: &Rgba) -> io::Result<()> {
        let jpeg = encode_jpeg(img).map_err(io::Error::other)?;
        let target = self.path_for(file);
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.dir.join(format!(".{}.{}.{n}.tmp", Self::key(file), std::process::id()));
        fs::write(&tmp, &jpeg)?;
        fs::rename(&tmp, &target).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
    }

    /// Deletes the oldest entries until the cache is within its cap.
    pub fn cleanup(&self) -> io::Result<u64> {
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file()
                    .then(|| (e.path(), m.len(), m.modified().unwrap_or(UNIX_EPOCH)))
            })
            .collect();
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        if total <= self.cap_bytes {
            return Ok(0);
        }
        entries.sort_by_key(|e| e.2);
        let mut removed = 0;
        for (path, len, _) in entries {
            if total <= self.cap_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
                removed += len;
            }
        }
        Ok(removed)
    }
}

pub fn encode_jpeg(img: &Rgba) -> Result<Vec<u8>, turbojpeg::Error> {
    let mut c = turbojpeg::Compressor::new()?;
    c.set_quality(85)?;
    c.set_subsamp(turbojpeg::Subsamp::Sub2x2)?;
    c.compress_to_vec(turbojpeg::Image {
        pixels: img.data.as_slice(),
        width: img.width as usize,
        pitch: img.width as usize * 4,
        height: img.height as usize,
        format: turbojpeg::PixelFormat::RGBA,
    })
}
```

Add `pub mod thumbcache;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test thumbcache`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat: on-disk JPEG thumbnail cache with size cap

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Move/copy with conflicts and rollback

**Files:**
- Create: `src/fileops.rs`
- Modify: `src/lib.rs` (add `pub mod fileops;`)

**Interfaces:**
- Consumes: `xmp::sidecar_path`
- Produces:
  - `fileops::OpKind { Move, Copy }`, `fileops::ConflictPolicy { Skip, Rename }`
  - `fileops::TransferOutcome { Done(Vec<(PathBuf, PathBuf)>), Skipped, Failed(String) }` — `Done` lists every (source, destination) moved/copied, media files and sidecars, in transfer order
  - `fileops::has_conflict(&[PathBuf], &Path) -> bool`
  - `fileops::plan_targets(&[PathBuf], &Path, ConflictPolicy) -> Option<Vec<(PathBuf, PathBuf)>>`
  - `fileops::transfer_item(&[PathBuf], &Path, OpKind, ConflictPolicy) -> TransferOutcome` — `sources` are the item's media files; existing sidecars travel with them. Never overwrites. On any failure, everything already transferred for this item is undone. Moving into the item's own folder is `Skipped`.

Rename policy: the same `_N` suffix is applied to every file of the item (`IMG_1.CR3`, `IMG_1.JPG` → `IMG_1_1.CR3`, `IMG_1_1.JPG`; sidecars follow: `IMG_1_1.CR3.xmp`), using the smallest N ≥ 1 for which none of the targets exist.

- [ ] **Step 1: Write the failing tests** (bottom of `src/fileops.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp::sidecar_path;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    struct Fixture {
        _tmp: tempfile::TempDir,
        src: PathBuf,
        dst: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        Fixture { _tmp: tmp, src, dst }
    }

    fn write(p: &Path, content: &str) {
        fs::write(p, content).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// IMG_1.CR3 (+ sidecar) and IMG_1.JPG in `src`.
    fn pair(f: &Fixture) -> Vec<PathBuf> {
        let raw = f.src.join("IMG_1.CR3");
        let jpg = f.src.join("IMG_1.JPG");
        write(&raw, "raw");
        write(&sidecar_path(&raw), "xmp");
        write(&jpg, "jpg");
        vec![raw, jpg]
    }

    #[test]
    fn move_pair_with_sidecar() {
        let f = fixture();
        let sources = pair(&f);
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        let TransferOutcome::Done(done) = out else { panic!("{out:?}") };
        assert_eq!(done.len(), 3);
        assert_eq!(names(&f.dst), ["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.JPG"]);
        assert!(names(&f.src).is_empty());
    }

    #[test]
    fn copy_keeps_sources_and_mtime() {
        let f = fixture();
        let sources = pair(&f);
        let before = fs::metadata(&sources[0]).unwrap().modified().unwrap();
        let out = transfer_item(&sources, &f.dst, OpKind::Copy, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert_eq!(names(&f.src).len(), 3);
        assert_eq!(names(&f.dst).len(), 3);
        let after = fs::metadata(f.dst.join("IMG_1.CR3")).unwrap().modified().unwrap();
        assert_eq!(before, after);
        assert_eq!(fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(), "jpg");
    }

    #[test]
    fn skip_on_conflict_changes_nothing() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.dst.join("IMG_1.JPG"), "existing");
        assert!(has_conflict(&sources, &f.dst));
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert_eq!(out, TransferOutcome::Skipped);
        assert_eq!(names(&f.src).len(), 3);
        assert_eq!(fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(), "existing");
    }

    #[test]
    fn rename_on_conflict_renames_whole_item() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.dst.join("IMG_1.JPG"), "existing");
        let out = transfer_item(&sources, &f.dst, OpKind::Move, ConflictPolicy::Rename);
        assert!(matches!(out, TransferOutcome::Done(_)), "{out:?}");
        assert_eq!(
            names(&f.dst),
            ["IMG_1.JPG", "IMG_1_1.CR3", "IMG_1_1.CR3.xmp", "IMG_1_1.JPG"]
        );
        assert_eq!(fs::read_to_string(f.dst.join("IMG_1.JPG")).unwrap(), "existing");
    }

    #[test]
    fn rename_skips_taken_numbers() {
        let f = fixture();
        let sources = pair(&f);
        write(&f.dst.join("IMG_1.CR3"), "x");
        write(&f.dst.join("IMG_1_1.JPG"), "x");
        let plan = plan_targets(&sources, &f.dst, ConflictPolicy::Rename).unwrap();
        assert_eq!(plan[0].1, f.dst.join("IMG_1_2.CR3"));
    }

    #[test]
    fn move_rolls_back_on_failure() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        write(&sidecar_path(&raw), "xmp");
        let missing = f.src.join("IMG_1.JPG"); // never created → second transfer fails
        let out = transfer_item(&[raw.clone(), missing], &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Failed(_)), "{out:?}");
        assert_eq!(names(&f.src), ["IMG_1.CR3", "IMG_1.CR3.xmp"]);
        assert!(names(&f.dst).is_empty());
    }

    #[test]
    fn copy_rolls_back_on_failure() {
        let f = fixture();
        let raw = f.src.join("IMG_1.CR3");
        write(&raw, "raw");
        let out = transfer_item(
            &[raw, f.src.join("IMG_1.JPG")],
            &f.dst,
            OpKind::Copy,
            ConflictPolicy::Skip,
        );
        assert!(matches!(out, TransferOutcome::Failed(_)));
        assert!(names(&f.dst).is_empty(), "{:?}", names(&f.dst));
    }

    #[test]
    fn move_into_same_folder_is_noop() {
        let f = fixture();
        let sources = pair(&f);
        let out = transfer_item(&sources, &f.src, OpKind::Move, ConflictPolicy::Rename);
        assert_eq!(out, TransferOutcome::Skipped);
        assert_eq!(names(&f.src), ["IMG_1.CR3", "IMG_1.CR3.xmp", "IMG_1.JPG"]);
    }

    #[test]
    fn moves_non_utf8_names() {
        let f = fixture();
        let p = f.src.join(OsStr::from_bytes(b"\xffphoto.jpg"));
        write(&p, "x");
        let out = transfer_item(&[p], &f.dst, OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert!(f.dst.join(OsStr::from_bytes(b"\xffphoto.jpg")).exists());
    }

    #[test]
    fn cross_filesystem_move_when_available() {
        // /dev/shm is tmpfs on Fedora; the project dir is usually on a disk.
        use std::os::unix::fs::MetadataExt;
        let shm = Path::new("/dev/shm");
        let Ok(src_dir) = tempfile::tempdir_in(shm) else { return };
        let Ok(dst_dir) = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")) else { return };
        let dev = |p: &Path| fs::metadata(p).unwrap().dev();
        if dev(src_dir.path()) == dev(dst_dir.path()) {
            eprintln!("same filesystem: skipping cross-fs test");
            return;
        }
        let p = src_dir.path().join("a.jpg");
        write(&p, "content");
        let out = transfer_item(&[p.clone()], dst_dir.path(), OpKind::Move, ConflictPolicy::Skip);
        assert!(matches!(out, TransferOutcome::Done(_)));
        assert!(!p.exists());
        assert_eq!(fs::read_to_string(dst_dir.path().join("a.jpg")).unwrap(), "content");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test fileops`
Expected: FAIL to compile.

- [ ] **Step 3: Implement (top of `src/fileops.rs`)**

```rust
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::xmp::sidecar_path;

const MAX_RENAME_ATTEMPTS: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Move,
    Copy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Skip,
    /// Append `_N` to every file of the item.
    Rename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    Done(Vec<(PathBuf, PathBuf)>),
    Skipped,
    Failed(String),
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok() // also catches dangling symlinks
}

fn numbered_name(src: &Path, n: u32) -> OsString {
    let mut s = src.file_stem().unwrap_or_default().to_os_string();
    s.push(format!("_{n}"));
    if let Some(ext) = src.extension() {
        s.push(".");
        s.push(ext);
    }
    s
}

fn build_plan(sources: &[PathBuf], dest_dir: &Path, n: u32) -> Vec<(PathBuf, PathBuf)> {
    let mut plan = Vec::new();
    for src in sources {
        let name = if n == 0 {
            src.file_name().unwrap_or_default().to_os_string()
        } else {
            numbered_name(src, n)
        };
        let dst = dest_dir.join(name);
        let sidecar = sidecar_path(src);
        let has_sidecar = exists(&sidecar);
        plan.push((src.clone(), dst.clone()));
        if has_sidecar {
            plan.push((sidecar, sidecar_path(&dst)));
        }
    }
    plan
}

fn is_free(plan: &[(PathBuf, PathBuf)]) -> bool {
    plan.iter().all(|(_, dst)| !exists(dst))
}

pub fn has_conflict(sources: &[PathBuf], dest_dir: &Path) -> bool {
    !is_free(&build_plan(sources, dest_dir, 0))
}

pub fn plan_targets(
    sources: &[PathBuf],
    dest_dir: &Path,
    policy: ConflictPolicy,
) -> Option<Vec<(PathBuf, PathBuf)>> {
    let plain = build_plan(sources, dest_dir, 0);
    if is_free(&plain) {
        return Some(plain);
    }
    match policy {
        ConflictPolicy::Skip => None,
        ConflictPolicy::Rename => (1..=MAX_RENAME_ATTEMPTS)
            .map(|n| build_plan(sources, dest_dir, n))
            .find(|p| is_free(p)),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub fn transfer_item(
    sources: &[PathBuf],
    dest_dir: &Path,
    kind: OpKind,
    policy: ConflictPolicy,
) -> TransferOutcome {
    let in_dest = sources
        .iter()
        .all(|s| s.parent().is_some_and(|p| same_dir(p, dest_dir)));
    if in_dest {
        return TransferOutcome::Skipped;
    }
    let Some(plan) = plan_targets(sources, dest_dir, policy) else {
        return TransferOutcome::Skipped;
    };
    let mut done = Vec::with_capacity(plan.len());
    for (src, dst) in plan {
        if let Err(e) = transfer_one(&src, &dst, kind) {
            rollback(&done, kind);
            return TransferOutcome::Failed(format!("{}: {e}", src.display()));
        }
        done.push((src, dst));
    }
    TransferOutcome::Done(done)
}

fn transfer_one(src: &Path, dst: &Path, kind: OpKind) -> io::Result<()> {
    if exists(dst) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
    }
    match kind {
        OpKind::Copy => copy_verified(src, dst),
        OpKind::Move => match fs::rename(src, dst) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                copy_verified(src, dst)?;
                fs::remove_file(src).inspect_err(|_| {
                    let _ = fs::remove_file(dst);
                })
            }
            Err(e) => Err(e),
        },
    }
}

/// Copies via a hidden temp file, fsyncs, checks the size, keeps the mtime, then renames.
fn copy_verified(src: &Path, dst: &Path) -> io::Result<()> {
    let mut tmp_name = OsString::from(".");
    tmp_name.push(dst.file_name().unwrap_or_default());
    tmp_name.push(".pbpart");
    let tmp = dst.with_file_name(tmp_name);

    let result = (|| {
        let mut input = File::open(src)?;
        let src_meta = input.metadata()?;
        let mut out = File::create_new(&tmp)?;
        let copied = io::copy(&mut input, &mut out)?;
        out.set_modified(src_meta.modified()?)?;
        out.sync_all()?;
        if copied != src_meta.len() {
            return Err(io::Error::other("size mismatch after copy"));
        }
        if exists(dst) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"));
        }
        fs::rename(&tmp, dst)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn rollback(done: &[(PathBuf, PathBuf)], kind: OpKind) {
    for (src, dst) in done.iter().rev() {
        match kind {
            OpKind::Copy => {
                let _ = fs::remove_file(dst);
            }
            OpKind::Move => {
                if fs::rename(dst, src).is_err() && copy_verified(dst, src).is_ok() {
                    let _ = fs::remove_file(dst);
                }
            }
        }
    }
}
```

`io::ErrorKind::CrossesDevices` is stable since Rust 1.85; `File::create_new` since 1.77; `File::set_modified` since 1.75.

Add `pub mod fileops;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test fileops -- --nocapture`
Expected: PASS — 10 tests (the cross-fs test may print "skipping").

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat: atomic per-item move/copy with conflict handling and rollback

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Configuration file

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod config;`)

**Interfaces:**
- Consumes: nothing
- Produces: `config::Config { auto_advance: bool, thumb_cache_items: usize, full_cache_items: usize, disk_cache_mb: u64, thumb_display_size: i32, last_folder: Option<PathBuf> }` (defaults: false, 300, 6, 2048, 192, None) with `default_path() -> PathBuf`, `load_from(&Path) -> Config` (default on missing/invalid file), `save_to(&self, &Path) -> io::Result<()>`.

- [ ] **Step 1: Write `src/config.rs` with tests**

```rust
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub auto_advance: bool,
    pub thumb_cache_items: usize,
    pub full_cache_items: usize,
    pub disk_cache_mb: u64,
    pub thumb_display_size: i32,
    pub last_folder: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            auto_advance: false,
            thumb_cache_items: 300,
            full_cache_items: 6,
            disk_cache_mb: 2048,
            thumb_display_size: 192,
            last_folder: None,
        }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("photobrowser")
            .join("config.toml")
    }

    pub fn load_from(path: &Path) -> Config {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let cfg = Config {
            auto_advance: true,
            last_folder: Some(PathBuf::from("/photos/2026")),
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
    }

    #[test]
    fn missing_or_garbage_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load_from(&dir.path().join("none.toml")), Config::default());
        let bad = dir.path().join("bad.toml");
        fs::write(&bad, "this is = = not toml").unwrap();
        assert_eq!(Config::load_from(&bad), Config::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        fs::write(&path, "auto_advance = true\n").unwrap();
        let cfg = Config::load_from(&path);
        assert!(cfg.auto_advance);
        assert_eq!(cfg.thumb_cache_items, 300);
    }
}
```

Add `pub mod config;` to `src/lib.rs`.

- [ ] **Step 2: Run tests**

Run: `cargo test config`
Expected: PASS — 3 tests.

- [ ] **Step 3: Run the whole suite and clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all tests pass; no clippy errors (fix any that appear before committing).

- [ ] **Step 4: Commit**

```bash
git add src
git commit -m "feat: TOML configuration

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 13: UI foundation — window, item model, thumbnail grid

**Files:**
- Create: `src/ui/style.rs`, `src/ui/item_object.rs`, `src/ui/state.rs`, `src/ui/grid.rs`, `src/ui/keys.rs`, `src/ui/window.rs`
- Modify: `src/ui/mod.rs` (replace)

**Interfaces:**
- Consumes: everything from Tasks 2–12
- Produces (used by Tasks 14–17):
  - `ui::item_object::ItemObject` — GObject with read-only properties `stars` (String), `rejected`, `is-pair`, `has-error` (bool), `name`, `error` (String), `thumbnail` (`gdk::Paintable`); methods `new(Item)`, `id()`, `item() -> Item`, `with_item(|&Item| ..)`, `set_item(Item)`, `update_item(|&mut Item| ..)`, `thumbnail()`, `set_thumbnail(Option<gdk::Texture>)`, `has_error()`, `set_error(Option<String>)`
  - `ui::item_object::stars_text(u8) -> String`
  - `ui::state::Msg` enum, `ui::state::Widgets` struct, `ui::state::AppState` with public: `config`, `index`, `pool`, `disk`, `tx`, `store`, `filter`, `sorter`, `selection`, `filter_state`, `sort_state`, `widgets`; methods `new`, `widgets()`, `toast(&str)`, `folder()`, `object(ItemId)`, `refilter()`, `resort()`, `update_counter()`, `handle(Msg)`, `choose_folder()`, `open_folder(PathBuf)`, `rescan()`, `folder_lost(&str)`, `bind_cell(&ItemObject)`, `unbind_cell(&ItemObject)`, `request_thumb(&ItemObject, Priority)`
  - `ui::state::texture_from(Rgba) -> gdk::Texture`
  - `ui::grid::{thumb_factory(&Rc<AppState>, compact: bool), build(&Rc<AppState>) -> gtk::GridView}`
  - `ui::style::{load(), set_thumb_size(i32), THUMB_SIZE_MIN, THUMB_SIZE_MAX, THUMB_SIZE_STEP}`
  - `ui::keys::handle(&Rc<AppState>, gdk::Key, gdk::ModifierType) -> glib::Propagation`

Threading rule: `AppState` lives on the GTK main thread (`Rc`, `RefCell`). Worker closures capture only `Send` data (`MediaFile`, `Item`, `Arc<DiskCache>`, the `async_channel::Sender`) and report back with `Msg`.

- [ ] **Step 1: Write `src/ui/style.rs`**

```rust
use gtk::gdk;

pub const THUMB_SIZE_MIN: i32 = 128;
pub const THUMB_SIZE_MAX: i32 = 512;
pub const THUMB_SIZE_STEP: i32 = 32;

const BASE_CSS: &str = "
.pb-cell { padding: 6px; }
.pb-stars { font-size: 0.9em; color: @accent_color; }
.pb-badge { font-size: 0.7em; font-weight: bold; padding: 1px 4px; margin: 4px;
            border-radius: 4px; background: alpha(black, 0.6); color: white; }
.pb-reject { font-weight: bold; padding: 0 5px; margin: 4px; border-radius: 4px;
             background: @error_bg_color; color: @error_fg_color; }
.pb-strip-thumb { min-width: 72px; min-height: 72px; }
.pb-osd { background: alpha(black, 0.6); color: white; padding: 8px 12px;
          border-radius: 8px; margin: 12px; }
.pb-viewer { background: black; }
";

thread_local! {
    static SIZE_PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
}

pub fn load() {
    let display = gdk::Display::default().expect("no display");
    let base = gtk::CssProvider::new();
    base.load_from_string(BASE_CSS);
    gtk::style_context_add_provider_for_display(&display, &base, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    SIZE_PROVIDER.with(|p| {
        gtk::style_context_add_provider_for_display(&display, p, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION)
    });
}

/// Sets the on-screen thumbnail size for every grid cell at once.
pub fn set_thumb_size(px: i32) {
    let px = px.clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX);
    SIZE_PROVIDER.with(|p| {
        p.load_from_string(&format!(".pb-thumb {{ min-width: {px}px; min-height: {px}px; }}"))
    });
}
```

- [ ] **Step 2: Write `src/ui/item_object.rs` with a test for `stars_text`**

```rust
use std::cell::RefCell;
use std::sync::OnceLock;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

use crate::media::{Item, ItemId};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ItemObject {
        pub item: RefCell<Option<Item>>,
        pub thumbnail: RefCell<Option<gdk::Texture>>,
        pub error: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemObject {
        const NAME: &'static str = "PbItemObject";
        type Type = super::ItemObject;
    }

    impl ObjectImpl for ItemObject {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPS.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("stars").read_only().build(),
                    glib::ParamSpecBoolean::builder("rejected").read_only().build(),
                    glib::ParamSpecBoolean::builder("is-pair").read_only().build(),
                    glib::ParamSpecString::builder("name").read_only().build(),
                    glib::ParamSpecObject::builder::<gdk::Paintable>("thumbnail").read_only().build(),
                    glib::ParamSpecBoolean::builder("has-error").read_only().build(),
                    glib::ParamSpecString::builder("error").read_only().build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let item = self.item.borrow();
            let item = item.as_ref();
            match pspec.name() {
                "stars" => item
                    .map(|i| if i.rejected { String::new() } else { stars_text(i.rating) })
                    .unwrap_or_default()
                    .to_value(),
                "rejected" => item.is_some_and(|i| i.rejected).to_value(),
                "is-pair" => item.is_some_and(|i| i.is_pair()).to_value(),
                "name" => item.map(|i| i.file_names()).unwrap_or_default().to_value(),
                "thumbnail" => self
                    .thumbnail
                    .borrow()
                    .as_ref()
                    .map(|t| t.clone().upcast::<gdk::Paintable>())
                    .to_value(),
                "has-error" => self.error.borrow().is_some().to_value(),
                "error" => self.error.borrow().clone().to_value(),
                name => unreachable!("unknown property {name}"),
            }
        }
    }
}

glib::wrapper! {
    pub struct ItemObject(ObjectSubclass<imp::ItemObject>);
}

impl ItemObject {
    pub fn new(item: Item) -> ItemObject {
        let obj: ItemObject = glib::Object::new();
        obj.imp().item.replace(Some(item));
        obj
    }

    pub fn id(&self) -> ItemId {
        self.with_item(|i| i.id)
    }

    pub fn item(&self) -> Item {
        self.with_item(Item::clone)
    }

    pub fn with_item<R>(&self, f: impl FnOnce(&Item) -> R) -> R {
        f(self.imp().item.borrow().as_ref().expect("ItemObject without item"))
    }

    pub fn set_item(&self, item: Item) {
        self.imp().item.replace(Some(item));
        self.notify_item();
    }

    pub fn update_item(&self, f: impl FnOnce(&mut Item)) {
        f(self.imp().item.borrow_mut().as_mut().expect("ItemObject without item"));
        self.notify_item();
    }

    fn notify_item(&self) {
        for name in ["stars", "rejected", "is-pair", "name"] {
            self.notify(name);
        }
    }

    pub fn thumbnail(&self) -> Option<gdk::Texture> {
        self.imp().thumbnail.borrow().clone()
    }

    pub fn set_thumbnail(&self, texture: Option<gdk::Texture>) {
        self.imp().thumbnail.replace(texture);
        self.notify("thumbnail");
    }

    pub fn has_error(&self) -> bool {
        self.imp().error.borrow().is_some()
    }

    pub fn set_error(&self, error: Option<String>) {
        self.imp().error.replace(error);
        self.notify("has-error");
        self.notify("error");
    }
}

pub fn stars_text(rating: u8) -> String {
    let r = rating.min(5) as usize;
    format!("{}{}", "★".repeat(r), "☆".repeat(5 - r))
}

#[cfg(test)]
mod tests {
    use super::stars_text;

    #[test]
    fn renders_stars() {
        assert_eq!(stars_text(0), "☆☆☆☆☆");
        assert_eq!(stars_text(3), "★★★☆☆");
        assert_eq!(stars_text(9), "★★★★★");
    }
}
```

- [ ] **Step 3: Write `src/ui/state.rs`**

```rust
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
        if let Some(obj) = removed {
            if let Some(pos) = self.store.find(&obj) {
                self.store.remove(pos);
            }
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
            if old_id != id && !on_screen {
                if let Some(old) = self.object(old_id) {
                    old.set_thumbnail(None);
                }
            }
        }
    }

    /// Fills the disk cache in the background so later scrolling never waits on a decoder.
    fn warm_disk_cache(&self, files: Vec<MediaFile>) {
        for file in files {
            let disk = self.disk.clone();
            self.pool.submit(Priority::Background, CancelToken::new(), move || {
                if !disk.contains(&file) {
                    if let Ok(img) = decode::guarded(|| {
                        decode::decode_for_display(&file, THUMB_EDGE, Quality::Fast)
                    }) {
                        let _ = disk.store(&file, &img);
                    }
                }
            });
        }
    }
}
```

- [ ] **Step 4: Write `src/ui/grid.rs`**

```rust
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use super::item_object::ItemObject;
use super::state::AppState;

fn list_item_object(obj: &glib::Object) -> Option<ItemObject> {
    obj.downcast_ref::<gtk::ListItem>()?
        .item()
        .and_downcast::<ItemObject>()
}

/// Cell factory shared by the grid (`compact = false`) and the filmstrip (`compact = true`).
pub fn thumb_factory(state: &Rc<AppState>, compact: bool) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .build();
        picture.add_css_class(if compact { "pb-strip-thumb" } else { "pb-thumb" });

        let broken = gtk::Image::from_icon_name("image-missing-symbolic");
        broken.set_pixel_size(48);
        broken.set_halign(gtk::Align::Center);
        broken.set_valign(gtk::Align::Center);

        let badge = gtk::Label::new(Some("RAW+JPG"));
        badge.add_css_class("pb-badge");
        badge.set_halign(gtk::Align::End);
        badge.set_valign(gtk::Align::Start);

        let reject = gtk::Label::new(Some("✕"));
        reject.add_css_class("pb-reject");
        reject.set_halign(gtk::Align::Start);
        reject.set_valign(gtk::Align::Start);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&picture));
        overlay.add_overlay(&broken);
        overlay.add_overlay(&badge);
        overlay.add_overlay(&reject);

        let stars = gtk::Label::new(None);
        stars.add_css_class("pb-stars");
        stars.set_visible(!compact);

        let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
        cell.add_css_class("pb-cell");
        cell.append(&overlay);
        cell.append(&stars);

        let item = li.property_expression("item");
        item.chain_property::<ItemObject>("thumbnail").bind(&picture, "paintable", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("has-error").bind(&broken, "visible", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("error").bind(&broken, "tooltip-text", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("is-pair").bind(&badge, "visible", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("rejected").bind(&reject, "visible", gtk::Widget::NONE);
        item.chain_property::<ItemObject>("name").bind(&cell, "tooltip-text", gtk::Widget::NONE);
        if !compact {
            item.chain_property::<ItemObject>("stars").bind(&stars, "label", gtk::Widget::NONE);
        }
        li.set_child(Some(&cell));
    });

    let s = state.clone();
    factory.connect_bind(move |_, obj| {
        if let Some(item) = list_item_object(obj) {
            s.bind_cell(&item);
        }
    });
    let s = state.clone();
    factory.connect_unbind(move |_, obj| {
        if let Some(item) = list_item_object(obj) {
            s.unbind_cell(&item);
        }
    });
    factory
}

pub fn build(state: &Rc<AppState>) -> gtk::GridView {
    let grid = gtk::GridView::new(Some(state.selection.clone()), Some(thumb_factory(state, false)));
    grid.set_max_columns(64);
    grid.set_enable_rubberband(true);
    grid
}
```

- [ ] **Step 5: Write `src/ui/keys.rs`** (extended in Tasks 14–16)

```rust
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::state::AppState;

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    match (ctrl, key.to_lower().to_unicode()) {
        (true, Some('o')) => state.choose_folder(),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
```

- [ ] **Step 6: Write `src/ui/window.rs`**

```rust
use std::path::PathBuf;

use adw::prelude::*;
use gtk::glib;

use super::state::{AppState, Widgets};
use super::{grid, keys, style};

pub fn build(app: &adw::Application, start_folder: Option<PathBuf>) {
    let (tx, rx) = async_channel::unbounded();
    let state = AppState::new(tx);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Photobrowser")
        .default_width(1400)
        .default_height(900)
        .build();

    let header = adw::HeaderBar::new();
    let open_button = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Open Folder (Ctrl+O)")
        .build();
    header.pack_start(&open_button);
    let counter = gtk::Label::new(None);
    counter.add_css_class("dim-label");
    counter.add_css_class("numeric");
    header.pack_end(&counter);

    let grid = grid::build(&state);
    let grid_scroller = gtk::ScrolledWindow::builder()
        .child(&grid)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let empty_button = gtk::Button::builder()
        .label("Open Folder…")
        .halign(gtk::Align::Center)
        .build();
    empty_button.add_css_class("pill");
    empty_button.add_css_class("suggested-action");
    let empty = adw::StatusPage::builder()
        .icon_name("folder-pictures-symbolic")
        .title("No Folder Open")
        .description("Choose a folder of photos to start culling")
        .child(&empty_button)
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&grid_scroller, Some("grid"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    let _ = state.widgets.set(Widgets { window: window.clone(), toasts, stack, grid, counter });

    for button in [&open_button, &empty_button] {
        let s = state.clone();
        button.connect_clicked(move |_| s.choose_folder());
    }
    let s = state.clone();
    state.selection.connect_items_changed(move |_, _, _, _| s.update_counter());

    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = state.clone();
    key_controller.connect_key_pressed(move |_, key, _, mods| keys::handle(&s, key, mods));
    window.add_controller(key_controller);

    style::set_thumb_size(state.config.borrow().thumb_display_size);

    let s = state.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            s.handle(msg);
        }
    });
    let disk = state.disk.clone();
    std::thread::spawn(move || {
        let _ = disk.cleanup();
    });

    let last = state.config.borrow().last_folder.clone();
    match start_folder.or(last).filter(|p| p.is_dir()) {
        Some(folder) => state.open_folder(folder),
        None => state.widgets().stack.set_visible_child_name("empty"),
    }
    window.present();
}
```

- [ ] **Step 7: Replace `src/ui/mod.rs`**

```rust
mod grid;
mod item_object;
mod keys;
mod state;
mod style;
mod window;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

pub use item_object::stars_text;

pub const APP_ID: &str = "io.github.enricod.PhotoBrowser";

/// `photobrowser [FOLDER]`
pub fn run() -> glib::ExitCode {
    crate::exif::init();
    let start_folder = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p));
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| style::load());
    app.connect_activate(move |app| window::build(app, start_folder.clone()));
    app.run_with_args::<&str>(&[])
}
```

- [ ] **Step 8: Build and run the unit tests**

Run: `cargo build && cargo test`
Expected: builds; all earlier tests plus `renders_stars` pass.
Compile errors from gtk4-rs signature drift (e.g. `NativeExt::surface`, `select_folder` callback type, `visible_dialog`) are fixed by checking `cargo doc -p gtk4 -p libadwaita --open`; keep the behaviour described here.

- [ ] **Step 9: Manual check** — prepare a test folder once and reuse it in later tasks:

```bash
mkdir -p ~/pb-test && cp <20–50 of your photos, including some RAW+JPEG pairs, a PNG and a TIFF> ~/pb-test/
head -c 2000 ~/pb-test/<some>.jpg > ~/pb-test/broken.jpg
cargo run --release -- ~/pb-test
```

Expected:
- the grid appears immediately; thumbnails stream in without the window freezing;
- pairs show a `RAW+JPG` badge; hovering a cell shows its file names;
- `broken.jpg` shows the "image missing" icon with the error as tooltip;
- the header counter reads `N / N`;
- `Ctrl+O` opens the folder chooser;
- quit and run the same command again: thumbnails appear at once (disk cache). `ls ~/.cache/photobrowser/thumbs | wc -l` ≈ number of items.

- [ ] **Step 10: Commit**

```bash
git add src
git commit -m "feat: main window with virtualised thumbnail grid

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: Filter bar, ratings, XMP writing, thumbnail zoom

**Files:**
- Create: `src/ui/filterbar.rs`
- Modify: `src/ui/state.rs`, `src/ui/window.rs`, `src/ui/keys.rs` (replace), `src/ui/mod.rs` (add `mod filterbar;`)

**Interfaces:**
- Consumes: Task 13 state; `catalog::rating::{RatingChange, apply}`, `xmp::write_rating`, `filter::{RejectFilter, TypeFilter, SortKey, SortState}`
- Produces on `AppState`: `set_filter(impl FnOnce(&mut FilterState))`, `set_sort(SortState)`, `selected_objects() -> Vec<ItemObject>`, `rating_targets() -> Vec<ItemObject>`, `rate(RatingChange)`, `unpin()`, `focus_filters()`, `zoom_thumbs(i32)`; new `Msg::XmpWritten`; `Widgets.filters: gtk::Box`. `filterbar::build(&Rc<AppState>) -> gtk::Box`.

XMP writes go through one dedicated thread, so writes to the same sidecar are never concurrent and stay in order. A file's record is marked synced only when its last queued write has finished.

- [ ] **Step 1: Write `src/ui/filterbar.rs`**

```rust
use std::rc::Rc;

use gtk::prelude::*;

use super::state::AppState;
use crate::filter::{RejectFilter, SortKey, SortState, TypeFilter};

pub fn build(state: &Rc<AppState>) -> gtk::Box {
    let rating = gtk::DropDown::from_strings(&["All ratings", "★ 1+", "★ 2+", "★ 3+", "★ 4+", "★ 5"]);
    rating.set_tooltip_text(Some("Minimum rating"));
    let reject = gtk::DropDown::from_strings(&["Show rejects", "Hide rejects", "Only rejects"]);
    reject.set_selected(1);
    let kind = gtk::DropDown::from_strings(&["All types", "RAW", "JPEG", "Other"]);
    let sort = gtk::DropDown::from_strings(&["Date ↑", "Date ↓", "Name ↑", "Name ↓"]);
    sort.set_tooltip_text(Some("Sort order"));

    let s = state.clone();
    rating.connect_selected_notify(move |dd| {
        let min = dd.selected().min(5) as u8;
        s.set_filter(|f| f.min_rating = min);
    });
    let s = state.clone();
    reject.connect_selected_notify(move |dd| {
        let mode = match dd.selected() {
            0 => RejectFilter::ShowAll,
            2 => RejectFilter::OnlyRejects,
            _ => RejectFilter::HideRejects,
        };
        s.set_filter(|f| f.reject = mode);
    });
    let s = state.clone();
    kind.connect_selected_notify(move |dd| {
        let kind = match dd.selected() {
            1 => TypeFilter::Raw,
            2 => TypeFilter::Jpeg,
            3 => TypeFilter::Other,
            _ => TypeFilter::All,
        };
        s.set_filter(|f| f.kind = kind);
    });
    let s = state.clone();
    sort.connect_selected_notify(move |dd| {
        let (key, descending) = match dd.selected() {
            1 => (SortKey::CaptureTime, true),
            2 => (SortKey::FileName, false),
            3 => (SortKey::FileName, true),
            _ => (SortKey::CaptureTime, false),
        };
        s.set_sort(SortState { key, descending });
    });

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for w in [&rating, &reject, &kind, &sort] {
        bar.append(w);
    }
    bar
}
```

- [ ] **Step 2: Extend `src/ui/state.rs`**

Add imports at the top:

```rust
use std::sync::mpsc;
use std::time::Duration;

use crate::catalog::rating::{self, RatingChange};
use crate::xmp;
```

Add the variant to `Msg`:

```rust
    XmpWritten { path: PathBuf, result: Result<i64, String> },
```

Add `pub filters: gtk::Box,` to `Widgets`.

Add fields to `AppState`:

```rust
    xmp_tx: mpsc::Sender<XmpJob>,
    xmp_inflight: RefCell<HashMap<PathBuf, u32>>,
    xmp_failures: Cell<u32>,
```

Add near `texture_from`:

```rust
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
                let result = xmp::write_rating(&job.path, job.rating, job.rejected).map_err(|e| e.to_string());
                if tx.send_blocking(Msg::XmpWritten { path: job.path, result }).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn XMP writer");
    job_tx
}
```

In `AppState::new`, before `Rc::new(...)`: `let xmp_tx = spawn_xmp_writer(tx.clone());` and add to the struct literal:

```rust
            xmp_tx,
            xmp_inflight: RefCell::default(),
            xmp_failures: Cell::new(0),
```

Add the match arm in `handle`:

```rust
            Msg::XmpWritten { path, result } => self.on_xmp_written(path, result),
```

At the end of `on_scanned`, after `self.warm_disk_cache(warm);`, add `self.flush_dirty_xmp();`.

Add these methods to `impl AppState`:

```rust
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

    pub fn zoom_thumbs(&self, step: i32) {
        use super::style::{set_thumb_size, THUMB_SIZE_MAX, THUMB_SIZE_MIN, THUMB_SIZE_STEP};
        let mut config = self.config.borrow_mut();
        let size = (config.thumb_display_size + step * THUMB_SIZE_STEP).clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX);
        if size != config.thumb_display_size {
            config.thumb_display_size = size;
            set_thumb_size(size);
            let _ = config.save_to(&Config::default_path());
        }
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
        self.selected_objects()
    }

    pub fn rate(self: &Rc<Self>, change: RatingChange) {
        let targets = self.rating_targets();
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
                if let Err(e) = self.index.borrow().set_rating(file, item.rating, item.rejected) {
                    eprintln!("index: {e}");
                }
                self.write_xmp(file.path.clone(), item.rating, item.rejected);
            }
        }
    }

    fn write_xmp(&self, path: PathBuf, rating: u8, rejected: bool) {
        *self.xmp_inflight.borrow_mut().entry(path.clone()).or_insert(0) += 1;
        let _ = self.xmp_tx.send(XmpJob { path, rating, rejected });
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
                s.toast(&format!("Couldn't write XMP for {n} file{}", if n == 1 { "" } else { "s" }));
            });
        }
    }

    /// Retries sidecar writes that failed or never ran (e.g. the app was closed).
    fn flush_dirty_xmp(&self) {
        let Some(folder) = self.folder() else { return };
        let dirty = self.index.borrow().dirty_records(&folder).unwrap_or_default();
        for rec in dirty {
            let in_flight = self.xmp_inflight.borrow().contains_key(&rec.path);
            if !in_flight {
                self.write_xmp(rec.path, rec.rating, rec.rejected);
            }
        }
    }
```

- [ ] **Step 3: Replace `src/ui/keys.rs`**

```rust
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::state::AppState;
use crate::catalog::rating::RatingChange;

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    match (ctrl, key.to_lower().to_unicode()) {
        (true, Some('o')) => state.choose_folder(),
        (true, Some('f')) => state.focus_filters(),
        (false, Some(c @ '0'..='5')) => state.rate(RatingChange::Set(c as u8 - b'0')),
        (false, Some('x')) => state.rate(RatingChange::ToggleReject),
        (false, Some('+' | '=')) => state.zoom_thumbs(1),
        (false, Some('-')) => state.zoom_thumbs(-1),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
```

Keypad digits work too: `to_unicode()` maps `KP_1` to `'1'`.

- [ ] **Step 4: Wire the filter bar, unpinning and Ctrl+scroll in `src/ui/window.rs`**

Add `filterbar` to the `use super::{...}` list. After `header.pack_end(&counter);` add:

```rust
    let filters = filterbar::build(&state);
    header.set_title_widget(Some(&filters));
```

Change the `Widgets` literal to include `filters`:

```rust
    let _ = state.widgets.set(Widgets { window: window.clone(), toasts, stack, grid, counter, filters });
```

After the `connect_items_changed` block add:

```rust
    let s = state.clone();
    state.selection.connect_selection_changed(move |_, _, _| s.unpin());

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = state.clone();
    scroll.connect_scroll(move |ctl, _dx, dy| {
        if ctl.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK) {
            s.zoom_thumbs(if dy < 0.0 { 1 } else { -1 });
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    grid_scroller.add_controller(scroll);
```

Add `mod filterbar;` to `src/ui/mod.rs`.

- [ ] **Step 5: Build and test**

Run: `cargo build && cargo test`
Expected: builds, all tests pass.

- [ ] **Step 6: Manual check** (`cargo run --release -- ~/pb-test`)

- Select a JPEG-only item, press `4`: stars update; `cat ~/pb-test/<file>.jpg.xmp` shows `xmp:Rating="4"`.
- Select a RAW+JPEG pair, press `X`: the ✕ marker appears; **both** `.CR3.xmp` (or your RAW extension) and `.JPG.xmp` contain `-1`. The item stays visible while selected; click another item → it disappears (rejects hidden by default).
- Set the rating filter to "★ 3+": only ≥ 3 items remain; counter updates.
- Select several items with `Ctrl+A`, press `2`: all update.
- `Ctrl` + mouse wheel and `+`/`-` resize thumbnails; the size is kept after restarting.
- Rate an image, quit, run `sqlite3 ~/.cache/photobrowser/index.db "select rating, xmp_dirty from files limit 5"` → ratings present, `xmp_dirty = 0`.
- Read-only check: `chmod a-w ~/pb-test`, rate a file that has no sidecar yet → toast "Couldn't write XMP for 1 file"; `chmod u+w ~/pb-test`, reopen the folder → the sidecar is written (dirty retry).

- [ ] **Step 7: Commit**

```bash
git add src
git commit -m "feat: ratings with XMP sidecars, filter bar and thumbnail zoom

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: Full-size viewer with prefetch, zoom, info overlay and filmstrip

**Files:**
- Create: `src/ui/viewer.rs`
- Modify: `src/ui/state.rs`, `src/ui/window.rs`, `src/ui/grid.rs`, `src/ui/keys.rs` (replace), `src/ui/mod.rs` (add `mod viewer;`)

**Interfaces:**
- Consumes: `decode::{decode_for_display, decode_actual_size, Quality}`, `exif::{read_info, ExifInfo}`, `grid::thumb_factory`
- Produces:
  - `ui::state::Zoom { Fit, Actual }`; `Msg::Full`, `Msg::Exif`; `Widgets.viewer: Rc<Viewer>`
  - `AppState`: `viewer_active()`, `current_position() -> Option<u32>`, `open_viewer(u32)`, `close_viewer()`, `show_at(u32)`, `step(i64)`, `toggle_zoom()`, `toggle_info()`, `request_full(&ItemObject, Zoom, Priority)`; `rating_targets()` returns the current item while the viewer is open; `rate()` refreshes the overlay and honours `auto_advance`.
  - `ui::viewer::Viewer` (`root: gtk::Overlay`; `new(&Rc<AppState>) -> Rc<Viewer>`, `current()`, `current_id()`, `set_current(&ItemObject, &str)`, `set_texture(&gdk::Texture)`, `zoom()`, `set_zoom(Zoom)`, `set_info(&str)`, `toggle_info() -> bool`, `info_visible()`, `toggle_strip()`)
  - `ui::viewer::info_text(&Item, Option<&ExifInfo>) -> String`

In the viewer, the selection holds exactly the current item, so the grid and the filmstrip stay in sync and a refilter never loses the current position.

- [ ] **Step 1: Write `src/ui/viewer.rs` with tests for `info_text`**

```rust
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::gdk;

use super::grid::thumb_factory;
use super::item_object::{stars_text, ItemObject};
use super::state::{AppState, Zoom};
use crate::exif::ExifInfo;
use crate::media::{Item, ItemId};

/// Overlay text: file names, rating, EXIF summary.
pub fn info_text(item: &Item, exif: Option<&ExifInfo>) -> String {
    let status = if item.rejected {
        "Rejected".to_string()
    } else if item.rating == 0 {
        "Unrated".to_string()
    } else {
        stars_text(item.rating)
    };
    let mut lines = vec![item.file_names(), status];
    if let Some(summary) = exif.map(ExifInfo::summary).filter(|s| !s.is_empty()) {
        lines.push(summary);
    }
    lines.join("\n")
}

pub struct Viewer {
    pub root: gtk::Overlay,
    scroller: gtk::ScrolledWindow,
    picture: gtk::Picture,
    info: gtk::Label,
    strip_revealer: gtk::Revealer,
    zoom: Cell<Zoom>,
    current: RefCell<Option<ItemObject>>,
    drag_origin: Cell<(f64, f64)>,
}

impl Viewer {
    pub fn new(state: &Rc<AppState>) -> Rc<Viewer> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&picture)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .vexpand(true)
            .build();

        let strip = gtk::ListView::new(Some(state.selection.clone()), Some(thumb_factory(state, true)));
        strip.set_orientation(gtk::Orientation::Horizontal);
        strip.set_single_click_activate(true);
        let strip_scroller = gtk::ScrolledWindow::builder()
            .child(&strip)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(96)
            .build();
        let strip_revealer = gtk::Revealer::builder()
            .child(&strip_scroller)
            .reveal_child(true)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&scroller);
        column.append(&strip_revealer);

        let info = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .xalign(0.0)
            .visible(false)
            .build();
        info.add_css_class("pb-osd");

        let root = gtk::Overlay::new();
        root.set_child(Some(&column));
        root.add_overlay(&info);
        root.add_css_class("pb-viewer");

        let viewer = Rc::new(Viewer {
            root,
            scroller,
            picture,
            info,
            strip_revealer,
            zoom: Cell::new(Zoom::Fit),
            current: RefCell::new(None),
            drag_origin: Cell::new((0.0, 0.0)),
        });

        let click = gtk::GestureClick::new();
        let s = state.clone();
        click.connect_pressed(move |_, n_press, _, _| {
            if n_press == 2 {
                s.toggle_zoom();
            }
        });
        viewer.scroller.add_controller(click);

        let drag = gtk::GestureDrag::new();
        let v = Rc::downgrade(&viewer);
        drag.connect_drag_begin(move |_, _, _| {
            if let Some(v) = v.upgrade() {
                v.drag_origin
                    .set((v.scroller.hadjustment().value(), v.scroller.vadjustment().value()));
            }
        });
        let v = Rc::downgrade(&viewer);
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(v) = v.upgrade() {
                let (h, vv) = v.drag_origin.get();
                v.scroller.hadjustment().set_value(h - dx);
                v.scroller.vadjustment().set_value(vv - dy);
            }
        });
        viewer.scroller.add_controller(drag);

        let s = state.clone();
        strip.connect_activate(move |_, pos| s.show_at(pos));
        viewer
    }

    pub fn current(&self) -> Option<ItemObject> {
        self.current.borrow().clone()
    }

    pub fn current_id(&self) -> Option<ItemId> {
        self.current.borrow().as_ref().map(ItemObject::id)
    }

    /// Shows `obj` at fit-to-window; the caller supplies the first texture.
    pub fn set_current(&self, obj: &ItemObject, info: &str) {
        *self.current.borrow_mut() = Some(obj.clone());
        self.set_zoom(Zoom::Fit);
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.info.set_label(info);
    }

    pub fn set_texture(&self, texture: &gdk::Texture) {
        self.picture.set_paintable(Some(texture));
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom.get()
    }

    pub fn set_zoom(&self, zoom: Zoom) {
        self.zoom.set(zoom);
        let (policy, shrink) = match zoom {
            Zoom::Fit => (gtk::PolicyType::Never, true),
            Zoom::Actual => (gtk::PolicyType::Automatic, false),
        };
        self.scroller.set_policy(policy, policy);
        self.picture.set_can_shrink(shrink);
    }

    pub fn set_info(&self, text: &str) {
        self.info.set_label(text);
    }

    pub fn info_visible(&self) -> bool {
        self.info.is_visible()
    }

    pub fn toggle_info(&self) -> bool {
        let visible = !self.info.is_visible();
        self.info.set_visible(visible);
        visible
    }

    pub fn toggle_strip(&self) {
        self.strip_revealer
            .set_reveal_child(!self.strip_revealer.reveals_child());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{FileKind, MediaFile};
    use std::path::PathBuf;

    fn item(rating: u8, rejected: bool) -> Item {
        let mut i = Item::new(vec![MediaFile {
            path: PathBuf::from("/p/IMG_1.JPG"),
            kind: FileKind::Jpeg,
            size: 1,
            mtime_ns: 0,
        }]);
        i.rating = rating;
        i.rejected = rejected;
        i
    }

    #[test]
    fn info_shows_rating_state() {
        assert_eq!(info_text(&item(0, false), None), "IMG_1.JPG\nUnrated");
        assert_eq!(info_text(&item(3, false), None), "IMG_1.JPG\n★★★☆☆");
        assert_eq!(info_text(&item(3, true), None), "IMG_1.JPG\nRejected");
    }

    #[test]
    fn info_appends_exif_summary() {
        let exif = ExifInfo { iso: Some(200), ..Default::default() };
        assert_eq!(info_text(&item(1, false), Some(&exif)), "IMG_1.JPG\n★☆☆☆☆\nISO 200");
        assert_eq!(info_text(&item(1, false), Some(&ExifInfo::default())), "IMG_1.JPG\n★☆☆☆☆");
    }
}
```

- [ ] **Step 2: Extend `src/ui/state.rs`**

Add imports: `use std::collections::HashSet;`, `use crate::exif::{self, ExifInfo};`, `use super::viewer::{info_text, Viewer};`.

Add the zoom type (near `Msg`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Zoom {
    Fit,
    Actual,
}
```

Add variants to `Msg`:

```rust
    Full { id: ItemId, generation: u64, zoom: Zoom, result: Result<Rgba, String> },
    Exif { id: ItemId, info: ExifInfo },
```

Add `pub viewer: Rc<Viewer>,` to `Widgets`.

Add fields to `AppState`:

```rust
    full_cache: RefCell<LruCache<(ItemId, Zoom), gdk::Texture>>,
    pending_full: RefCell<HashSet<(ItemId, Zoom)>>,
    exif_cache: RefCell<HashMap<ItemId, ExifInfo>>,
```

In `new`, read `let full_cap = config.full_cache_items + 2;` next to `thumb_cap` (+2 leaves room for 100 % decodes of the current and previous image) and initialise:

```rust
            full_cache: RefCell::new(LruCache::new(capacity(full_cap))),
            pending_full: RefCell::default(),
            exif_cache: RefCell::default(),
```

In `reset`, add:

```rust
        self.full_cache.borrow_mut().clear();
        self.pending_full.borrow_mut().clear();
        self.exif_cache.borrow_mut().clear();
        if let Some(w) = self.widgets.get() {
            if w.stack.visible_child_name().as_deref() == Some("viewer") {
                w.stack.set_visible_child_name("grid");
            }
        }
```

Add match arms in `handle`:

```rust
            Msg::Full { id, generation, zoom, result } => self.on_full(id, generation, zoom, result),
            Msg::Exif { id, info } => self.on_exif(id, info),
```

Replace `rating_targets`:

```rust
    pub fn rating_targets(&self) -> Vec<ItemObject> {
        if self.viewer_active() {
            return self.widgets().viewer.current().into_iter().collect();
        }
        self.selected_objects()
    }
```

At the end of `rate` (after the `for obj in &targets` loop) add:

```rust
        if self.viewer_active() {
            self.refresh_info();
            if self.config.borrow().auto_advance {
                self.step(1);
            }
        }
```

Add the viewer methods:

```rust
    // ---- viewer --------------------------------------------------------------

    pub fn viewer_active(&self) -> bool {
        self.widgets().stack.visible_child_name().as_deref() == Some("viewer")
    }

    pub fn current_position(&self) -> Option<u32> {
        let set = self.selection.selection();
        (!set.is_empty()).then(|| set.minimum())
    }

    pub fn open_viewer(self: &Rc<Self>, pos: u32) {
        self.widgets().stack.set_visible_child_name("viewer");
        self.show_at(pos);
    }

    pub fn close_viewer(&self) {
        let w = self.widgets();
        w.stack.set_visible_child_name("grid");
        if let Some(pos) = self.current_position() {
            w.grid.scroll_to(pos, gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT, None);
        }
    }

    pub fn show_at(self: &Rc<Self>, pos: u32) {
        let Some(obj) = self.selection.item(pos).and_downcast::<ItemObject>() else { return };
        self.selection.select_item(pos, true);
        let viewer = self.widgets().viewer.clone();
        let text = self.info_for(&obj);
        viewer.set_current(&obj, &text);

        let cached = self.full_cache.borrow_mut().get(&(obj.id(), Zoom::Fit)).cloned();
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
        let n = self.selection.n_items() as i64;
        for delta in [1i64, -1, 2, -2] {
            let p = pos as i64 + delta;
            if (0..n).contains(&p) {
                if let Some(o) = self.selection.item(p as u32).and_downcast::<ItemObject>() {
                    self.request_full(&o, Zoom::Fit, Priority::Prefetch);
                }
            }
        }
    }

    pub fn step(self: &Rc<Self>, delta: i64) {
        let Some(cur) = self.current_position() else { return };
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
        if viewer.toggle_info() {
            if let Some(obj) = viewer.current() {
                self.request_exif(&obj);
            }
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
        if self.full_cache.borrow().contains(&key) || self.pending_full.borrow().contains(&key) {
            return;
        }
        self.pending_full.borrow_mut().insert(key);
        let item = obj.item();
        let edge = self.screen_edge();
        let (tx, generation) = (self.tx.clone(), self.generation.get());
        self.pool.submit(priority, CancelToken::new(), move || {
            let result = decode::guarded(|| match zoom {
                Zoom::Fit => decode::decode_for_display(item.display_file(), edge, Quality::High),
                Zoom::Actual => decode::decode_actual_size(&item),
            })
            .map_err(|e| e.to_string());
            let _ = tx.send_blocking(Msg::Full { id: item.id, generation, zoom, result });
        });
    }

    fn on_full(&self, id: ItemId, generation: u64, zoom: Zoom, result: Result<Rgba, String>) {
        self.pending_full.borrow_mut().remove(&(id, zoom));
        if generation != self.generation.get() {
            return;
        }
        let viewer = self.widgets().viewer.clone();
        let is_current = viewer.current_id() == Some(id);
        match result {
            Ok(img) => {
                let texture = texture_from(img);
                self.full_cache.borrow_mut().push((id, zoom), texture.clone());
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
        let (id, path) = obj.with_item(|i| (i.id, i.raw_file().unwrap_or(i.display_file()).path.clone()));
        let tx = self.tx.clone();
        self.pool.submit(Priority::Visible, CancelToken::new(), move || {
            let _ = tx.send_blocking(Msg::Exif { id, info: exif::read_info(&path) });
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
```

If `NativeExt::surface()` returns `gdk::Surface` rather than `Option<gdk::Surface>` in your gtk4-rs version, replace `.surface().and_then(|surface| ...)` with `display.monitor_at_surface(&window.surface())`.

- [ ] **Step 3: Add the viewer to `src/ui/window.rs`**

Add `viewer::Viewer` to the imports (`use super::viewer::Viewer;`). After `stack.add_named(&grid_scroller, Some("grid"));` add:

```rust
    let viewer = Viewer::new(&state);
    stack.add_named(&viewer.root, Some("viewer"));
```

Include `viewer` in the `Widgets` literal:

```rust
    let _ = state.widgets.set(Widgets { window: window.clone(), toasts, stack, grid, counter, filters, viewer });
```

- [ ] **Step 4: Open the viewer from the grid (`src/ui/grid.rs`)**

In `build`, before `grid` is returned:

```rust
    let s = state.clone();
    grid.connect_activate(move |_, pos| s.open_viewer(pos));
```

- [ ] **Step 5: Replace `src/ui/keys.rs`**

```rust
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};

use super::state::AppState;
use crate::catalog::rating::RatingChange;

pub fn handle(state: &Rc<AppState>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
    if state.widgets().window.visible_dialog().is_some() {
        return glib::Propagation::Proceed;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let ch = key.to_lower().to_unicode();

    if state.viewer_active() && !ctrl {
        match key {
            gdk::Key::Escape => state.close_viewer(),
            gdk::Key::Right | gdk::Key::Down | gdk::Key::space => state.step(1),
            gdk::Key::Left | gdk::Key::Up | gdk::Key::BackSpace => state.step(-1),
            _ => match ch {
                Some('z') => state.toggle_zoom(),
                Some('i') => state.toggle_info(),
                Some('f') => state.widgets().viewer.toggle_strip(),
                _ => return handle_common(state, ctrl, ch),
            },
        }
        return glib::Propagation::Stop;
    }
    handle_common(state, ctrl, ch)
}

fn handle_common(state: &Rc<AppState>, ctrl: bool, ch: Option<char>) -> glib::Propagation {
    match (ctrl, ch) {
        (true, Some('o')) => state.choose_folder(),
        (true, Some('f')) => state.focus_filters(),
        (false, Some(c @ '0'..='5')) => state.rate(RatingChange::Set(c as u8 - b'0')),
        (false, Some('x')) => state.rate(RatingChange::ToggleReject),
        (false, Some('+' | '=')) if !state.viewer_active() => state.zoom_thumbs(1),
        (false, Some('-')) if !state.viewer_active() => state.zoom_thumbs(-1),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
```

Add `mod viewer;` to `src/ui/mod.rs`.

- [ ] **Step 6: Build and test**

Run: `cargo build && cargo test`
Expected: builds; all tests pass including the 2 `viewer` tests.

- [ ] **Step 7: Manual check** (`cargo run --release -- ~/pb-test`)

- `Enter` or double-click a thumbnail: the image fills the window (black background), sharp within a moment; the filmstrip shows the current item selected.
- Hold `→`: navigation is instant for prefetched images (no blank frames); `←`, `Space`, `Backspace` work; the grid selection follows.
- `Z` on a RAW+JPEG pair: after ~1 s the full LibRaw decode appears at 100 %; drag pans; `Z` again fits. `Z` on a JPEG shows it at native size.
- `I` shows filename, rating and EXIF (camera, lens, shutter, aperture, ISO, focal length); rating with `3` updates it.
- `F` hides/shows the filmstrip.
- With "★ 3+" active: rate the current image `1` → it stays; press `→` → the previous image disappears from the filmstrip; the viewer continues with the next one.
- `Esc` returns to the grid with the last viewed item selected and scrolled into view.
- A portrait RAW (camera rotated) is upright in grid and viewer.

- [ ] **Step 8: Commit**

```bash
git add src
git commit -m "feat: full-size viewer with prefetch, 100% zoom, info overlay and filmstrip

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: Move and copy from the UI

**Files:**
- Create: `src/ui/dialogs.rs`
- Modify: `src/ui/state.rs`, `src/ui/keys.rs`, `src/ui/mod.rs` (add `mod dialogs;`)

**Interfaces:**
- Consumes: `fileops::{transfer_item, has_conflict, OpKind, ConflictPolicy, TransferOutcome}`, `index::{add_recent_destination, recent_destinations, rename_path, get, upsert_many}`, `xmp::is_sidecar`
- Produces: `dialogs::choose_destination(&Rc<AppState>, OpKind)`; `AppState::{start_transfer(Vec<ItemObject>, PathBuf, OpKind), move_to_last()}`; `Msg::{Transferred, TransfersFinished}`.

One conflict prompt covers the whole batch ("Skip Existing" / "Keep Both" / "Cancel"), which is the spec's "apply to all" choice made up front. Transfers run on their own thread, never on the decode pool, so thumbnails keep loading.

- [ ] **Step 1: Write `src/ui/dialogs.rs`**

```rust
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use super::state::AppState;
use crate::fileops::OpKind;

/// "Move/Copy to…": recent destinations plus a folder chooser.
pub fn choose_destination(state: &Rc<AppState>, kind: OpKind) {
    let targets = state.rating_targets();
    if targets.is_empty() {
        state.toast("Select one or more images first");
        return;
    }
    let title = match kind {
        OpKind::Move => "Move to Folder",
        OpKind::Copy => "Copy to Folder",
    };
    let dialog = adw::Dialog::builder().title(title).content_width(460).build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let recent: Vec<_> = state
        .index
        .borrow()
        .recent_destinations()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.is_dir() && Some(d) != state.folder().as_ref())
        .collect();
    if !recent.is_empty() {
        let heading = gtk::Label::builder().label("Recent folders").xalign(0.0).build();
        heading.add_css_class("heading");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        for dir in recent {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.display().to_string());
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&name).as_str())
                .subtitle(glib::markup_escape_text(&dir.to_string_lossy()).as_str())
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (s, dlg, t) = (state.clone(), dialog.clone(), targets.clone());
            row.connect_activated(move |_| {
                dlg.close();
                s.start_transfer(t.clone(), dir.clone(), kind);
            });
            list.append(&row);
        }
        content.append(&heading);
        content.append(&list);
    }

    let choose = gtk::Button::with_label("Choose Folder…");
    choose.add_css_class("pill");
    choose.set_halign(gtk::Align::Center);
    let (s, dlg) = (state.clone(), dialog.clone());
    choose.connect_clicked(move |_| {
        dlg.close();
        let chooser = gtk::FileDialog::builder().title(title).modal(true).build();
        let s2 = s.clone();
        let t = targets.clone();
        chooser.select_folder(Some(&s.widgets().window), gio::Cancellable::NONE, move |res| {
            if let Some(path) = res.ok().and_then(|f| f.path()) {
                s2.start_transfer(t, path, kind);
            }
        });
    });
    content.append(&choose);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&state.widgets().window));
}
```

- [ ] **Step 2: Extend `src/ui/state.rs`**

Add imports:

```rust
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fileops::{self, ConflictPolicy, OpKind, TransferOutcome};
```

Add variants to `Msg`:

```rust
    Transferred { id: ItemId, kind: OpKind, outcome: TransferOutcome },
    TransfersFinished { kind: OpKind, dest: PathBuf, done: usize, skipped: usize, failed: usize },
```

Add a field `viewer_resume: Cell<Option<u32>>,` to `AppState`, initialised with `viewer_resume: Cell::new(None),`.

Add match arms in `handle`:

```rust
            Msg::Transferred { id, kind, outcome } => self.on_transferred(id, kind, outcome),
            Msg::TransfersFinished { kind, dest, done, skipped, failed } => {
                self.on_transfers_finished(kind, &dest, done, skipped, failed)
            }
```

Add methods:

```rust
    // ---- move / copy ---------------------------------------------------------

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
        let jobs: Vec<(ItemId, Vec<PathBuf>)> = targets
            .iter()
            .map(|o| o.with_item(|i| (i.id, i.files.iter().map(|f| f.path.clone()).collect())))
            .collect();
        let conflicts = jobs.iter().filter(|(_, src)| fileops::has_conflict(src, &dest)).count();
        if conflicts == 0 {
            self.run_transfer(jobs, dest, kind, ConflictPolicy::Skip);
            return;
        }
        let body = format!(
            "{conflicts} of {} item{} already exist in “{}”.",
            jobs.len(),
            if jobs.len() == 1 { "" } else { "s" },
            dest.display()
        );
        let dialog = adw::AlertDialog::new(Some("Files Already Exist"), Some(&body));
        dialog.add_responses(&[("cancel", "_Cancel"), ("skip", "_Skip Existing"), ("rename", "_Keep Both")]);
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
            s.run_transfer(jobs.clone(), dest.clone(), kind, policy);
        });
        dialog.present(Some(&self.widgets().window));
    }

    fn run_transfer(
        self: &Rc<Self>,
        jobs: Vec<(ItemId, Vec<PathBuf>)>,
        dest: PathBuf,
        kind: OpKind,
        policy: ConflictPolicy,
    ) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let _ = self.index.borrow().add_recent_destination(&dest, now);
        self.viewer_resume
            .set(if self.viewer_active() { self.current_position() } else { None });
        let verb = match kind {
            OpKind::Move => "Moving",
            OpKind::Copy => "Copying",
        };
        self.toast(&format!("{verb} {} item{}…", jobs.len(), if jobs.len() == 1 { "" } else { "s" }));
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("pb-fileops".into())
            .spawn(move || {
                let (mut done, mut skipped, mut failed) = (0, 0, 0);
                for (id, sources) in jobs {
                    let outcome = fileops::transfer_item(&sources, &dest, kind, policy);
                    match &outcome {
                        TransferOutcome::Done(_) => done += 1,
                        TransferOutcome::Skipped => skipped += 1,
                        TransferOutcome::Failed(e) => {
                            eprintln!("transfer failed: {e}");
                            failed += 1;
                        }
                    }
                    let _ = tx.send_blocking(Msg::Transferred { id, kind, outcome });
                }
                let _ = tx.send_blocking(Msg::TransfersFinished { kind, dest, done, skipped, failed });
            })
            .expect("failed to spawn file operation thread");
    }

    fn on_transferred(&self, id: ItemId, kind: OpKind, outcome: TransferOutcome) {
        let TransferOutcome::Done(pairs) = outcome else { return };
        let media: Vec<&(PathBuf, PathBuf)> = pairs.iter().filter(|(src, _)| !xmp::is_sidecar(src)).collect();
        match kind {
            OpKind::Move => {
                {
                    let mut index = self.index.borrow_mut();
                    for (src, dst) in &media {
                        if let Err(e) = index.rename_path(src, dst) {
                            eprintln!("index: {e}");
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

    fn on_transfers_finished(
        self: &Rc<Self>,
        kind: OpKind,
        dest: &std::path::Path,
        done: usize,
        skipped: usize,
        failed: usize,
    ) {
        let verb = match kind {
            OpKind::Move => "Moved",
            OpKind::Copy => "Copied",
        };
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut text = format!("{verb} {done} item{} to “{name}”", if done == 1 { "" } else { "s" });
        if skipped > 0 {
            text.push_str(&format!(", {skipped} skipped"));
        }
        if failed > 0 {
            text.push_str(&format!(", {failed} failed"));
        }
        self.toast(&text);

        if let Some(pos) = self.viewer_resume.take() {
            let n = self.selection.n_items();
            if n == 0 {
                self.close_viewer();
            } else if self.current_position().is_none() {
                self.show_at(pos.min(n - 1));
            }
        }
    }
```

- [ ] **Step 3: Add the shortcuts to `src/ui/keys.rs`**

In `handle_common`, add `shift` handling. Replace the function with:

```rust
fn handle_common(state: &Rc<AppState>, ctrl: bool, shift: bool, ch: Option<char>) -> glib::Propagation {
    match (ctrl, ch) {
        (true, Some('o')) => state.choose_folder(),
        (true, Some('f')) => state.focus_filters(),
        (true, Some('m')) if shift => state.move_to_last(),
        (false, Some(c @ '0'..='5')) => state.rate(RatingChange::Set(c as u8 - b'0')),
        (false, Some('x')) => state.rate(RatingChange::ToggleReject),
        (false, Some('m')) => dialogs::choose_destination(state, OpKind::Move),
        (false, Some('c')) => dialogs::choose_destination(state, OpKind::Copy),
        (false, Some('+' | '=')) if !state.viewer_active() => state.zoom_thumbs(1),
        (false, Some('-')) if !state.viewer_active() => state.zoom_thumbs(-1),
        _ => return glib::Propagation::Proceed,
    }
    glib::Propagation::Stop
}
```

In `handle`, compute `let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);` next to `ctrl`, and pass it in both calls: `handle_common(state, ctrl, shift, ch)`. Add imports `use super::dialogs;` and `use crate::fileops::OpKind;`.

Add `mod dialogs;` to `src/ui/mod.rs`.

- [ ] **Step 4: Build and test**

Run: `cargo build && cargo test`
Expected: builds; all tests pass.

- [ ] **Step 5: Manual check** (`mkdir -p ~/pb-dest && cargo run --release -- ~/pb-test`)

- Select two items including a RAW+JPEG pair, press `M` → "Choose Folder…" → `~/pb-dest`: toast "Moved 2 items to “pb-dest”"; the items leave the grid; `ls ~/pb-dest` shows RAW, JPEG and both `.xmp` sidecars.
- Open `~/pb-dest`: the moved items keep their ratings.
- Back in `~/pb-test`, select another item, press `Ctrl+Shift+M`: it moves to `~/pb-dest` without a dialog.
- Press `C` on an item and pick `~/pb-dest` twice: the second time the conflict dialog appears; "Keep Both" creates `NAME_1.EXT` (+ sidecars `NAME_1.EXT.xmp`); "Skip Existing" leaves the destination unchanged.
- In the viewer, `M` moves the current image and the viewer continues with the next one.
- `M` → recent list shows `pb-dest` first.
- Move everything back: `mv ~/pb-dest/* ~/pb-test/`.

- [ ] **Step 6: Commit**

```bash
git add src
git commit -m "feat: move/copy selected items with conflict prompt and recent destinations

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 17: Folder monitoring, preferences, benchmarks, README and acceptance

**Files:**
- Create: `src/ui/prefs.rs`, `benches/pipeline.rs`, `README.md`
- Modify: `src/ui/state.rs`, `src/ui/window.rs`, `src/ui/mod.rs` (add `mod prefs;`), `Cargo.toml` (bench target)

**Interfaces:**
- Consumes: all previous tasks
- Produces: live folder updates (`gio::FileMonitor`, 250 ms debounce), "folder lost" handling, `prefs::show(&Rc<AppState>)`, `cargo bench` suite, README.

- [ ] **Step 1: Watch the open folder (`src/ui/state.rs`)**

Add fields to `AppState`:

```rust
    monitor: RefCell<Option<gio::FileMonitor>>,
    rescan_source: RefCell<Option<glib::SourceId>>,
```

initialised with `monitor: RefCell::new(None), rescan_source: RefCell::new(None),`.

In `reset`, add:

```rust
        if let Some(monitor) = self.monitor.borrow_mut().take() {
            monitor.cancel();
        }
        if let Some(source) = self.rescan_source.borrow_mut().take() {
            source.remove();
        }
```

In `open_folder`, after `self.start_scan(folder, known);` change it to:

```rust
        self.start_scan(folder.clone(), known);
        self.watch_folder(&folder);
```

Add methods:

```rust
    // ---- folder monitoring ---------------------------------------------------

    fn watch_folder(self: &Rc<Self>, folder: &std::path::Path) {
        let dir = gio::File::for_path(folder);
        let monitor = match dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
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
                gio::FileMonitorEvent::Deleted | gio::FileMonitorEvent::MovedOut if is_dir_itself => {
                    s.folder_lost("The folder was removed");
                }
                gio::FileMonitorEvent::Unmounted => s.folder_lost("The folder's drive was unmounted"),
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
```

Our own sidecar writes trigger rescans too; that is cheap (a directory listing plus `stat`s) and harmless, because records are marked dirty until their write completes and a synced sidecar has `mtime == xmp_synced_at`.

- [ ] **Step 2: Write `src/ui/prefs.rs`**

```rust
use std::rc::Rc;

use adw::prelude::*;

use super::state::AppState;
use crate::config::Config;

pub fn show(state: &Rc<AppState>) {
    let cfg = state.config.borrow().clone();

    let browsing = adw::PreferencesGroup::builder().title("Browsing").build();
    let advance = adw::SwitchRow::builder()
        .title("Advance After Rating")
        .subtitle("In the full view, go to the next image after setting a rating")
        .active(cfg.auto_advance)
        .build();
    browsing.add(&advance);

    let caches = adw::PreferencesGroup::builder()
        .title("Caches")
        .description("Changes take effect after restarting")
        .build();
    let thumbs = adw::SpinRow::with_range(50.0, 5000.0, 50.0);
    thumbs.set_title("Thumbnails Kept in Memory");
    thumbs.set_value(cfg.thumb_cache_items as f64);
    let fulls = adw::SpinRow::with_range(2.0, 30.0, 1.0);
    fulls.set_title("Full-Size Images Kept in Memory");
    fulls.set_value(cfg.full_cache_items as f64);
    let disk = adw::SpinRow::with_range(100.0, 100_000.0, 100.0);
    disk.set_title("Thumbnail Disk Cache (MB)");
    disk.set_value(cfg.disk_cache_mb as f64);
    caches.add(&thumbs);
    caches.add(&fulls);
    caches.add(&disk);

    let page = adw::PreferencesPage::new();
    page.add(&browsing);
    page.add(&caches);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);

    let s = state.clone();
    dialog.connect_closed(move |_| {
        let mut c = s.config.borrow_mut();
        c.auto_advance = advance.is_active();
        c.thumb_cache_items = thumbs.value() as usize;
        c.full_cache_items = fulls.value() as usize;
        c.disk_cache_mb = disk.value() as u64;
        let _ = c.save_to(&Config::default_path());
    });
    dialog.present(Some(&state.widgets().window));
}
```

- [ ] **Step 3: Add a menu with Preferences (`src/ui/window.rs`)**

Add `prefs` to the `use super::{...}` list and `use gtk::gio;`. After `header.pack_end(&counter);` add:

```rust
    let menu = gio::Menu::new();
    menu.append(Some("Preferences"), Some("win.preferences"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Main Menu")
        .build();
    header.pack_end(&menu_button);
```

After the `Widgets` are set, add:

```rust
    let s = state.clone();
    let preferences = gio::ActionEntry::builder("preferences")
        .activate(move |_: &adw::ApplicationWindow, _, _| prefs::show(&s))
        .build();
    window.add_action_entries([preferences]);
```

(`pack_end` places widgets right-to-left, so the menu ends up right of the counter.)

Add `mod prefs;` to `src/ui/mod.rs`.

- [ ] **Step 4: Write `benches/pipeline.rs`**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use criterion::{criterion_group, criterion_main, Criterion};
use photobrowser::catalog::sync::scan_folder;
use photobrowser::decode::{decode_for_display, Quality, THUMB_EDGE};
use photobrowser::media::{FileKind, MediaFile};

fn write_gradient(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]))
        .save(path)
        .unwrap();
}

fn media(path: PathBuf) -> MediaFile {
    let kind = FileKind::from_path(&path).unwrap();
    MediaFile { path, kind, size: 0, mtime_ns: 0 }
}

fn bench_thumbnails(c: &mut Criterion) {
    photobrowser::exif::init();
    let dir = tempfile::tempdir().unwrap();
    let mut cases = vec![
        ("jpeg_24mp", dir.path().join("big.jpg"), (6000, 4000)),
        ("png_6mp", dir.path().join("mid.png"), (3000, 2000)),
        ("tiff_6mp", dir.path().join("mid.tif"), (3000, 2000)),
    ];
    for (_, path, (w, h)) in &cases {
        write_gradient(path, *w, *h);
    }
    if let Ok(samples) = std::env::var("PB_RAW_SAMPLES") {
        let raw = std::fs::read_dir(samples)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| FileKind::from_path(p) == Some(FileKind::Raw));
        if let Some(raw) = raw {
            cases.push(("raw_preview", raw, (0, 0)));
        }
    }
    for (name, path, _) in cases {
        let file = media(path);
        c.bench_function(&format!("thumbnail_{name}"), |b| {
            b.iter(|| decode_for_display(&file, THUMB_EDGE, Quality::Fast).unwrap())
        });
    }
}

fn bench_scan(c: &mut Criterion) {
    photobrowser::exif::init();
    let dir = tempfile::tempdir().unwrap();
    for i in 0..2000 {
        write_gradient(&dir.path().join(format!("IMG_{i:04}.jpg")), 64, 48);
    }
    c.bench_function("scan_2000_new", |b| {
        b.iter(|| scan_folder(dir.path(), &HashMap::new()).unwrap())
    });
    let first = scan_folder(dir.path(), &HashMap::new()).unwrap();
    let known: HashMap<_, _> = first.upserts.into_iter().map(|r| (r.path.clone(), r)).collect();
    c.bench_function("scan_2000_known", |b| b.iter(|| scan_folder(dir.path(), &known).unwrap()));
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_thumbnails, bench_scan
}
criterion_main!(benches);
```

Append to `Cargo.toml`:

```toml
[[bench]]
name = "pipeline"
harness = false
```

Run: `cargo bench` (optionally with `PB_RAW_SAMPLES=/path/to/raw-samples`)
Expected, on a typical laptop: `thumbnail_jpeg_24mp` ≲ 15 ms, `scan_2000_known` ≲ 50 ms (the "re-open" path; the grid itself is shown from the index before the scan finishes). Record the numbers in the README; investigate if `scan_2000_known` exceeds 200 ms.

- [ ] **Step 5: Check RawTherapee compatibility**

Rate a RAW file 4 in Photobrowser, then open its folder in RawTherapee's file browser. Note whether RawTherapee shows the 4-star rank (RawTherapee 5.10+ may read `xmp:Rating` from sidecars; older versions keep ranks only in `.pp3`/cache). Write the observed result, with the RawTherapee version, into the README section below.

- [ ] **Step 6: Write `README.md`**

````markdown
# Photobrowser

Fast GTK 4 photo culling for Linux: browse a folder of JPEG, PNG, TIFF and RAW
files, rate them, filter, and move or copy the keepers. RAW+JPEG pairs behave
as one image. Ratings are written to darktable-compatible XMP sidecars.

## Build

```bash
sudo dnf install -y rust cargo gtk4-devel libadwaita-devel LibRaw-devel \
  libgexiv2_0.14-devel turbojpeg-devel sqlite-devel clang-devel
cargo build --release
./target/release/photobrowser [FOLDER]
```

## Keyboard

| Key | Action |
|---|---|
| `Ctrl+O` | Open folder |
| `1`–`5` / `0` | Set / clear rating |
| `X` | Toggle reject |
| `M` / `C` | Move / copy to folder… |
| `Ctrl+Shift+M` | Move to the last destination |
| `Ctrl+F` | Focus the filter bar |
| `Enter`, double-click | Open full view |
| `←` `→` `Space` `Backspace` | Previous / next (full view) |
| `Z`, double-click | Fit ↔ 100 % (RAW: full decode) |
| `I` | Info overlay |
| `F` | Filmstrip |
| `Esc` | Back to the grid |
| `Ctrl`+wheel, `+` / `-` | Thumbnail size |

Ratings apply to the selection in the grid and to the current image in the full view.

## Where things are stored

- Ratings: `IMG_001.CR3.xmp` next to each file (`xmp:Rating`, `-1` = rejected).
  Only the rating is changed; darktable edits in the sidecar are preserved.
- Index: `~/.cache/photobrowser/index.db` (safe to delete; rebuilt from sidecars).
- Thumbnails: `~/.cache/photobrowser/thumbs/` (capped, default 2 GB).
- Preferences: `~/.config/photobrowser/config.toml`.

## Compatibility

- **darktable** reads the ratings when importing. For folders already imported,
  enable *preferences → storage → look for updated XMP files on startup*.
- **RawTherapee**: <result of Task 17 Step 5, with version>.

## Development

```bash
cargo test
PB_RAW_SAMPLES=/path/to/raw-samples cargo test   # also exercises RAW decoding
cargo bench
```

Benchmarks on <machine>: <numbers from Task 17 Step 4>.
````

Replace the two `<…>` placeholders with the observed results before committing.

- [ ] **Step 7: Full verification**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo build --release`
Expected: all tests pass, no clippy errors, release build succeeds.

- [ ] **Step 8: Manual acceptance** (`./target/release/photobrowser ~/pb-test`)

- While the app is open, copy a new photo into `~/pb-test`: it appears within a second; delete it: it disappears.
- Edit a rating in darktable (or `exiv2 -M"set Xmp.xmp.Rating 5" FILE.xmp` if exiv2 CLI is installed) on a sidecar: the grid shows the new rating within a second.
- Rename `~/pb-test` in a file manager while it is open: toast "The folder was removed", empty state shown. Rename it back and reopen.
- Preferences → enable "Advance After Rating": in the full view, `3` rates and moves to the next image. Settings survive a restart.
- On a real card-sized folder (1,000–2,000 of your RAW+JPEG shots): first open streams thumbnails without UI stalls; the second open shows the grid with thumbnails in well under a second; holding `→` in the full view never shows blank frames once prefetch has caught up.
- darktable imports a rated folder and shows the same stars and rejects.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml src benches README.md
git commit -m "feat: live folder updates, preferences, benchmarks and README

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Spec coverage

| Spec section | Task(s) |
|---|---|
| §2 Technology, system packages | 1, 9 |
| §3.1 Supported files | 2 |
| §3.2 Pairing | 2, 5 |
| §3.3 Ratings and rejects | 6, 14 |
| §3.4 XMP sidecars (naming, preserve, import, dirty retry) | 4, 5, 14 |
| §3.5 Filtering, sorting, "stays visible" | 6, 14, 15 |
| §3.6 File operations (move/copy, pairs, conflicts, rollback, recent, Ctrl+Shift+M) | 11, 16 |
| §4 Architecture, §4.1 types, §4.2 index schema, §4.3 data flow | 2, 3, 5, 13 |
| §5.1 Performance targets | 17 (bench + acceptance) |
| §5.2 Decode strategy | 8, 9 |
| §5.3 Caching (disk, memory LRUs) | 10, 13, 15 |
| §5.4 Job scheduling | 7, 13, 15 |
| §6 UI (window, grid, full view, shortcuts, preferences) | 13–17 |
| §7 Error handling | 3, 4, 8, 9, 11, 13, 14, 17 |
| §8 Testing | every task; RAW via `PB_RAW_SAMPLES` (9); benches (17) |
