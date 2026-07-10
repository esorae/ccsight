use std::collections::HashSet;
use std::fs;
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

    /// Parse pins file bytes. `Some(entries)` on a valid, current-version
    /// file; `None` on parse failure or version mismatch. The byte seam that
    /// lets `load` tell "no pins" from "unreadable" so it never silently zeros
    /// real data (the pins-safety invariant). Must never panic on arbitrary
    /// bytes; the `SEAM_INPUTS` table and property tests below enforce that.
    pub fn from_bytes(bytes: &[u8]) -> Option<Vec<PinEntry>> {
        match serde_json::from_slice::<PinsData>(bytes) {
            Ok(data) if data.version == PINS_VERSION => Some(data.pins),
            _ => None,
        }
    }

    pub fn load() -> Result<Self> {
        let data_path = Self::default_path()?;
        let (entries, lookup) = if data_path.exists() {
            let bytes = fs::read(&data_path)?;
            match Self::from_bytes(&bytes) {
                Some(pins) => {
                    let lookup: HashSet<PathBuf> = pins.iter().map(|e| e.path.clone()).collect();
                    (pins, lookup)
                }
                None => {
                    // Parse failure / version mismatch: move file aside instead
                    // of zeroing, otherwise the next toggle+save would
                    // overwrite real pins with a single-entry file. Rename
                    // failures are silent (best-effort recovery).
                    let backup = data_path.with_extension(format!(
                        "json.corrupt-{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%S")
                    ));
                    let _ = fs::rename(&data_path, &backup);
                    (Vec::new(), HashSet::new())
                }
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
        // Reject the `/dev/null` placeholder used by `Pins::empty()` when HOME is unset.
        // Saving to /dev/null on Linux silently succeeds but never persists, masking the
        // failure. Surface it as an error so callers can inform the user.
        if self.data_path.as_os_str() == "/dev/null" {
            return Err(anyhow::anyhow!(
                "Cannot save pins: HOME is not set (data_path is /dev/null fallback)"
            ));
        }
        let data = PinsData {
            version: PINS_VERSION,
            pins: self.entries.clone(),
        };
        let bytes = serde_json::to_vec(&data)?;
        crate::infrastructure::atomic_write(&self.data_path, &bytes)?;
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

    /// Swap the entry at `idx` with the one above it. No-op when `idx`
    /// is at the top or out of range. Used by the pins popup so users
    /// can rearrange their order manually instead of being stuck with
    /// pin-time ordering.
    pub fn move_up(&mut self, idx: usize) -> bool {
        if idx == 0 || idx >= self.entries.len() {
            return false;
        }
        self.entries.swap(idx, idx - 1);
        true
    }

    /// Swap the entry at `idx` with the one below it.
    pub fn move_down(&mut self, idx: usize) -> bool {
        if idx + 1 >= self.entries.len() {
            return false;
        }
        self.entries.swap(idx, idx + 1);
        true
    }

    fn default_path() -> Result<PathBuf> {
        crate::infrastructure::pins_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Byte blobs the config-loader seam must survive, each with its expected
    /// outcome: `None` is the "unreadable" signal `load` needs to move the file
    /// aside, `Some(n)` accepts it with `n` pins. Confusing the two silently
    /// discards a hand-edited `pins.json`. Inline rather than files on disk so
    /// a review diff shows the bytes.
    const SEAM_INPUTS: &[(&str, &[u8], Option<usize>)] = &[
        ("empty", b"", None),
        ("empty_object", b"{}", None),
        ("garbage", b"\xff\x00garbage\x1b[m", None),
        (
            "valid",
            b"{\"version\":1,\"pins\":[{\"path\":\"/tmp/x.jsonl\",\"pinned_at\":\"2026-01-01T00:00:00Z\"}]}",
            Some(1),
        ),
        ("wrong_version", b"{\"version\":99,\"pins\":[]}", None),
        // Shapes a text editor actually produces. Each must reject the whole
        // file rather than yield a short pin list.
        (
            "version_as_string",
            b"{\"version\":\"1\",\"pins\":[]}",
            None,
        ),
        (
            "utf8_bom",
            b"\xef\xbb\xbf{\"version\":1,\"pins\":[]}",
            None,
        ),
        ("trailing_comma", b"{\"version\":1,\"pins\":[],}", None),
        ("pins_as_object", b"{\"version\":1,\"pins\":{}}", None),
        (
            "pinned_at_not_rfc3339",
            b"{\"version\":1,\"pins\":[{\"path\":\"/tmp/x.jsonl\",\"pinned_at\":\"yesterday\"}]}",
            None,
        ),
        // An empty list is the user unpinning everything, not corruption — the
        // one case where `Some(0)` is the right answer.
        (
            "legitimately_empty",
            b"{\"version\":1,\"pins\":[]}",
            Some(0),
        ),
        // A newer ccsight adding a field must not lock this build out.
        (
            "unknown_field_forward_compat",
            b"{\"version\":1,\"pins\":[],\"future\":true}",
            Some(0),
        ),
    ];

    #[test]
    fn seam_inputs_load_to_their_expected_outcome_and_accepted_pins_serialize() {
        for (name, bytes, expected) in SEAM_INPUTS {
            let parsed = Pins::from_bytes(bytes);
            assert_eq!(
                parsed.as_ref().map(Vec::len),
                *expected,
                "{name}: wrong outcome — a rejected file must stay `None`, never an empty set"
            );
            if let Some(entries) = parsed {
                serde_json::to_string(&entries)
                    .unwrap_or_else(|e| panic!("{name}: accepted pins do not serialize: {e}"));
            }
        }
    }

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

    #[test]
    fn test_empty_pins_save_returns_error() {
        // Regression: Pins::empty() falls back to /dev/null when HOME is unset. save()
        // must return an error rather than silently writing to /dev/null.
        let pins = Pins::empty();
        if pins.data_path.as_os_str() == "/dev/null" {
            let err = pins
                .save()
                .expect_err("save() should fail on /dev/null fallback");
            assert!(
                err.to_string().contains("HOME is not set"),
                "error message should mention HOME, got: {err}"
            );
        }
        // If HOME is set, default path is real and save would succeed; skip the assertion.
    }

    #[test]
    fn test_save_load_roundtrip() {
        // Regression: writing pins to disk and reading them back must reproduce the
        // exact entry list (order + paths). Guards against accidental schema drift.
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ccsight-pins-roundtrip-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut pins = Pins {
            data_path: path.clone(),
            entries: Vec::new(),
            lookup: HashSet::new(),
        };
        let p1 = PathBuf::from("/tmp/a.jsonl");
        let p2 = PathBuf::from("/tmp/b.jsonl");
        pins.toggle(&p1);
        pins.toggle(&p2);
        pins.save().expect("save pins");

        // Reload from the same on-disk file.
        let file = std::fs::File::open(&path).expect("open saved pins");
        let reader = std::io::BufReader::new(file);
        let data: PinsData = serde_json::from_reader(reader).expect("parse pins");

        assert_eq!(data.version, PINS_VERSION);
        assert_eq!(data.pins.len(), 2);
        // Most recently toggled is at the front.
        assert_eq!(data.pins[0].path, p2);
        assert_eq!(data.pins[1].path, p1);

        let _ = std::fs::remove_file(&path);
    }

    // ── Property tests: the `from_bytes(&[u8])` config-loader seam ─────────────
    // `wrong_version_is_none` is the only assertion of the anti-zero
    // (pins-safety) invariant anywhere, so it belongs on the per-commit gate.
    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            // Adversarial bytes never panic.
            #[test]
            fn from_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
                let _ = Pins::from_bytes(&bytes);
            }

            // A valid, current-version blob round-trips to the same entry count.
            #[test]
            fn valid_pins_roundtrip(paths in proptest::collection::vec("[a-z0-9/._-]{0,24}", 0..6)) {
                let data = PinsData {
                    version: PINS_VERSION,
                    pins: paths
                        .iter()
                        .map(|p| PinEntry {
                            path: PathBuf::from(p),
                            pinned_at: Utc::now(),
                        })
                        .collect(),
                };
                let bytes = serde_json::to_vec(&data).unwrap();
                let parsed = Pins::from_bytes(&bytes);
                prop_assert!(parsed.is_some());
                prop_assert_eq!(parsed.unwrap().len(), paths.len());
            }

            // Anti-zero invariant: a wrong-version blob yields None (NOT
            // Some(empty)), so load backs the file up instead of zeroing pins.
            #[test]
            fn wrong_version_is_none(v in any::<u32>().prop_filter("current", |v| *v != PINS_VERSION)) {
                let blob = serde_json::json!({"version": v, "pins": []}).to_string();
                prop_assert!(Pins::from_bytes(blob.as_bytes()).is_none());
            }
        }
    }
}
