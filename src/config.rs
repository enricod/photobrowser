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
        assert_eq!(
            Config::load_from(&dir.path().join("none.toml")),
            Config::default()
        );
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
