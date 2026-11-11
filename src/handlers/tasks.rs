//! Background-task spawn helpers extracted from inline `thread::spawn` blocks
//! in `main.rs::run`. Each helper:
//! 1. Sets the relevant `state.*_task = Some(rx)` flag so the main loop polls it
//! 2. Captures the cloned session/day data into a `move ||` closure
//! 3. Sends the result back through an `mpsc::channel`
//!
//! Callers must verify the precondition (e.g. `state.summary_task.is_none()`)
//! before invoking — these helpers do not check, they just start.

use std::sync::mpsc;
use std::thread;

use crate::aggregator::{DailyGroup, SessionInfo};
use crate::state::SummaryType;
use crate::{AppState, summary};

/// A clipboard write in flight. The text rides along so a failed write can be
/// resent through the terminal ([`osc52`]) from the main thread.
pub struct ClipboardTask {
    pub(crate) rx: mpsc::Receiver<Result<(), String>>,
    pub(crate) text: String,
}

/// Detached clipboard write — `arboard::Clipboard::set_text` is
/// synchronous and can stall multi-second on macOS NSPasteboard
/// contention, freezing the event loop.
pub(crate) fn spawn_clipboard_write(text: String) -> ClipboardTask {
    let (tx, rx) = mpsc::channel();
    // Tests press `y` too, and must not overwrite the user's clipboard.
    if cfg!(test) {
        let _ = tx.send(Ok(()));
        return ClipboardTask { rx, text };
    }
    let copy = text.clone();
    thread::spawn(move || {
        let result = match arboard::Clipboard::new() {
            Ok(mut clipboard) => clipboard.set_text(&copy).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(result);
    });
    ClipboardTask { rx, text }
}

/// OSC 52 asks the terminal to set its clipboard. It reaches one the OS API
/// cannot: over SSH, or under a multiplexer server left in an ended login
/// session. One-way: the terminal never confirms, and may ignore it.
pub(crate) fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Spawn an AI session-summary task (generate or regenerate) and stash the
/// receiver in `state.summary_task`. Also sets `generating_summary`,
/// `active_popup = Summary`, and `summary_type` so the popup shows immediately.
pub(crate) fn start_session_summary(state: &mut AppState, session: SessionInfo, regenerate: bool) {
    state.generating_summary = true;
    state.active_popup = crate::ActivePopup::Summary { scroll: 0 };
    state.summary_type = Some(SummaryType::Session(Box::new(session.clone())));
    let summary_date = state.daily_groups.get(state.selected_day).map(|g| g.date);
    let (tx, rx) = mpsc::channel();
    state.summary_task = Some(rx);
    thread::spawn(move || {
        let result = if regenerate {
            summary::regenerate_session_summary(&session, summary_date)
        } else {
            summary::generate_session_summary(&session, summary_date)
        };
        let _ = tx.send(result);
    });
}

/// Spawn an AI day-summary task (generate or regenerate). Same UI-flag side
/// effects as [`start_session_summary`] but for `SummaryType::Day`.
pub(crate) fn start_day_summary(state: &mut AppState, group: DailyGroup, regenerate: bool) {
    state.generating_summary = true;
    state.active_popup = crate::ActivePopup::Summary { scroll: 0 };
    state.summary_type = Some(SummaryType::Day(group.clone()));
    let (tx, rx) = mpsc::channel();
    state.summary_task = Some(rx);
    thread::spawn(move || {
        let result = if regenerate {
            summary::regenerate_day_summary(&group)
        } else {
            summary::generate_day_summary(&group)
        };
        let _ = tx.send(result);
    });
}

/// Spawn the JSONL custom-title generation task triggered by `t` in Daily /
/// Session detail popups. Records `(day, session)` in `state.updating_session`
/// for the row's spinner; the result is matched back by file path.
pub(crate) fn start_jsonl_regen(
    state: &mut AppState,
    session: SessionInfo,
    day: usize,
    sess: usize,
) {
    let file_path = session.file_path.clone();
    let (tx, rx) = mpsc::channel();
    state.updating_session = Some((day, sess));
    state.updating_task = Some((rx, file_path));
    thread::spawn(move || {
        let result = summary::regenerate_jsonl_title(&session);
        let _ = tx.send(result);
    });
}

#[cfg(test)]
mod tests {
    use super::{base64, osc52};

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "{input:?}");
        }
        // Multi-byte text goes through as its UTF-8 bytes.
        assert_eq!(base64("あ".as_bytes()), "44GC");
    }

    #[test]
    fn osc52_wraps_the_payload_for_the_clipboard_selection() {
        assert_eq!(osc52("foo"), "\x1b]52;c;Zm9v\x07");
    }
}
