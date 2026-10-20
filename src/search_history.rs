//! Persisted query history for the global search popup. Stores the most
//! recent N queries in chronological order (newest last) at
//! `~/.ccsight/search_history.json`. The Esc-keeps-text behaviour covers
//! the "restore last query" case; this module covers everything older.

use std::collections::VecDeque;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Cap so a long-lived user doesn't accumulate megabytes of dead queries.
/// 100 entries is enough to scroll back through a session's worth of
/// experimentation without making the JSON file unwieldy.
const MAX_ENTRIES: usize = 100;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct OnDisk {
    entries: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct SearchHistory {
    entries: VecDeque<String>,
    /// Cursor into `entries`. None = the user is composing a fresh query
    /// (not browsing). Some(i) = entries[i] is the surfaced past query.
    /// Recall direction: Up = older (i++), Down = newer (i--, eventually None).
    cursor: Option<usize>,
    /// Draft snapshot taken when the user first presses Up. Restoring it
    /// on Down past index 0 means the user can browse and back-out without
    /// losing what they were typing.
    draft: Option<String>,
    /// Where `push` saves. `Some` only when `load` read the file or found
    /// none: a history it could not read is never written over, and the
    /// default (every test state) never writes at all.
    path: Option<std::path::PathBuf>,
}

impl SearchHistory {
    pub fn load() -> Self {
        crate::infrastructure::search_history_path()
            .map_or_else(|_| Self::default(), Self::load_from)
    }

    fn load_from(path: std::path::PathBuf) -> Self {
        let on_disk = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<OnDisk>(&bytes) {
                Ok(on_disk) => on_disk,
                // Moved aside rather than written over, as pins.json is: the
                // history starts fresh and the old file stays recoverable.
                Err(_) => {
                    let aside = path.with_extension(format!(
                        "json.corrupt-{}",
                        chrono::Utc::now().format("%Y%m%dT%H%M%S")
                    ));
                    if std::fs::rename(&path, &aside).is_err() {
                        return Self::default();
                    }
                    OnDisk::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => OnDisk::default(),
            Err(_) => return Self::default(),
        };
        let mut entries = VecDeque::from(on_disk.entries);
        while entries.len() > MAX_ENTRIES {
            entries.pop_front();
        }
        Self {
            entries,
            cursor: None,
            draft: None,
            path: Some(path),
        }
    }

    /// Append a query to the history (and persist). Dedupes against the
    /// last entry so spamming Enter on the same query doesn't bloat the
    /// file. Empty queries are ignored.
    pub fn push(&mut self, query: &str) {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.entries.back().is_some_and(|last| last == trimmed) {
            self.reset_cursor();
            return;
        }
        // Move existing duplicate to the end rather than keeping two copies.
        if let Some(pos) = self.entries.iter().position(|e| e == trimmed) {
            self.entries.remove(pos);
        }
        self.entries.push_back(trimmed.to_string());
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.reset_cursor();
        let _ = self.save();
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let on_disk = OnDisk {
            entries: self.entries.iter().cloned().collect(),
        };
        let bytes = serde_json::to_vec_pretty(&on_disk)?;
        crate::infrastructure::atomic_write(path, &bytes)?;
        Ok(())
    }

    /// Step to the previous (older) history entry. Stashes `current_draft`
    /// the first time so Down can restore it.
    pub fn step_back(&mut self, current_draft: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => {
                self.draft = Some(current_draft.to_string());
                self.entries.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        self.entries.get(next).cloned()
    }

    /// Step to the next (newer) entry. When we step past the last entry
    /// (newest), return the stashed draft so the user can resume editing.
    pub fn step_forward(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let cur = self.cursor?;
        if cur + 1 < self.entries.len() {
            self.cursor = Some(cur + 1);
            self.entries.get(cur + 1).cloned()
        } else {
            self.cursor = None;
            self.draft.take()
        }
    }

    /// Drop the active recall cursor — called whenever the user types or
    /// closes the popup, since further edits should start a fresh history
    /// trip on the next Up.
    pub fn reset_cursor(&mut self) {
        self.cursor = None;
        self.draft = None;
    }

    pub fn is_browsing(&self) -> bool {
        self.cursor.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A history file that cannot be parsed is moved aside and the history
    // starts fresh; a missing one is created on the first query; a default
    // history never writes.
    #[test]
    fn only_a_history_read_from_disk_or_absent_is_saved() {
        let dir = std::env::temp_dir().join(format!("ccsight-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, b"{not json").unwrap();
        let mut h = SearchHistory::load_from(corrupt.clone());
        h.push("query");
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1, "the bad file is kept aside");
        assert_eq!(std::fs::read(&aside[0]).unwrap(), b"{not json");
        assert!(
            String::from_utf8(std::fs::read(&corrupt).unwrap())
                .unwrap()
                .contains("query")
        );

        let fresh = dir.join("fresh.json");
        let _ = std::fs::remove_file(&fresh);
        let mut h = SearchHistory::load_from(fresh.clone());
        h.push("query");
        assert!(
            String::from_utf8(std::fs::read(&fresh).unwrap())
                .unwrap()
                .contains("query")
        );

        let mut h = SearchHistory::default();
        h.push("query");
        assert!(h.path.is_none(), "a default history has nowhere to write");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn step_back_returns_newest_then_older() {
        let mut h = SearchHistory::default();
        h.entries.push_back("a".into());
        h.entries.push_back("b".into());
        h.entries.push_back("c".into());
        assert_eq!(h.step_back("draft").as_deref(), Some("c"));
        assert_eq!(h.step_back("draft").as_deref(), Some("b"));
        assert_eq!(h.step_back("draft").as_deref(), Some("a"));
        // Bottoms out at oldest, no underflow.
        assert_eq!(h.step_back("draft").as_deref(), Some("a"));
    }

    #[test]
    fn step_forward_restores_draft_past_newest() {
        let mut h = SearchHistory::default();
        h.entries.push_back("a".into());
        h.entries.push_back("b".into());
        assert_eq!(h.step_back("my draft").as_deref(), Some("b"));
        // Past the newest, we get the original draft back.
        assert_eq!(h.step_forward().as_deref(), Some("my draft"));
        // After resuming the draft we're no longer browsing.
        assert!(!h.is_browsing());
    }

    #[test]
    fn push_dedupes_against_recent() {
        let mut h = SearchHistory::default();
        h.entries.push_back("same".into());
        // Without ever saving (load path unused in tests), just exercise the
        // dedup logic by calling push directly.
        let _ = h.entries.clone();
        // Push a duplicate of the last entry — should be a no-op.
        h.push("same");
        assert_eq!(h.entries.len(), 1);
        h.push("other");
        assert_eq!(h.entries.len(), 2);
    }
}
