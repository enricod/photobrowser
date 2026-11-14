use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub auto_advance: bool,
    pub thumb_cache_items: usize,
    pub full_cache_items: usize,
    pub disk_cache_mb: u64,
    pub thumb_display_size: i32,
    /// Show the open folder's subfolders above the grid.
    pub show_folders: bool,
    /// Left out when saving if it is not valid UTF-8 (TOML strings must be), so it cannot
    /// make every save fail.
    #[serde(serialize_with = "utf8_path_or_none")]
    pub last_folder: Option<PathBuf>,
}

fn utf8_path_or_none<S: Serializer>(path: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
    match path.as_deref().and_then(Path::to_str) {
        Some(text) => s.serialize_some(text),
        None => s.serialize_none(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            auto_advance: false,
            thumb_cache_items: 300,
            full_cache_items: 6,
            disk_cache_mb: 2048,
            thumb_display_size: 192,
            show_folders: false,
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

    /// Loads the config, keeping every valid setting: a bad value only resets that setting.
    /// A file that was not fully valid is first copied to `<path>.bad-<timestamp>`, since the
    /// next save rewrites it. A missing file gives the defaults.
    pub fn load_from(path: &Path) -> Config {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Config::default(),
            Err(e) => {
                eprintln!("couldn't read {}: {e}; using defaults", path.display());
                return Config::default();
            }
        };
        let (config, problem) = parse_lenient(&text);
        if let Some(problem) = problem {
            let backup = back_up(path);
            eprintln!(
                "{}: {problem}; the original was saved as {}",
                path.display(),
                backup.map_or_else(
                    |e| format!("(backup failed: {e})"),
                    |b| b.display().to_string()
                )
            );
        }
        config
    }

    /// Writes atomically (temp file, sync, rename), so a crash or a full disk never leaves a
    /// truncated config behind.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let result = (|| {
            let mut file = fs::File::create(&tmp)?;
            io::Write::write_all(&mut file, text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

/// The config in `text`, plus a description of what was wrong with it, if anything.
fn parse_lenient(text: &str) -> (Config, Option<String>) {
    if let Ok(config) = toml::from_str::<Config>(text) {
        return (config, None);
    }
    let Ok(mut table) = toml::from_str::<toml::Table>(text) else {
        return (
            Config::default(),
            Some("not valid TOML; using defaults".into()),
        );
    };
    // Drop each setting that is invalid on its own; unknown keys are ignored by serde.
    let bad: Vec<String> = table
        .iter()
        .filter(|(key, value)| {
            let mut single = toml::Table::new();
            single.insert((*key).clone(), (*value).clone());
            toml::Value::Table(single).try_into::<Config>().is_err()
        })
        .map(|(key, _)| key.clone())
        .collect();
    for key in &bad {
        table.remove(key);
    }
    let config = toml::Value::Table(table)
        .try_into::<Config>()
        .unwrap_or_default();
    (
        config,
        Some(format!(
            "invalid setting(s) reset to default: {}",
            bad.join(", ")
        )),
    )
}

/// Copies `path` to `<path>.bad-<timestamp>[-N]` without overwriting an earlier backup.
fn back_up(path: &Path) -> io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let named = |suffix: String| {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        PathBuf::from(p)
    };
    let mut backup = named(format!(".bad-{stamp}"));
    let mut n = 1;
    while backup.exists() {
        backup = named(format!(".bad-{stamp}-{n}"));
        n += 1;
    }
    fs::copy(path, &backup)?;
    Ok(backup)
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
        assert_eq!(
            Config::load_from(&dir.path().join("none.toml")),
            Config::default()
        );
        let bad = dir.path().join("bad.toml");
        fs::write(&bad, "this is = = not toml").unwrap();
        assert_eq!(Config::load_from(&bad), Config::default());
    }

    fn backups(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".bad-"))
            .collect()
    }

    #[test]
    fn one_bad_value_keeps_the_other_settings_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "auto_advance = true\nthumb_display_size = \"big\"\ndisk_cache_mb = 512\n";
        fs::write(&path, text).unwrap();
        let cfg = Config::load_from(&path);
        assert!(cfg.auto_advance);
        assert_eq!(cfg.disk_cache_mb, 512);
        assert_eq!(cfg.thumb_display_size, Config::default().thumb_display_size);
        let backups = backups(dir.path());
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(&backups[0]).unwrap(), text);
    }

    #[test]
    fn non_toml_file_gives_defaults_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "this is = = not toml").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
        assert_eq!(backups(dir.path()).len(), 1);
    }

    #[test]
    fn missing_or_valid_file_is_not_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::load_from(&path);
        fs::write(&path, "auto_advance = true\n").unwrap();
        Config::load_from(&path);
        assert!(backups(dir.path()).is_empty());
    }

    #[test]
    fn non_utf8_last_folder_does_not_block_saving() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config {
            auto_advance: true,
            last_folder: Some(PathBuf::from(std::ffi::OsStr::from_bytes(
                b"/photos/\xff\xfe",
            ))),
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path);
        assert!(loaded.auto_advance);
        assert_eq!(loaded.last_folder, None);
    }

    #[test]
    fn save_is_complete_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save_to(&path).unwrap();
        let cfg = Config {
            disk_cache_mb: 99,
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), cfg);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["config.toml"]);
    }

    #[test]
    fn failed_save_leaves_the_old_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "auto_advance = true\n").unwrap();
        // The temp file name is taken by a directory, so the write cannot happen.
        fs::create_dir(dir.path().join("config.toml.tmp")).unwrap();
        assert!(Config::default().save_to(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "auto_advance = true\n");
    }

    #[test]
    fn show_folders_roundtrips_and_defaults_to_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "auto_advance = true\n").unwrap();
        assert!(!Config::load_from(&path).show_folders, "older configs: off");
        let cfg = Config {
            show_folders: true,
            ..Default::default()
        };
        cfg.save_to(&path).unwrap();
        assert!(Config::load_from(&path).show_folders);
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
