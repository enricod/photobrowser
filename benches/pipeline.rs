use std::collections::HashMap;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use photobrowser::catalog::sync::scan_folder;
use photobrowser::decode::{Quality, THUMB_EDGE, decode_for_display};
use photobrowser::media::{FileKind, MediaFile};

fn write_gradient(path: &Path, w: u32, h: u32) {
    image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    })
    .save(path)
    .unwrap();
}

fn media(path: PathBuf) -> MediaFile {
    let kind = FileKind::from_path(&path).unwrap();
    MediaFile {
        path,
        kind,
        size: 0,
        mtime_ns: 0,
    }
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
    let known: HashMap<_, _> = first
        .upserts
        .into_iter()
        .map(|r| (r.path.clone(), r))
        .collect();
    c.bench_function("scan_2000_known", |b| {
        b.iter(|| scan_folder(dir.path(), &known).unwrap())
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = bench_thumbnails, bench_scan
}
criterion_main!(benches);
