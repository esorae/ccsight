use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const PINS_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct PinsData {
    version: u32,
    pins: Vec<PinEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PinEntry {
    pub path: PathBuf,
    pub pinned_at: DateTime<Utc>,
}

pub struct Pins {
    data_path: PathBuf,
    entries: Vec<PinEntry>,
    lookup: HashSet<PathBuf>,
}

impl Pins {
    pub fn empty() -> Self {
        Self {
            data_path: Self::default_path().unwrap_or_else(|_| PathBuf::from("/dev/null")),
            entries: Vec::new(),
            lookup: HashSet::new(),
        }
    }

    pub fn load() -> Result<Self> {
        let data_path = Self::default_path()?;
        let (entries, lookup) = if data_path.exists() {
            let file = File::open(&data_path)?;
            let reader = BufReader::new(file);
            match serde_json::from_reader::<_, PinsData>(reader) {
                Ok(data) if data.version == PINS_VERSION => {
                    let lookup: HashSet<PathBuf> =
                        data.pins.iter().map(|e| e.path.clone()).collect();
                    (data.pins, lookup)
                }
                _ => (Vec::new(), HashSet::new()),
            }
        } else {
            (Vec::new(), HashSet::new())
        };

        Ok(Self {
            data_path,
            entries,
            lookup,
        })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.data_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = self.data_path.with_extension("json.tmp");
        let file = File::create(&temp_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&temp_path, permissions)?;
        }

        let data = PinsData {
            version: PINS_VERSION,
            pins: self.entries.clone(),
        };

        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &data)?;
        writer.flush()?;
        writer.into_inner()?.sync_all()?;

        if let Err(e) = fs::rename(&temp_path, &self.data_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(e.into());
        }
        Ok(())
    }

    pub fn toggle(&mut self, path: &Path) -> bool {
        if self.lookup.contains(path) {
            self.entries.retain(|e| e.path != path);
            self.lookup.remove(path);
            false
        } else {
            let entry = PinEntry {
                path: path.to_path_buf(),
                pinned_at: Utc::now(),
            };
            self.entries.insert(0, entry);
            self.lookup.insert(path.to_path_buf());
            true
        }
    }

    pub fn is_pinned(&self, path: &Path) -> bool {
        self.lookup.contains(path)
    }

    pub fn entries(&self) -> &[PinEntry] {
        &self.entries
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|e| e.path != path);
        self.lookup.remove(path);
    }

    fn default_path() -> Result<PathBuf> {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".config/ccsight/pins.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_toggle_pin() {
        let mut pins = Pins::empty();
        let path = PathBuf::from("/tmp/test.jsonl");

        assert!(!pins.is_pinned(&path));
        assert!(pins.toggle(&path));
        assert!(pins.is_pinned(&path));
        assert!(!pins.toggle(&path));
        assert!(!pins.is_pinned(&path));
    }

    #[test]
    fn test_remove_pin() {
        let mut pins = Pins::empty();
        let path = PathBuf::from("/tmp/test.jsonl");

        pins.toggle(&path);
        assert!(pins.is_pinned(&path));
        pins.remove(&path);
        assert!(!pins.is_pinned(&path));
    }

    #[test]
    fn test_toggle_inserts_at_front() {
        let mut pins = Pins::empty();
        let p1 = PathBuf::from("/tmp/a.jsonl");
        let p2 = PathBuf::from("/tmp/b.jsonl");

        pins.toggle(&p1);
        pins.toggle(&p2);

        assert_eq!(pins.entries[0].path, p2);
        assert_eq!(pins.entries[1].path, p1);
    }
}
