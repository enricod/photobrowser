# Photobrowser

Fast GTK 4 photo culling for Linux: browse a folder of JPEG, PNG, TIFF and RAW
files, rate them, filter, and move or copy the keepers. RAW+JPEG pairs behave
as one image. Ratings are written to darktable-compatible XMP sidecars.

## Build

Fedora:

```bash
sudo dnf install -y rust cargo gtk4-devel libadwaita-devel LibRaw-devel \
  libgexiv2_0.14-devel turbojpeg-devel sqlite-devel clang-devel
```

Debian / Ubuntu (Rust from [rustup](https://rustup.rs)):

```bash
sudo apt install -y pkg-config clang libclang-dev libgtk-4-dev libadwaita-1-dev \
  libraw-dev libgexiv2-dev libturbojpeg0-dev libsqlite3-dev
```

libadwaita 1.8 or newer is required (for the Keyboard Shortcuts dialog); check
with `pkg-config --modversion libadwaita-1`.

Either gexiv2 0.14 (`gexiv2`) or 0.16 (`gexiv2-0.16`) works; `build.rs` picks
whichever is installed. Then:

```bash
cargo build --release
./target/release/photobrowser [FOLDER]
```

A `Makefile` wraps the common commands:

| Target | What it does |
|---|---|
| `make help` | List targets (also the default when running plain `make`). |
| `make build` | `cargo build --release`. |
| `make run [DIR=folder]` | Build, then run `./target/release/photobrowser [DIR]`. |
| `make test` | `cargo test`. |
| `make test-raw RAW=folder` | `cargo test` with `PB_RAW_SAMPLES` set, to also exercise RAW decoding. `RAW` is required. |
| `make check` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. |
| `make bench [RAW=folder]` | `cargo bench`, including `PB_RAW_SAMPLES` when `RAW` is set. |
| `make install [PREFIX=dir]` | Build, then install the binary and a desktop entry so the app shows up in GNOME. Default `PREFIX` is `$HOME/.local`. |
| `make uninstall [PREFIX=dir]` | Remove what `make install` installed. |
| `make clean` | `cargo clean`. |

`make install` puts the binary at `$(PREFIX)/bin/photobrowser` and the desktop
entry at `$(PREFIX)/share/applications/io.github.enricod.PhotoBrowser.desktop`
(default prefix: `~/.local`). `~/.local/bin` is on `PATH` by default on
Fedora; if it isn't on yours, add it so the desktop entry's `Exec=photobrowser`
can find the binary. Use `make uninstall` (with the same `PREFIX`, if you
overrode it) to remove both files again.

## Keyboard

| Key | Action |
|---|---|
| `Ctrl+O` | Open folder |
| `Alt+↑` | Open the parent folder (also ↑ in the path bar) |
| `1`–`5` / `0` | Set / clear rating |
| `X` | Toggle reject |
| `M` / `C` | Move / copy to folder… |
| `Ctrl+Shift+M` | Move to the last destination |
| `Ctrl+F` | Focus the filter bar |
| `Ctrl+A` | Select all visible photos (grid) |
| `Ctrl+?` | Keyboard shortcuts (also in the main menu) |
| `Enter`, double-click | Open full view |
| `←` `→` `Space` `Backspace` | Previous / next (full view) |
| `Z`, double-click | Fit ↔ 100 % (RAW: full decode) |
| `I` | Info overlay |
| `F` | Filmstrip |
| `Esc` | Back to the grid |
| `Ctrl`+wheel, `+` / `-` | Thumbnail size |

Ratings apply to the selection in the grid and to the current image in the full view.

The main menu (top-right of the header bar) has **Move to…**, **Copy to…** and
**Organize…** (sort the selection into `YYYY/YYYY-MM-DD` folders by shot date),
**Keyboard Shortcuts**, a **Preferences** dialog for auto-advance after rating
and the in-memory/disk cache sizes, and **About Photobrowser**.

The bar under the header shows the open folder, with a ↑ button to open its
parent. Tick **Show folders** there to open a panel on the right listing its
subfolders: type in its filter box to narrow the list (Enter opens the first
match), and click a folder to open it.

## Where things are stored

- Ratings: `IMG_001.CR3.xmp` next to each file (`xmp:Rating`, `-1` = rejected).
  Only the rating is changed; darktable edits in the sidecar are preserved.
- Index: `~/.cache/photobrowser/index.db` (safe to delete; rebuilt from sidecars).
- Thumbnails: `~/.cache/photobrowser/thumbs/` (capped, default 2 GB).
- Preferences: `~/.config/photobrowser/config.toml`.

The open folder is watched live (file creation/deletion/rename, including the
folder itself being moved, renamed or unmounted): the grid updates within
about a second, and the app falls back to the empty state with a toast if the
folder disappears.

## Compatibility

- **darktable** reads the ratings when importing. For folders already imported,
  enable *preferences → storage → look for updated XMP files on startup*.
- **RawTherapee**: per RawTherapee documentation/source, not tested here.
  RawTherapee added the ability to read ranks from **external** `.xmp`
  sidecars in version 5.11 (feature request
  [#6084](https://github.com/Beep6581/RawTherapee/issues/6084), implemented by
  [PR #6988](https://github.com/Beep6581/RawTherapee/pull/6988), see the
  [5.11 release notes](https://rawtherapee.com/2024/08/rawtherapee-5.11-released/)).
  It is **opt-in** (a File Browser preference, off by default) and only
  1–5 star ranks are read; per the PR author, a rating of `-1` is *ignored*
  the same way it already is when reading XMP embedded in the raw/JPEG itself,
  so files this app has **rejected** will not show as rejected in RawTherapee,
  only their star rating (if any) will carry over. RawTherapee 5.13 (the
  version installed here, as the `com.rawtherapee.RawTherapee` flatpak) is a
  later 5.x release in the same line, so the feature should still be present,
  but this was not exercised by driving the RawTherapee GUI in this session.
  **To verify on your own install:** in RawTherapee, open *Preferences → File
  Browser* and enable the option to load rank/color from XMP sidecars (wording
  may vary slightly by version), restart RawTherapee, open a folder that this
  app has rated, and confirm the star count matches (a rejected item is
  expected to *not* show as rejected, per the behavior above).

## Development

```bash
cargo test
PB_RAW_SAMPLES=/path/to/raw-samples cargo test   # also exercises RAW decoding
cargo bench
```

Benchmarks on this machine (Intel(R) Core(TM) i7-10510U CPU @ 1.80GHz, 8
logical CPUs, `cargo bench` with `PB_RAW_SAMPLES` pointing at a folder with a
`.CR2`/`.ORF` sample), criterion medians:

| Benchmark | Time |
|---|---|
| `thumbnail_jpeg_24mp` (6000×4000 JPEG) | 27.9 ms |
| `thumbnail_png_6mp` (3000×2000 PNG) | 31.9 ms |
| `thumbnail_tiff_6mp` (3000×2000 TIFF) | 16.7 ms |
| `thumbnail_raw_preview` (embedded RAW preview) | 16.8 ms |
| `scan_2000_new` (2,000 files, empty index) | 90.5 ms |
| `scan_2000_known` (2,000 files, already indexed — the "re-open" path) | 7.3 ms |

`scan_2000_known` is well under the 200 ms target (the grid is shown from the
index before this rescan even finishes). The `thumbnail_*` numbers run higher
than the ≲15 ms rule of thumb from a low-power ultrabook CPU (single-core
turbo ~4.9 GHz but a 1.8 GHz base and thermally limited under sustained load);
they are not on the interactive path for reopening a folder, since thumbnails
are cached to disk after their first decode.
