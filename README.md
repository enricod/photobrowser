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

The main menu (top-right of the header bar) has a **Preferences** dialog for
auto-advance after rating and the in-memory/disk cache sizes.

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
