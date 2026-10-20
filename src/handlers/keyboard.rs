//! Popup-state-specific keyboard handlers extracted from `main.rs::run`.
//!
//! Each handler owns one `else if state.show_X { match key.code { ... } }`
//! branch from the main event loop. They are dispatched only when their
//! corresponding popup flag is true (caller does the routing).
//!
//! Conventions:
//! - All handlers take `&mut AppState` and a `KeyEvent`.
//! - No return value — keys are always considered consumed.
//! - Each handler must not assume cross-popup interaction; if a popup-A key
//!   triggers popup-B, that is set up via state flags and the next event tick
//!   picks up the new state.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::handlers;
use crate::infrastructure::live_sessions::LiveSession;
use crate::infrastructure::live_snapshots::LiveSnapshot;
use crate::state::{ConvListMode, ConversationPane, DailyPanel, MAX_PANES, SummaryType};
use crate::{AppState, PeriodFilter, Tab, search, ui};

/// Live tab snapshot stepper. `delta = +1` walks back one snapshot
/// (older), `-1` walks forward (newer / toward today). The snapshot list
/// is sorted captured_at-descending across the full retention window, so
/// stepping crosses date boundaries automatically when multiple snapshots
/// exist within the same day.
pub(crate) fn step_live_view_snapshot(state: &mut AppState, delta: i32) {
    let snapshots = LiveSnapshot::load_recent();
    let total = snapshots.len();
    let next = match delta.signum() {
        1 => state
            .live_view_snapshot_offset
            .saturating_add(delta.unsigned_abs() as usize),
        -1 => state
            .live_view_snapshot_offset
            .saturating_sub(delta.unsigned_abs() as usize),
        _ => return,
    };
    let next = next.min(total);
    if next == state.live_view_snapshot_offset {
        state.toast(if delta > 0 {
            "Already at oldest snapshot"
        } else {
            "Already at today"
        });
        return;
    }
    state.live_view_snapshot_offset = next;
    state.live_past_snapshot_total = total;
    if next == 0 {
        state.live_past_sessions.clear();
        state.live_past_snapshot_meta = None;
    } else {
        // offset 1 → snapshots[0] (most-recent past), offset N → snapshots[N-1].
        if let Some(snap) = snapshots.into_iter().nth(next - 1) {
            state.live_past_snapshot_meta = Some((snap.captured_at, snap.date));
            let mut rows: Vec<LiveSession> = snap
                .sessions
                .into_values()
                .map(|e| {
                    let jsonl_mtime = e
                        .jsonl_path
                        .as_deref()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                        .and_then(|d| {
                            chrono::DateTime::<chrono::Utc>::from_timestamp(
                                d.as_secs() as i64,
                                d.subsec_nanos(),
                            )
                        });
                    LiveSession {
                        session_id: e.session_id,
                        jsonl_path: e.jsonl_path,
                        cwd: e.cwd,
                        name: e.name,
                        status: None,
                        pid: 0,
                        started_at: None,
                        updated_at: jsonl_mtime,
                        jsonl_mtime,
                        is_live: false,
                        was_recently_live: false,
                    }
                })
                .collect();
            rows.sort_by_key(|s| {
                std::cmp::Reverse(
                    s.jsonl_mtime
                        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
                )
            });
            state.live_past_sessions = rows;
        }
    }
    state.live_selected = 0;
    state.live_scroll = 0;
    state.needs_draw = true;
}

/// Scroll intent shared by every `j/k`-scrollable popup. Centralizing the
/// key→intent mapping in `scroll_action_for_key` means a new popup can't
/// silently end up with a narrower key set than its siblings.
pub(crate) enum ScrollAction {
    Up(usize),
    Down(usize),
    Home,
    End,
}

const SCROLL_PAGE: usize = 10;

pub(crate) fn scroll_action_for_key(key: &KeyEvent) -> Option<ScrollAction> {
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(ScrollAction::Down(1)),
        KeyCode::Up | KeyCode::Char('k') => Some(ScrollAction::Up(1)),
        KeyCode::PageDown | KeyCode::Char('d') => Some(ScrollAction::Down(SCROLL_PAGE)),
        KeyCode::PageUp | KeyCode::Char('u') => Some(ScrollAction::Up(SCROLL_PAGE)),
        KeyCode::Home | KeyCode::Char('g') => Some(ScrollAction::Home),
        KeyCode::End | KeyCode::Char('G') => Some(ScrollAction::End),
        _ => None,
    }
}

/// Apply to a `usize` scroll field. `End` saturates to `usize::MAX` — the
/// draw fn clamps against actual content height every frame, so landing
/// past the end is safe and renders the last page.
pub(crate) fn apply_scroll_usize(current: usize, action: ScrollAction) -> usize {
    match action {
        ScrollAction::Up(n) => current.saturating_sub(n),
        ScrollAction::Down(n) => current.saturating_add(n),
        ScrollAction::Home => 0,
        ScrollAction::End => usize::MAX,
    }
}

/// `u16` counterpart for scroll fields whose backing type is `u16`.
pub(crate) fn apply_scroll_u16(current: u16, action: ScrollAction) -> u16 {
    match action {
        ScrollAction::Up(n) => current.saturating_sub(n as u16),
        ScrollAction::Down(n) => current.saturating_add(n as u16),
        ScrollAction::Home => 0,
        ScrollAction::End => u16::MAX,
    }
}

/// `ActivePopup::Help` branch — Esc/q/? close, j/k/↑↓ scroll the body.
pub(crate) fn handle_help_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
            state.active_popup = crate::ActivePopup::None;
        }
        _ => {
            if let Some(action) = scroll_action_for_key(&key) {
                state.set_help_scroll(apply_scroll_u16(state.help_scroll(), action));
            }
        }
    }
}

/// `ActivePopup::ProjectDetail` branch — Esc/q/Enter close (Enter
/// mirrors the insights detail pattern so the same key toggles open/close),
/// j/k/↑↓ scroll.
pub(crate) fn handle_project_detail_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            // Project detail is only ever drilled into from the Projects-list
            // DashboardDetail popup, so closing returns there (the two-level
            // drill-back), not straight to the bare dashboard.
            state.active_popup = crate::ActivePopup::DashboardDetail;
        }
        KeyCode::Enter => open_project_session_detail(state),
        _ => {
            if let Some(action) = scroll_action_for_key(&key) {
                move_project_detail_cursor(state, action);
            }
        }
    }
}

/// The Session Detail of the project row under the cursor; closing it lands
/// back on that row.
pub(crate) fn open_project_session_detail(state: &mut AppState) {
    let path = state.project_detail_path().to_string();
    let (scroll, cursor) = (state.project_detail_scroll(), state.project_detail_cursor());
    let Some(session) = crate::ui::project_session_rows(state, &path)
        .get(cursor)
        .map(|(_, s)| (*s).clone())
    else {
        return;
    };
    state.session_detail_override = Some(session);
    state.session_detail_live_extra = None;
    state.session_detail_scroll = 0;
    state.active_popup = crate::ActivePopup::Detail {
        back: Some(crate::state::DetailBack::ProjectDetail {
            path,
            scroll,
            cursor,
        }),
    };
}

/// Move the project detail's cursor, kept on a session row.
pub(crate) fn move_project_detail_cursor(state: &mut AppState, action: ScrollAction) {
    let rows = crate::ui::project_session_rows(state, state.project_detail_path()).len();
    let next = apply_scroll_usize(state.project_detail_cursor(), action);
    state.set_project_detail_cursor(next.min(rows.saturating_sub(1)));
}

/// Daily ←/→: one day older / newer, from the first session. An open day
/// detail (Breakdown, Activity) stays open on the new day, from its top.
fn step_daily_day(state: &mut AppState, older: bool) {
    let last = state.daily_groups.len().saturating_sub(1);
    if older && state.selected_day < last {
        state.selected_day += 1;
    } else if !older && state.selected_day > 0 {
        state.selected_day -= 1;
    } else {
        return;
    }
    state.selected_session = 0;
    state.daily_breakdown_scroll = 0;
    state.set_day_activity_scroll(0);
    state.set_day_activity_cursor(0);
}

/// `ActivePopup::DayActivity` branch — ↑↓ pick a session, Enter opens its
/// detail, ←→ step the day, Esc/q close.
pub(crate) fn handle_day_activity_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.active_popup = crate::ActivePopup::None;
        }
        KeyCode::Left | KeyCode::Char('h') => step_daily_day(state, true),
        KeyCode::Right | KeyCode::Char('l') => step_daily_day(state, false),
        KeyCode::Enter => open_day_activity_session(state),
        _ => {
            if let Some(action) = scroll_action_for_key(&key) {
                move_day_activity_cursor(state, action);
            }
        }
    }
}

/// The Session Detail of the Day activity row under the cursor; closing it
/// returns to that row.
pub(crate) fn open_day_activity_session(state: &mut AppState) {
    let Some(path) = day_activity_selected_path(state) else {
        return;
    };
    let Some(idx) = state
        .daily_groups
        .get(state.selected_day)
        .and_then(|g| g.user_sessions().position(|s| s.file_path == path))
    else {
        return;
    };
    // The picked session becomes the list's cursor, so focus follows it.
    state.selected_session = idx;
    state.daily_panel = DailyPanel::Sessions;
    let back = crate::state::DetailBack::DayActivity {
        scroll: state.day_activity_scroll(),
        cursor: state.day_activity_cursor(),
    };
    state.active_popup = crate::ActivePopup::Detail { back: Some(back) };
    state.session_detail_scroll = 0;
}

/// The session the Activity popup's cursor sits on.
fn day_activity_selected_path(state: &AppState) -> Option<std::path::PathBuf> {
    let group = state.daily_groups.get(state.selected_day)?;
    let rows = crate::ui::day_activity_rows(state, group);
    rows.into_iter()
        .nth(state.day_activity_cursor())
        .map(|(path, _, _)| path)
}

/// Move the Activity popup's cursor, kept on a session row.
pub(crate) fn move_day_activity_cursor(state: &mut AppState, action: ScrollAction) {
    let rows = state
        .daily_groups
        .get(state.selected_day)
        .map_or(0, |g| g.user_sessions().count());
    let next = apply_scroll_usize(state.day_activity_cursor(), action);
    state.set_day_activity_cursor(next.min(rows.saturating_sub(1)));
}

/// Insights ←/→: the previous / next panel, wrapping at either end.
fn step_insights_panel(state: &mut AppState, back: bool) {
    let n = crate::ui::INSIGHTS_PANELS;
    state.insights_panel = if back {
        (state.insights_panel + n - 1) % n
    } else {
        (state.insights_panel + 1) % n
    };
}

/// `ActivePopup::InsightsDetail` branch — Esc/q/Enter/i close,
/// ←/→ h/l cycle through the panels (wrap), ↑/↓ j/k scroll the body.
pub(crate) fn handle_insights_detail_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('i') => {
            state.active_popup = crate::ActivePopup::None;
        }
        KeyCode::Left | KeyCode::Char('h') => {
            step_insights_panel(state, true);
            state.set_insights_detail_scroll(0);
        }
        KeyCode::Right | KeyCode::Char('l') => {
            step_insights_panel(state, false);
            state.set_insights_detail_scroll(0);
        }
        _ => {
            if let Some(action) = scroll_action_for_key(&key) {
                state.set_insights_detail_scroll(apply_scroll_usize(
                    state.insights_detail_scroll(),
                    action,
                ));
            }
        }
    }
}

/// `ActivePopup::FilterPopup` branch — period filter popup with two
/// sub-modes (preset list nav vs Custom date input).
pub(crate) fn handle_filter_popup_key(state: &mut AppState, key: KeyEvent) {
    let total_items = PeriodFilter::ALL_VARIANTS.len() + 1;
    if state.filter_input_mode() {
        match key.code {
            // Esc backs out of the input field to the list of preset
            // ranges (still inside the filter popup); a second Esc
            // closes the popup. Without this two-step exit users
            // who fat-fingered the Custom row had no way to escape
            // back to the preset list without losing the popup.
            KeyCode::Esc => {
                state.set_filter_input_mode(false);
                if let Some(input) = state.filter_input_mut() {
                    input.clear();
                }
                state.set_filter_input_error(false);
            }
            KeyCode::Enter => {
                let text = state
                    .filter_input()
                    .map(|i| i.text.clone())
                    .unwrap_or_default();
                if let Some(filter) = PeriodFilter::parse_custom(&text) {
                    state.period_filter = filter;
                    state.apply_filter();
                    state.active_popup = crate::ActivePopup::None;
                } else {
                    state.set_filter_input_error(true);
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = state.filter_input_mut() {
                    input.delete_back();
                }
                state.set_filter_input_error(false);
            }
            KeyCode::Left => {
                if let Some(input) = state.filter_input_mut() {
                    input.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(input) = state.filter_input_mut() {
                    input.move_right();
                }
            }
            KeyCode::Home => {
                if let Some(input) = state.filter_input_mut() {
                    input.move_home();
                }
            }
            KeyCode::End => {
                if let Some(input) = state.filter_input_mut() {
                    input.move_end();
                }
            }
            // Date-shape chars only (see `InputKind::Filter::sanitize_input_char`,
            // shared with the paste path); otherwise `f`/`j`/`k` (popup / nav
            // keys) would silently append garbage.
            KeyCode::Char(c) => {
                if let Some(c) = crate::InputKind::Filter.sanitize_input_char(c) {
                    if let Some(input) = state.filter_input_mut() {
                        input.insert_char(c);
                    }
                    state.set_filter_input_error(false);
                }
            }
            _ => {}
        }
    } else {
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => {
                state.active_popup = crate::ActivePopup::None;
            }
            KeyCode::Up | KeyCode::Char('k') if state.filter_popup_selected() > 0 => {
                state.set_filter_popup_selected(state.filter_popup_selected() - 1);
            }
            KeyCode::Down | KeyCode::Char('j')
                if state.filter_popup_selected() < total_items - 1 =>
            {
                state.set_filter_popup_selected(state.filter_popup_selected() + 1);
            }
            KeyCode::Enter => choose_filter_row(state),
            // Typing a digit (or `-`/`.`) from ANY row jumps to the
            // Custom row and starts date input with that keystroke —
            // starting to type a date is an unambiguous intent, and
            // silently ignoring it on preset rows reads as a dead key.
            KeyCode::Char(c) => {
                if let Some(c) = crate::InputKind::Filter.sanitize_input_char(c) {
                    state.set_filter_popup_selected(custom_idx);
                    state.set_filter_input_mode(true);
                    if let Some(input) = state.filter_input_mut() {
                        input.set(String::new());
                        input.insert_char(c);
                    }
                    state.set_filter_input_error(false);
                }
            }
            _ => {}
        }
    }
}

/// `ActivePopup::Detail { back: None }` branch — Session detail popup. Esc/q/i close,
/// Enter opens the conversation, ↑↓/j/k scroll, Space toggles pin, C opens
/// conversation in a new pane, S/r kicks off (re-)summary, R regenerates the
/// JSONL summary.
pub(crate) fn handle_session_detail_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('i') => state.close_detail(),
        KeyCode::Enter => {
            let Some(path) = crate::handlers::pane::detail_session_path(state) else {
                return;
            };
            // From the tab views the list follows the tab (Live, else Day); from
            // an open conversation it keeps its mode. A pane already showing
            // the transcript is focused, not reloaded.
            let from_tab = !state.show_conversation;
            let opened = if from_tab && state.tab == Tab::Live {
                open_live_conversation(state)
            } else if let Some(i) = state
                .panes
                .iter()
                .position(|p| p.file_path.as_deref() == Some(path.as_path()))
            {
                state.active_pane_index = Some(i);
                state.show_conversation = true;
                true
            } else {
                crate::handlers::pane::open_path_in_pane(state, &path)
            };
            if opened {
                if from_tab && state.tab != Tab::Live {
                    state.conv_list_mode = ConvListMode::Day;
                }
                state.active_popup = crate::ActivePopup::None;
                state.session_detail_override = None;
                state.session_detail_live_extra = None;
            } else if state.panes.iter().any(|p| p.loading) {
                state.toast("A conversation is still loading".to_string());
            }
        }
        KeyCode::Char(' ') => {
            if let Some(session) = crate::handlers::pane::detail_session(state) {
                crate::handlers::pane::toggle_pin(state, &session.file_path);
            }
        }
        KeyCode::Char('y') => {
            // Mirror the Live tab `y` binding: copy `cd ... && claude -r UUID`.
            if let Some(path) = crate::handlers::pane::detail_session(state).map(|s| s.file_path) {
                copy_resume_command(state, &path);
                state.needs_draw = true;
            }
        }
        KeyCode::Char('C') => {
            let no_loading_panes = state.panes.iter().all(|p| !p.loading);
            if no_loading_panes
                && let Some(session) = crate::handlers::pane::detail_session(state)
                && open_or_focus_pane(state, &session.file_path)
            {
                state.conv_list_mode = ConvListMode::Day;
                state.show_conversation = true;
                state.active_popup = crate::ActivePopup::None;
                state.session_detail_override = None;
                state.session_detail_live_extra = None;
            }
        }
        // `s` opens the AI summary popup (where `r` regenerates); `t` writes a
        // custom title directly here too (Enter saves, ^R AI-generates), so
        // writing a title doesn't require the summary-generation detour first.
        KeyCode::Char('s') => {
            if state.summary_task.is_none()
                && let Some(session) = crate::handlers::pane::detail_session(state)
            {
                handlers::tasks::start_session_summary(state, session, false);
            }
        }
        // `t` opens the title editor prefilled with the current title. Enter
        // saves the typed text; clearing it and pressing Enter AI-generates.
        KeyCode::Char('t') => {
            if let Some(session) = crate::handlers::pane::detail_session(state) {
                let back = match &state.active_popup {
                    crate::ActivePopup::Detail { back } => back.clone(),
                    _ => None,
                };
                open_title_editor(state, &session, crate::TitleEditReturn::Detail(back));
            }
        }
        _ => {
            if let Some(action) = scroll_action_for_key(&key) {
                state.session_detail_scroll =
                    apply_scroll_usize(state.session_detail_scroll, action);
            }
        }
    }
}

/// Copy the verified resume command for `jsonl_path` to the clipboard, or
/// say why there is none: Cowork sessions re-open from the desktop app, and
/// an unverifiable dir is stated honestly (`claude --resume` + Ctrl+A still
/// reaches the session) — never guessed, since a wrong `cd` makes
/// `claude -r` silently fail to find the session.
fn copy_resume_command(state: &mut AppState, jsonl_path: &std::path::Path) {
    if crate::infrastructure::is_cowork_audit_path(jsonl_path) {
        state.toast("Cowork — re-open from Claude Desktop");
        return;
    }
    let Some(session_id) = jsonl_path.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    match state.resume_dir(jsonl_path) {
        Some(dir) => {
            let cmd = crate::shell::resume_command(&dir.to_string_lossy(), session_id);
            state.clipboard_task = Some(crate::handlers::tasks::spawn_clipboard_write(cmd.clone()));
            state.toast(format!("Copied: {cmd}"));
        }
        None => state.toast("Resume dir unknown — use the `claude --resume` picker"),
    }
}

/// Lines ↑↓ move through a message taller than the pane: small enough to
/// read as a scroll, while `d` / `u` still page.
const CONV_LINE_STEP: usize = 3;

/// Opens the Live cursor's transcript in the active pane, with the
/// conversation list switched to Live so j/k walks the live sessions.
fn open_live_conversation(state: &mut AppState) -> bool {
    let Some(jsonl) = crate::live_selected_session(state).and_then(|s| s.jsonl_path.clone()) else {
        return false;
    };
    // `active_pane_index` carries keyboard focus: an empty-panes open that
    // left it unset would keep j/k on the session list.
    if state.panes.is_empty() {
        state.panes.push(ConversationPane::load_from(&jsonl));
        state.active_pane_index = Some(0);
    } else {
        let idx = state.active_pane_index.unwrap_or(0);
        state.panes[idx] = ConversationPane::load_from(&jsonl);
        state.active_pane_index = Some(idx);
    }
    state.conv_list_mode = ConvListMode::Live;
    state.show_conversation = true;
    // The Live conv list counts active + paused, the same index as the tab.
    state.selected_session = state.live_selected;
    true
}

/// Open `path` in a new pane, or focus the pane already showing it — a second
/// pane of one transcript scrolls and searches identically, so it adds nothing.
/// A full set of panes says so rather than ignoring the key. Returns whether a
/// pane now shows `path`.
fn open_or_focus_pane(state: &mut AppState, path: &std::path::Path) -> bool {
    if let Some(i) = state
        .panes
        .iter()
        .position(|p| p.file_path.as_deref() == Some(path))
    {
        state.active_pane_index = Some(i);
        state.toast(format!("Already open in pane {}", i + 1));
        return true;
    }
    if state.panes.len() >= MAX_PANES {
        state.toast(format!("{MAX_PANES} panes open — X closes the focused one"));
        return false;
    }
    state.panes.push(ConversationPane::load_from(path));
    state.active_pane_index = Some(state.panes.len() - 1);
    true
}

/// Filter popup with the cursor on the active period (the Custom row for a
/// custom range), so Enter without moving keeps the current filter.
pub(crate) fn open_filter_popup(state: &mut AppState) {
    let selected = if matches!(state.period_filter, PeriodFilter::Custom(_, _)) {
        PeriodFilter::ALL_VARIANTS.len()
    } else {
        PeriodFilter::ALL_VARIANTS
            .iter()
            .position(|&v| v == state.period_filter)
            .unwrap_or(0)
    };
    state.active_popup = crate::ActivePopup::FilterPopup {
        selected,
        input_mode: false,
        input: crate::TextInput::default(),
        input_error: false,
    };
}

/// Project popup with the cursor on the active project. Preselects against
/// `project_list_sorted`, the order the popup renders and Enter indexes — the
/// raw `project_list` order would land on an unrelated project.
pub(crate) fn open_project_popup(state: &mut AppState) {
    let selected = match &state.project_filter {
        Some(name) => state
            .project_list_sorted()
            .iter()
            .position(|(n, _, _)| n == name)
            .map_or(0, |i| i + 1),
        None => 0,
    };
    state.active_popup = crate::ActivePopup::ProjectPopup {
        selected,
        scroll: 0,
    };
}

/// Enter / double-click in the filter popup: apply a preset, or start editing
/// the Custom row prefilled with the active custom range.
pub(crate) fn choose_filter_row(state: &mut AppState) {
    if state.filter_popup_selected() < PeriodFilter::ALL_VARIANTS.len() {
        state.period_filter = PeriodFilter::ALL_VARIANTS[state.filter_popup_selected()];
        state.apply_filter();
        state.active_popup = crate::ActivePopup::None;
        return;
    }
    state.set_filter_input_mode(true);
    let text = match state.period_filter {
        PeriodFilter::Custom(s, Some(e)) if s == e => s.format("%Y-%m-%d").to_string(),
        PeriodFilter::Custom(s, Some(e)) => {
            format!("{}..{}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
        }
        PeriodFilter::Custom(s, None) => s.format("%Y-%m-%d").to_string(),
        _ => String::new(),
    };
    if let Some(input) = state.filter_input_mut() {
        input.set(text);
    }
    state.set_filter_input_error(false);
}

/// Enter / double-click in the project popup. Row 0 is "All"; the rest index
/// `project_list_sorted`, the order the popup renders.
pub(crate) fn choose_project_row(state: &mut AppState) {
    let selected = state.project_popup_selected();
    state.project_filter = selected.checked_sub(1).and_then(|i| {
        state
            .project_list_sorted()
            .get(i)
            .map(|(name, _, _)| (*name).clone())
    });
    state.apply_filter();
    state.active_popup = crate::ActivePopup::None;
}

/// Drill into row `row` of the Projects list, resolved through the same sort
/// the panel, popup and click paths render.
pub(crate) fn open_project_detail(state: &mut AppState, row: usize) {
    let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
    state.sort_projects(&mut projects);
    if let Some((name, _)) = projects.get(row) {
        state.active_popup = crate::ActivePopup::ProjectDetail {
            path: (*name).clone(),
            scroll: 0,
            cursor: 0,
        };
    }
}

/// `s` on a ranked panel (Projects 1, Models 2, Ecosystem 3, Languages 4):
/// flip recency vs magnitude. The cursor indexes the sorted rows, so it and
/// the viewport restart at the top instead of landing on an unrelated row.
fn toggle_panel_sort(state: &mut AppState, panel: usize) {
    match panel {
        1 => state.dashboard_projects_sort = state.dashboard_projects_sort.toggle(),
        2 => state.dashboard_models_sort = state.dashboard_models_sort.toggle(),
        3 => {
            state.dashboard_ecosystem_sort = state.dashboard_ecosystem_sort.toggle();
            state.mcp_selected_server = 0;
            state.mcp_selected_tool = None;
        }
        4 => state.dashboard_languages_sort = state.dashboard_languages_sort.toggle(),
        _ => return,
    }
    state.dashboard_scroll[panel] = 0;
    state.dashboard_viewport[panel] = 0;
}

/// h / l across the dashboard panels, wrapping at both ends.
fn step_dashboard_panel(state: &mut AppState, forward: bool) {
    // One scroll slot per panel, so the array length is the panel count.
    let panels = state.dashboard_scroll.len();
    state.dashboard_panel = if forward {
        (state.dashboard_panel + 1) % panels
    } else {
        (state.dashboard_panel + panels - 1) % panels
    };
}

/// Show Ecosystem section `section` from its top. Section 0 (Tools) holds the
/// MCP server rows, whose cursor restarts so a value from another visit
/// cannot sit past the visible server list.
pub(crate) fn set_ecosystem_section(state: &mut AppState, section: usize) {
    state.tools_detail_section = section;
    state.dashboard_scroll[3] = 0;
    if section == 0 {
        state.mcp_selected_server = 0;
        state.mcp_selected_tool = None;
    }
}

/// Expand / collapse MCP server row `idx` of the Tools section.
pub(crate) fn toggle_mcp_server(state: &mut AppState, idx: usize) {
    let servers = crate::collect_mcp_servers(state);
    if let Some(name) = servers.get(idx)
        && !state.mcp_expanded_servers.remove(name)
    {
        state.mcp_expanded_servers.insert(name.clone());
    }
}

/// The session `s` summarizes in the conversation view: the focused pane's,
/// else the list cursor's. A subagent pane falls through to the list, since a
/// subagent transcript has no resume-title slot for the summary's `t`.
fn conv_summary_session(
    state: &AppState,
    list_file: fn(&AppState, usize) -> Option<std::path::PathBuf>,
) -> Option<crate::aggregator::SessionInfo> {
    let pane_path = state
        .active_pane_index
        .and_then(|i| state.panes.get(i))
        .and_then(|p| p.file_path.clone());
    [pane_path, list_file(state, state.selected_session)]
        .into_iter()
        .flatten()
        .find_map(|p| crate::handlers::pane::find_indexed_session_by_path(state, &p))
}

/// `m` / a click on the pin trigger: the conversation view on the Pinned
/// list, its first pin previewed.
pub(crate) fn open_pinned_view(state: &mut AppState, preview: fn(&mut AppState)) {
    state.conv_list_mode = ConvListMode::Pinned;
    state.selected_session = 0;
    state.tab = Tab::Daily;
    if !state.show_conversation {
        state.panes.clear();
        state.panes.push(ConversationPane::default());
        state.show_conversation = true;
    }
    state.active_pane_index = None;
    if state.panes.len() == 1 {
        preview(state);
    }
}

/// S-Tab / a click on the list title: Day → Pinned (when any) → All → Day,
/// with a lone pane following the list's new first row.
pub(crate) fn cycle_conv_list_mode(state: &mut AppState, preview: fn(&mut AppState)) {
    state.conv_list_mode = match state.conv_list_mode {
        ConvListMode::Day if state.pins.entries().is_empty() => ConvListMode::All,
        ConvListMode::Day => ConvListMode::Pinned,
        ConvListMode::Pinned => ConvListMode::All,
        ConvListMode::All => ConvListMode::Day,
        // Sticky once entered from the Live tab, so j/k keeps walking live rows.
        ConvListMode::Live => ConvListMode::Live,
    };
    state.selected_session = 0;
    if state.panes.len() == 1 {
        preview(state);
    }
}

/// Open the focused Dashboard panel's detail popup, its cursor on the row
/// the compact panel had selected.
pub(crate) fn open_dashboard_detail(state: &mut AppState) {
    state.active_popup = crate::ActivePopup::DashboardDetail;
}

/// `C` / Shift+Enter on Daily: the cursor's session in a pane of its own, or
/// focus on the pane already holding it.
fn open_selected_session_in_new_pane(state: &mut AppState) {
    if state.panes.iter().any(|p| p.loading) {
        return;
    }
    let path = state
        .daily_groups
        .get(state.selected_day)
        .and_then(|g| g.user_sessions().nth(state.selected_session))
        .map(|s| s.file_path.clone());
    if let Some(path) = path
        && open_or_focus_pane(state, &path)
    {
        state.conv_list_mode = ConvListMode::Day;
        state.show_conversation = true;
    }
}

/// Open the `TitleEdit` popup for `session`, prefilled with its current title
/// (cursor at end) so `t` starts an edit rather than a blank field.
/// `return_to` is where save / cancel land afterwards.
pub(crate) fn open_title_editor(
    state: &mut AppState,
    session: &crate::aggregator::SessionInfo,
    return_to: crate::TitleEditReturn,
) {
    // `session` is a clone from `daily_groups`, which a just-saved title does
    // NOT mutate — only the `session_titles` cache is updated until the next
    // data reload. Prefill from the cache first (same precedence as
    // `resolved_title`) so an immediate re-edit shows what was just saved
    // instead of silently reverting it on Enter.
    let current = state
        .session_titles
        .get(&session.file_path)
        .map(String::as_str)
        .or_else(|| session.display_title())
        .unwrap_or("");
    let mut input = crate::TextInput::default();
    input.set(current.to_string());
    input.move_end();
    state.active_popup = crate::ActivePopup::TitleEdit {
        input,
        path: session.file_path.clone(),
        return_to,
    };
}

/// `ActivePopup::TitleEdit` branch — free-text session-title editor. Enter
/// writes the typed text as a `custom-title` row; `Ctrl+R` AI-generates one
/// (regardless of field contents). Empty Enter is rejected, not a silent
/// AI-generate, so clearing the prefill then Enter can't destroy the title.
pub(crate) fn handle_title_edit_key(state: &mut AppState, key: KeyEvent) {
    // Ctrl+R AI-generates. Matched before `KeyCode::Char` so the modifier isn't
    // swallowed as a literal `r` insert.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
        let crate::ActivePopup::TitleEdit {
            path, return_to, ..
        } = std::mem::take(&mut state.active_popup)
        else {
            return;
        };
        state.active_popup = return_to.restore();
        match handlers::pane::find_session_indices_by_path(state, &path).and_then(
            |(day, sess, idx)| {
                let s = state.daily_groups.get(day)?.sessions.get(idx)?.clone();
                Some((s, day, sess))
            },
        ) {
            Some((session, day, sess)) => {
                handlers::tasks::start_jsonl_regen(state, session, day, sess);
                state.toast("Generating title…");
            }
            None => state.toast("Session no longer loaded — cannot write title"),
        }
        return;
    }
    match key.code {
        KeyCode::Esc => {
            if let crate::ActivePopup::TitleEdit { return_to, .. } = &state.active_popup {
                state.active_popup = return_to.clone().restore();
            }
        }
        KeyCode::Enter => {
            let title = state
                .title_input()
                .map(|i| i.text.trim().to_string())
                .unwrap_or_default();
            if title.is_empty() {
                // Keep the popup open; empty Enter is not a save nor an AI trigger.
                state.toast("Type a title, or ^R to AI-generate");
                return;
            }
            let crate::ActivePopup::TitleEdit {
                path, return_to, ..
            } = std::mem::take(&mut state.active_popup)
            else {
                return;
            };
            state.active_popup = return_to.restore();
            // Manual title → write the custom-title row (sessionId = file stem).
            let sid = path.file_stem().and_then(|s| s.to_str()).map(String::from);
            match sid {
                Some(sid) if crate::update_jsonl_custom_title(&path, &sid, &title).is_ok() => {
                    // Update the single title cache so every surface reflects it.
                    state.set_session_title(&path, &title);
                    state.toast("Title saved");
                }
                Some(_) => state.toast("❌ Failed to write title"),
                None => state.toast("❌ No session id — cannot write title"),
            }
        }
        code => {
            let crate::ActivePopup::TitleEdit { input, .. } = &mut state.active_popup else {
                return;
            };
            match code {
                KeyCode::Backspace => input.delete_back(),
                KeyCode::Left => input.move_left(),
                KeyCode::Right => input.move_right(),
                KeyCode::Home => input.move_home(),
                KeyCode::End => input.move_end(),
                KeyCode::Char(c) => input.insert_char(c),
                _ => {}
            }
        }
    }
}

/// `show_conversation` branch — handles in-pane content search AND pane
/// nav. Uses `return;` to short-circuit post-dispatch (only `needs_draw`
/// runs after; harmless to skip).
pub(crate) fn handle_conversation_key(
    state: &mut AppState,
    key: crossterm::event::KeyEvent,
    preview_conversation_in_pane: fn(&mut AppState),
    open_conversation_in_pane: fn(&mut AppState),
    get_conv_session_file: fn(&AppState, usize) -> Option<std::path::PathBuf>,
    get_conv_session_count: fn(&AppState) -> usize,
) {
    let pane_search_mode = state
        .active_pane_index
        .and_then(|i| state.panes.get(i))
        .is_some_and(|p| p.search_mode);

    if pane_search_mode {
        if let Some(pane) = state.active_pane_mut() {
            match key.code {
                KeyCode::Esc => {
                    // VS Code semantics: Esc ends the search outright —
                    // matches (and thus highlights) clear, the query stays
                    // for `/`-reopen. The last peeked message stays expanded
                    // so the found text survives the bar closing.
                    pane.search_mode = false;
                    pane.search_select_all = false;
                    pane.peek_expanded = None;
                    pane.pending_search_scroll = false;
                    if pane.search_matches.is_empty()
                        && let Some((saved_scroll, saved_msg)) = pane.search_saved_scroll.take()
                    {
                        pane.scroll = saved_scroll;
                        pane.selected_message = saved_msg;
                    }
                    pane.search_saved_scroll = None;
                    pane.search_matches.clear();
                }
                KeyCode::Enter if !pane.search_matches.is_empty() => {
                    pane.search_select_all = false;
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        pane.search_current = pane
                            .search_current
                            .checked_sub(1)
                            .unwrap_or(pane.search_matches.len() - 1);
                    } else {
                        pane.search_current = (pane.search_current + 1) % pane.search_matches.len();
                    }
                    pane.pending_search_scroll = true;
                }
                KeyCode::Backspace => {
                    if pane.search_select_all {
                        // Restored query is "selected": Backspace wipes it.
                        pane.search_input.set(String::new());
                    } else {
                        pane.search_input.delete_back();
                    }
                    ui::on_pane_search_edit(pane);
                }
                KeyCode::Left => {
                    pane.search_select_all = false;
                    pane.search_input.move_left();
                }
                KeyCode::Right => {
                    pane.search_select_all = false;
                    pane.search_input.move_right();
                }
                KeyCode::Home => {
                    pane.search_select_all = false;
                    pane.search_input.move_home();
                }
                KeyCode::End => {
                    pane.search_select_all = false;
                    pane.search_input.move_end();
                }
                KeyCode::Char(c) => {
                    if pane.search_select_all {
                        // First char replaces the restored query wholesale.
                        pane.search_input.set(String::new());
                    }
                    pane.search_input.insert_char(c);
                    ui::on_pane_search_edit(pane);
                }
                _ => {}
            }
        }
        return;
    }

    match key.code {
        KeyCode::Char('0') => {
            state.active_pane_index = None;
        }
        KeyCode::Char('1') | KeyCode::Char('2') | KeyCode::Char('3') | KeyCode::Char('4') => {
            let target_idx = match key.code {
                KeyCode::Char('1') => 0,
                KeyCode::Char('2') => 1,
                KeyCode::Char('3') => 2,
                KeyCode::Char('4') => 3,
                _ => unreachable!(),
            };
            if state.active_pane_index.is_none() {
                // Resolve through the same mode-aware lookup the preview uses.
                // Indexing `daily_groups` picks the day list's Nth session,
                // which is a different session while Pinned / Live is showing.
                if let Some(file_path) = get_conv_session_file(state, state.selected_session) {
                    // The preview already holds the cursor's session, so loading
                    // it again would put two identical panes side by side.
                    if let Some(existing) = state
                        .panes
                        .iter()
                        .position(|p| p.file_path.as_deref() == Some(file_path.as_path()))
                    {
                        state.active_pane_index = Some(existing);
                    } else {
                        let new_pane = ConversationPane::load_from(&file_path);
                        while state.panes.len() <= target_idx {
                            state.panes.push(ConversationPane::default());
                        }
                        state.panes[target_idx] = new_pane;
                        state.active_pane_index = Some(target_idx);
                    }
                }
            } else if target_idx < state.panes.len() {
                state.active_pane_index = Some(target_idx);
            }
        }
        KeyCode::Char('Q') => crate::close_conversation_view(state),
        KeyCode::Char('T') => {
            state.session_list_hidden = !state.session_list_hidden;
        }
        // Pane focused: accordion-toggle the cursor message in compact mode.
        // No pane focused (Daily session list): open the selected conversation.
        KeyCode::Enter => {
            if let Some(idx) = state.active_pane_index {
                if let Some(pane) = state.panes.get_mut(idx)
                    && pane.compact
                    && let Some(&(line, msg_idx)) = pane.message_lines.get(pane.selected_message)
                    && !pane.expanded.remove(&msg_idx)
                {
                    pane.expanded.insert(msg_idx);
                    // Align the just-expanded message to the viewport top so its
                    // body reads from the start — the message's first line is
                    // unchanged by expansion (preceding lines are untouched), and
                    // the draw clamps this down if it overshoots max_scroll.
                    pane.scroll = line;
                }
            } else {
                open_conversation_in_pane(state);
            }
        }
        // Toggle the whole pane between compact (one line per message) and the
        // classic full-transcript reading view.
        KeyCode::Char('c') => {
            if let Some(pane) = state.active_pane_mut() {
                pane.compact = !pane.compact;
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            // Session Detail closing lives in `handle_session_detail_key`
            // (dispatched before this). No residual-search cleanup either:
            // bar-Esc already cleared matches, and the query text survives
            // intentionally so `/` can restore it.
            if state.search_preview_mode {
                crate::return_to_search(state);
            } else if state.active_pane_index.is_some() {
                // Esc only ever backs out one level. Moving focus inward is
                // `Tab` / `1-4`, opening a pane is `C`, closing one is `X`.
                state.active_pane_index = None;
            } else {
                crate::close_conversation_view(state);
            }
        }
        // Pairs with `C` (open a pane). Esc no longer removes panes, so this
        // is the only way to drop one without closing the whole view.
        KeyCode::Char('X') => {
            if state.active_pane_index.is_none() && !state.panes.is_empty() {
                // The list has focus; say which key reaches a pane instead of
                // letting the close key do nothing.
                state.toast("Focus a pane (1-4) to close it".to_string());
            }
            if let Some(idx) = state.active_pane_index {
                state.panes.remove(idx);
                if state.panes.is_empty() {
                    crate::close_conversation_view(state);
                } else {
                    state.active_pane_index = Some(idx.min(state.panes.len() - 1));
                }
            }
        }
        KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
            let pane_count = state.panes.len();
            if pane_count > 0 {
                state.active_pane_index = match state.active_pane_index {
                    None => Some(0),
                    Some(idx) => {
                        if idx + 1 < pane_count {
                            Some(idx + 1)
                        } else {
                            None
                        }
                    }
                };
            }
        }
        KeyCode::Char('h') | KeyCode::Left => {
            let pane_count = state.panes.len();
            if pane_count > 0 {
                state.active_pane_index = match state.active_pane_index {
                    None => Some(pane_count - 1),
                    Some(idx) => {
                        if idx > 0 {
                            Some(idx - 1)
                        } else {
                            None
                        }
                    }
                };
            }
        }
        // `?` is advertised as Global — it must work here too, not just on
        // the tab views.
        KeyCode::Char('?') => {
            state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        }
        KeyCode::Char('/') => {
            if let Some(pane) = state.active_pane_mut() {
                pane.search_saved_scroll = Some((pane.scroll, pane.selected_message));
                pane.search_mode = true;
                // Reopen restores the previous query "selected" (VS Code):
                // highlights come back immediately, the first typed char
                // replaces the text, Enter continues from the top.
                pane.search_select_all = !pane.search_input.text.is_empty();
                pane.search_current = 0;
                ui::update_pane_search_matches(pane);
            }
        }
        // j/k bindings live with the Down/Up arms below
        // so vim-style nav and arrow-key nav share one
        // implementation (and the help popup's
        // `j/k: Select message` actually works once a
        // pane is focused).
        KeyCode::Char('H')
            if state.conv_list_mode == ConvListMode::Day
                && state.selected_day < state.daily_groups.len().saturating_sub(1) =>
        {
            state.selected_day += 1;
            state.selected_session = 0;
            if state.panes.len() == 1 {
                preview_conversation_in_pane(state);
            }
        }
        KeyCode::Char('L')
            if state.conv_list_mode == ConvListMode::Day && state.selected_day > 0 =>
        {
            state.selected_day -= 1;
            state.selected_session = 0;
            if state.panes.len() == 1 {
                preview_conversation_in_pane(state);
            }
        }
        KeyCode::Char(' ') => {
            // `show_detail` is intercepted by the dispatcher; here we only
            // need the in-conv-view session-pin path.
            if state.active_pane_index.is_none()
                && let Some(fp) = get_conv_session_file(state, state.selected_session)
            {
                crate::handlers::pane::toggle_pin(state, &fp);
            }
        }
        KeyCode::Char('m') => {
            // Toggle: Pinned <-> Day. Pressing `m` twice
            // returns to the daily view, matching the help
            // text's "m: pins" hint as a switch rather than
            // a one-way trip.
            state.conv_list_mode = match state.conv_list_mode {
                ConvListMode::Pinned => ConvListMode::Day,
                _ if state.pins.entries().is_empty() => state.conv_list_mode,
                _ => ConvListMode::Pinned,
            };
            state.selected_session = 0;
            state.active_pane_index = None;
            if state.panes.len() == 1 {
                preview_conversation_in_pane(state);
            }
        }
        KeyCode::Char('C') => {
            // Open the currently-selected session in a new side-by-side pane
            // (mirrors the default-mode handler so multi-pane is reachable
            // without escaping the conversation view first).
            let no_loading = state.panes.iter().all(|p| !p.loading);
            if no_loading
                // Mode-aware, like the digit keys: the day list's Nth session
                // is a different one while Pinned / Live is showing.
                && let Some(file_path) = get_conv_session_file(state, state.selected_session)
            {
                open_or_focus_pane(state, &file_path);
            }
        }
        KeyCode::BackTab if state.active_pane_index.is_none() => {
            cycle_conv_list_mode(state, preview_conversation_in_pane);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if state.active_pane_index.is_none() {
                let max = get_conv_session_count(state).saturating_sub(1);
                if state.selected_session < max {
                    state.selected_session += 1;
                } else if state.conv_list_mode == ConvListMode::Day
                    && state.selected_day < state.daily_groups.len().saturating_sub(1)
                {
                    state.selected_day += 1;
                    state.selected_session = 0;
                }
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            } else if let Some(pane) = state.active_pane_mut() {
                let msg_count = pane.message_lines.len();
                if msg_count > 0 {
                    // Mirror the draw-time clamp: `selected_message == usize::MAX`
                    // is the "scroll to end" sentinel set at load time; without
                    // this guard, `+ 1` below overflows before the first draw.
                    if pane.selected_message == usize::MAX || pane.selected_message >= msg_count {
                        pane.selected_message = msg_count - 1;
                    }
                    let cur = pane.selected_message;
                    // A message taller than the viewport must be read in chunks:
                    // reveal its hidden bottom first, advancing to the next message
                    // only once its end is on screen (this also covers the tail of
                    // the last message).
                    let scrolled_within = if let (Some(vh), Some(cached)) =
                        (pane.last_visible_height, pane.rendered.as_ref())
                    {
                        let total_lines = cached.0.len();
                        let sel_end = pane
                            .message_lines
                            .get(cur + 1)
                            .map_or(total_lines, |&(l, _)| l);
                        if sel_end > pane.scroll + vh {
                            pane.scroll =
                                (pane.scroll + CONV_LINE_STEP).min(sel_end.saturating_sub(vh));
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if !scrolled_within && cur + 1 < msg_count {
                        pane.selected_message = cur + 1;
                    }
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.active_pane_index.is_none() {
                if state.selected_session > 0 {
                    state.selected_session -= 1;
                } else if state.conv_list_mode == ConvListMode::Day && state.selected_day > 0 {
                    state.selected_day -= 1;
                    state.selected_session = get_conv_session_count(state).saturating_sub(1);
                }
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            } else if let Some(pane) = state.active_pane_mut() {
                let msg_count = pane.message_lines.len();
                if msg_count > 0
                    && (pane.selected_message == usize::MAX || pane.selected_message >= msg_count)
                {
                    pane.selected_message = msg_count - 1;
                }
                let cur = pane.selected_message;
                // Symmetric to j: if the selected message's top is scrolled above
                // the viewport, reveal its hidden head first, moving to the
                // previous message only once its start is on screen (this also
                // covers the head of the first message).
                let sel_start = pane.message_lines.get(cur).map_or(0, |&(l, _)| l);
                let scrolled_within = pane.last_visible_height.is_some() && sel_start < pane.scroll;
                if scrolled_within {
                    pane.scroll = pane.scroll.saturating_sub(CONV_LINE_STEP).max(sel_start);
                }
                if !scrolled_within && cur > 0 {
                    pane.selected_message -= 1;
                }
            }
        }
        KeyCode::PageDown | KeyCode::Char('d') => {
            // Auto-focus the first pane so d/u work even without pressing
            // Tab/l first — the help text implies these scroll the
            // conversation, so requiring explicit focus would feel like a
            // silent no-op.
            if state.active_pane_index.is_none() && !state.panes.is_empty() {
                state.active_pane_index = Some(0);
            }
            if let Some(pane) = state.active_pane_mut() {
                pane.scroll = pane.scroll.saturating_add(20);
                if !pane.message_lines.is_empty() {
                    let msg_idx = pane
                        .message_lines
                        .iter()
                        .position(|&(start, _)| start >= pane.scroll)
                        .unwrap_or(pane.message_lines.len() - 1);
                    pane.selected_message = msg_idx;
                }
            }
        }
        KeyCode::PageUp | KeyCode::Char('u') => {
            if state.active_pane_index.is_none() && !state.panes.is_empty() {
                state.active_pane_index = Some(0);
            }
            if let Some(pane) = state.active_pane_mut() {
                pane.scroll = pane.scroll.saturating_sub(20);
                if !pane.message_lines.is_empty() {
                    let msg_idx = pane
                        .message_lines
                        .iter()
                        .rposition(|&(start, _)| start <= pane.scroll)
                        .unwrap_or(0);
                    pane.selected_message = msg_idx;
                }
            }
        }
        KeyCode::Home | KeyCode::Char('g') => {
            if let Some(pane) = state.active_pane_mut() {
                pane.scroll = 0;
                pane.selected_message = 0;
            }
        }
        KeyCode::End | KeyCode::Char('G') => {
            if let Some(pane) = state.active_pane_mut() {
                pane.scroll = usize::MAX;
                let msg_count = pane.message_lines.len();
                if msg_count > 0 {
                    pane.selected_message = msg_count - 1;
                }
            }
        }
        // Pin reorder: J / K move the focused row down / up within the
        // Pinned list. Guarded on conv_list_mode so the same keys keep
        // their "scroll one message" meaning inside a focused pane.
        KeyCode::Char('J')
            if state.active_pane_index.is_none()
                && state.conv_list_mode == ConvListMode::Pinned =>
        {
            let idx = state.selected_session;
            if state.pins.move_down(idx) {
                state.selected_session = idx + 1;
                crate::handlers::pane::persist_pins(state);
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            }
        }
        KeyCode::Char('K')
            if state.active_pane_index.is_none()
                && state.conv_list_mode == ConvListMode::Pinned =>
        {
            let idx = state.selected_session;
            if state.pins.move_up(idx) {
                state.selected_session = idx - 1;
                crate::handlers::pane::persist_pins(state);
                if state.panes.len() == 1 {
                    preview_conversation_in_pane(state);
                }
            }
        }
        // Next / previous message start; J / K outside the Pinned list.
        KeyCode::Char('n' | 'J') => {
            if let Some(pane) = state.active_pane_mut()
                && let Some(&(next_pos, _)) = pane
                    .message_lines
                    .iter()
                    .find(|&&(pos, _)| pos > pane.scroll + 2)
            {
                pane.scroll = next_pos;
            }
        }
        KeyCode::Char('N' | 'K') => {
            if let Some(pane) = state.active_pane_mut() {
                if let Some(&(prev_pos, _)) = pane
                    .message_lines
                    .iter()
                    .rev()
                    .find(|&&(pos, _)| pos + 2 < pane.scroll)
                {
                    pane.scroll = prev_pos;
                } else {
                    pane.scroll = 0;
                }
            }
        }
        KeyCode::Char('y') => {
            // A compact row can fold several consecutive messages (a tool group)
            // into one `message_lines` entry, so copy every message from this
            // row's start up to the next row's start — not just the first. In
            // full mode each row is one message, so the range is a single entry.
            let content = state
                .active_pane_index
                .and_then(|idx| state.panes.get(idx))
                .and_then(|pane| {
                    let &(_, start_msg) = pane.message_lines.get(pane.selected_message)?;
                    let end_msg = pane
                        .message_lines
                        .get(pane.selected_message + 1)
                        .map_or(pane.messages.len(), |&(_, m)| m);
                    let text = pane
                        .messages
                        .get(start_msg..end_msg.min(pane.messages.len()))?
                        .iter()
                        .map(ui::extract_message_text)
                        .collect::<Vec<_>>()
                        .join("\n");
                    (!text.is_empty()).then_some(text)
                });
            if let Some(content) = content {
                let len = content.chars().count();
                state.toast(format!("Copied ({len} chars)"));
                state.clipboard_task = Some(crate::handlers::tasks::spawn_clipboard_write(content));
            }
        }
        KeyCode::Char('i') => {
            // Only the open path lives here; closing is owned by the
            // Session Detail popup's own handler (`i`/Esc/q/Enter).
            state.active_popup = crate::ActivePopup::Detail { back: None };
            state.session_detail_scroll = 0;
        }
        // Open the AI summary popup for the focused pane's session, falling back
        // to the list-selected session when no pane has focus. Regenerate / write
        // resume title (`r` / `t`) then live inside that popup.
        KeyCode::Char('s') if state.summary_task.is_none() => {
            let session = conv_summary_session(state, get_conv_session_file);
            if let Some(session) = session {
                handlers::tasks::start_session_summary(state, session, false);
            }
        }
        _ => {}
    }
}

/// Default branch (no popup, no search-mode, no conversation pane). Handles
/// global keys: tab switching, panel nav, summary/regen triggers, Daily-tab
/// session list nav. Returns `true` when the second `q` press confirms quit
/// — the caller breaks the event loop.
pub(crate) fn handle_default_key(
    state: &mut AppState,
    key: KeyEvent,
    preview_conversation_in_pane: fn(&mut AppState),
) -> bool {
    // Matching on `key.code` alone made Ctrl+F open the period filter, where
    // every other application starts a text search. Shift is deliberately not
    // listed: `J` / `X` / `C` / `Q` arrive carrying it.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return false;
    }
    match key.code {
        KeyCode::Char('q') => {
            if state.ctrl_c_pressed {
                return true;
            }
            state.ctrl_c_pressed = true;
            state.toast("Press q again to quit".to_string());
            state.needs_draw = true;
        }
        KeyCode::Esc if state.daily_breakdown_open => {
            state.daily_breakdown_open = false;
            state.daily_breakdown_scroll = 0;
        }
        KeyCode::Char('x')
            if state.retention_warning.is_some() && !state.retention_warning_dismissed =>
        {
            state.retention_warning_dismissed = true;
        }
        KeyCode::Char('?') => {
            state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        }
        KeyCode::Char(' ') if state.tab == Tab::Live => {
            // Pin / unpin the currently-selected Live row — same semantics as
            // Daily's Space handler so the `*` glyph in Live and Daily share
            // a single toggle path.
            let path = crate::live_selected_session(state).and_then(|s| s.jsonl_path.clone());
            if let Some(path) = path {
                crate::handlers::pane::toggle_pin(state, &path);
            }
        }
        KeyCode::Char('v') if state.tab == Tab::Live && state.live_view_snapshot_offset == 0 => {
            // Cycle Split → Active-only → Paused-only. Land the cursor in the
            // newly visible pane and reset both scrolls so the full-screen list
            // starts at the top.
            state.live_pane_mode = match state.live_pane_mode {
                crate::LivePaneMode::Split => crate::LivePaneMode::ActiveOnly,
                crate::LivePaneMode::ActiveOnly => crate::LivePaneMode::PausedOnly,
                crate::LivePaneMode::PausedOnly => crate::LivePaneMode::Split,
            };
            let (lo, _) = crate::live_selectable_range(state);
            state.live_selected = lo;
            state.live_scroll = 0;
            state.live_paused_scroll = 0;
        }
        KeyCode::Char('y') if state.tab == Tab::Live => {
            // Snapshot the fields before mutating `state` (toast) so the
            // `live_selected_session` borrow is released.
            let sel = crate::live_selected_session(state)
                .map(|s| (s.jsonl_path.clone(), s.cwd.clone(), s.session_id.clone()));
            if let Some((jsonl_path, pid_cwd, session_id)) = sel {
                if let Some(path) = jsonl_path {
                    copy_resume_command(state, &path);
                } else {
                    // No transcript on disk yet: its storage slug will derive
                    // from this very pid cwd when the first entry lands, so
                    // the pid cwd is correct by construction.
                    let cmd = crate::shell::resume_command(&pid_cwd.to_string_lossy(), &session_id);
                    state.clipboard_task =
                        Some(crate::handlers::tasks::spawn_clipboard_write(cmd.clone()));
                    state.toast(format!("Copied: {cmd}"));
                }
            }
        }
        KeyCode::Enter if state.tab == Tab::Live => {
            // A session too new to be indexed has no detail yet; its
            // transcript is still worth opening.
            let indexed = crate::live_selected_session(state)
                .and_then(|s| s.jsonl_path.clone())
                .is_some_and(|p| {
                    crate::handlers::pane::find_indexed_session_by_path(state, &p).is_some()
                });
            if indexed {
                crate::handlers::pane::open_live_session_detail(state);
            } else {
                open_live_conversation(state);
            }
        }
        KeyCode::Char('f') => open_filter_popup(state),
        KeyCode::Char('p') => open_project_popup(state),
        KeyCode::Char(' ') => {
            if state.tab == Tab::Daily
                && !state.show_conversation
                && let Some(session) = crate::current_selected_session(state)
            {
                crate::handlers::pane::toggle_pin(state, &session.file_path);
            }
        }
        KeyCode::Char('m') if !state.pins.entries().is_empty() => {
            open_pinned_view(state, preview_conversation_in_pane);
        }
        KeyCode::Char('/') => {
            // Pre-seed `filter:live ` only on clean Live-tab input; existing
            // input survives so Esc-then-/ restores the prior edit.
            if state.tab == Tab::Live && state.search_input.text.is_empty() {
                state.search_input.set("filter:live ".to_string());
            }
            // Re-populate the result list whenever the popup opens with
            // text — the Esc handler drops `search_results` to free memory,
            // so without this restored queries would render empty until
            // the user touched the input.
            if !state.search_input.text.is_empty() {
                crate::rerun_search(state, crate::start_content_search);
            }
            state.search_mode = true;
            state.search_input.move_end();
            // Restored query opens "selected" (VS Code find widget), same as
            // the pane search: the first typed char replaces it wholesale.
            // The just-seeded Live-tab `filter:live ` prefix stays appendable.
            let fresh_live_seed =
                state.tab == Tab::Live && state.search_input.text == "filter:live ";
            state.search_select_all = !state.search_input.text.is_empty() && !fresh_live_seed;
        }
        KeyCode::Tab
        | KeyCode::Char('1')
        | KeyCode::Char('2')
        | KeyCode::Char('3')
        | KeyCode::Char('4') => {
            if state.show_summary() || state.generating_summary {
                state.clear_summary();
            }
            state.active_popup = crate::ActivePopup::None;
            state.session_detail_override = None;
            state.session_detail_live_extra = None;
            state.daily_breakdown_open = false;
            state.daily_breakdown_scroll = 0;
            // No tab bar is drawn during the initial load (the splash owns
            // the whole screen), so tab switching is meaningless and would
            // only decide which view the user lands on when loading ends.
            // Ignore it entirely — load always resolves onto Dashboard.
            if state.loading {
                return false;
            }
            // Order: Dashboard → Live → Daily → Insights.
            state.tab = match key.code {
                KeyCode::Char('1') => Tab::Dashboard,
                KeyCode::Char('2') => Tab::Live,
                KeyCode::Char('3') => Tab::Daily,
                KeyCode::Char('4') => Tab::Insights,
                KeyCode::Tab => match state.tab {
                    Tab::Dashboard => Tab::Live,
                    Tab::Live => Tab::Daily,
                    Tab::Daily => Tab::Insights,
                    Tab::Insights => Tab::Dashboard,
                },
                _ => state.tab,
            };
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if state.tab == Tab::Dashboard {
                step_dashboard_panel(state, false);
            } else if state.tab == Tab::Live {
                step_live_view_snapshot(state, 1);
            } else if state.tab == Tab::Daily {
                step_daily_day(state, true);
            } else if state.tab == Tab::Insights {
                step_insights_panel(state, true);
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if state.tab == Tab::Dashboard {
                step_dashboard_panel(state, true);
            } else if state.tab == Tab::Live {
                step_live_view_snapshot(state, -1);
            } else if state.tab == Tab::Daily {
                step_daily_day(state, false);
            } else if state.tab == Tab::Insights {
                step_insights_panel(state, false);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.tab == Tab::Dashboard {
                // Panel 5 (heatmap) uses inverted scroll: older dates are at top
                if state.dashboard_panel == 5 {
                    let scroll = &mut state.dashboard_scroll[5];
                    *scroll = scroll.saturating_add(1);
                } else {
                    let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
                    if *scroll > 0 {
                        *scroll -= 1;
                    }
                }
            } else if state.tab == Tab::Daily {
                if state.daily_breakdown_open {
                    if state.daily_breakdown_scroll > 0 {
                        state.daily_breakdown_scroll -= 1;
                    }
                } else if state.daily_panel == DailyPanel::Breakdown {
                    // Only onto a panel the last frame drew (narrow terminals
                    // hide Activity, short ones both).
                    if state.layout.activity_panel_area.is_some() {
                        state.daily_panel = DailyPanel::Activity;
                    }
                } else if state.daily_panel == DailyPanel::Activity {
                    // The top panel: nothing above it to move to.
                } else if state.selected_session > 0 {
                    state.selected_session -= 1;
                } else if state.layout.breakdown_panel_area.is_some() {
                    // Past the first session, ↑ moves on to the panel above.
                    state.daily_panel = DailyPanel::Breakdown;
                }
            } else if state.tab == Tab::Live {
                crate::step_live_selection(state, true);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if state.tab == Tab::Dashboard {
                if state.dashboard_panel == 5 {
                    let scroll = &mut state.dashboard_scroll[5];
                    *scroll = scroll.saturating_sub(1);
                } else {
                    let max_items = crate::dashboard_max_items(state);
                    let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
                    if *scroll + 1 < max_items {
                        *scroll += 1;
                    }
                }
            } else if state.tab == Tab::Daily {
                if state.daily_breakdown_open {
                    if state.daily_breakdown_scroll < state.daily_breakdown_max_scroll {
                        state.daily_breakdown_scroll += 1;
                    }
                } else if state.daily_panel == DailyPanel::Activity {
                    state.daily_panel = DailyPanel::Breakdown;
                } else if state.daily_panel == DailyPanel::Breakdown {
                    // Back down from the panel to the first session.
                    state.daily_panel = DailyPanel::Sessions;
                    state.selected_session = 0;
                } else {
                    let max = state
                        .daily_groups
                        .get(state.selected_day)
                        .map_or(0, |g| g.user_sessions().count().saturating_sub(1));
                    if state.selected_session < max {
                        state.selected_session += 1;
                    }
                }
            } else if state.tab == Tab::Live {
                crate::step_live_selection(state, false);
            }
        }
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::SHIFT) && state.tab == Tab::Daily =>
        {
            open_selected_session_in_new_pane(state);
        }
        KeyCode::Enter => {
            if state.tab == Tab::Daily {
                // Enter opens the focused panel's detail, as on Dashboard; an
                // open Breakdown closes on it like on Esc.
                if state.daily_breakdown_open {
                    state.daily_breakdown_open = false;
                    state.daily_breakdown_scroll = 0;
                } else if state.daily_panel == DailyPanel::Breakdown {
                    state.daily_breakdown_open = true;
                    state.daily_breakdown_scroll = 0;
                } else if state.daily_panel == DailyPanel::Activity {
                    state.active_popup = crate::ActivePopup::DayActivity {
                        scroll: 0,
                        cursor: 0,
                    };
                } else {
                    state.active_popup = crate::ActivePopup::Detail { back: None };
                    state.session_detail_scroll = 0;
                }
            } else if state.tab == Tab::Dashboard {
                open_dashboard_detail(state);
            } else if state.tab == Tab::Insights {
                state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
            }
        }
        KeyCode::Char('C') if state.tab == Tab::Daily => open_selected_session_in_new_pane(state),
        KeyCode::Char('S') => {
            if state.tab == Tab::Daily
                && state.summary_task.is_none()
                && let Some(group) = state.daily_groups.get(state.selected_day).cloned()
            {
                handlers::tasks::start_day_summary(state, group, false);
            }
        }
        KeyCode::Char('s') => {
            if state.tab == Tab::Daily
                && state.summary_task.is_none()
                && let Some(session) = crate::current_selected_session(state)
            {
                handlers::tasks::start_session_summary(state, session, false);
            } else if state.tab == Tab::Dashboard {
                // The compact Ecosystem panel has no ranked list to reorder.
                if matches!(state.dashboard_panel, 1 | 2 | 4) {
                    toggle_panel_sort(state, state.dashboard_panel);
                }
            }
        }
        // Regenerate (`r`) and resume-title write (`t`) moved into the Summary
        // popup (`handle_summary_popup_key`) so they sit next to the summary
        // they act on, with consistent meaning across every view.
        KeyCode::Char('T') if state.tab == Tab::Daily && !state.daily_groups.is_empty() => {
            state.selected_day = 0;
            state.selected_session = 0;
        }
        KeyCode::Char('T') if state.tab == Tab::Live && state.live_view_snapshot_offset != 0 => {
            // Mirror Daily's `T` — jump to today. No-op when already on today.
            state.live_view_snapshot_offset = 0;
            state.live_past_sessions.clear();
            state.live_selected = 0;
            state.live_scroll = 0;
            state.needs_draw = true;
        }
        // `t` edits the selected session's title right from the list — same
        // key the Detail / Summary popups advertise, so it works everywhere
        // a session is selected.
        KeyCode::Char('t') if state.tab == Tab::Daily => {
            if let Some(session) = crate::current_selected_session(state) {
                open_title_editor(state, &session, crate::TitleEditReturn::Root);
            }
        }
        KeyCode::Char('t') if state.tab == Tab::Live => {
            let jsonl = crate::live_selected_session(state).and_then(|s| s.jsonl_path.clone());
            if let Some(jsonl) = jsonl {
                match crate::handlers::pane::find_indexed_session_by_path(state, &jsonl) {
                    Some(session) => {
                        open_title_editor(state, &session, crate::TitleEditReturn::Root);
                    }
                    None => state.toast("Session not yet indexed; ccsight reloads every 30s"),
                }
            }
        }
        KeyCode::Char('i') if state.tab == Tab::Insights => {
            state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        }
        // Unbound printable key: name it instead of silently ignoring, so a
        // slip (wrong tab, key from another view) is distinguishable from a
        // dead keyboard. Modified chords never reach here — the guard above
        // drops them, since terminals send many.
        KeyCode::Char(c) => {
            state.toast(format!("'{c}': no action here — ? shows key bindings"));
            state.needs_draw = true;
        }
        _ => {}
    }
    false
}

/// `ActivePopup::DashboardDetail` branch — Tools detail popup with
/// MCP-tab-specific cursor logic + cross-section/cross-panel navigation.
/// MCP tab uses row-based cursor (`mcp_selected_server` indexes the sorted
/// server list; `mcp_selected_tool` is `Option<usize>` for tool index within
/// the selected server). Enter/Space toggles expansion only on the header row.
pub(crate) fn handle_dashboard_detail_key(state: &mut AppState, key: KeyEvent) {
    // After the Built-in/MCP merge, MCP server-grouped rendering lives inside
    // the Tools tab (index 0). MCP-specific keys (Enter/Space/o/c, j/k cursor)
    // still target MCP rows there — built-in rows above them are non-navigable.
    let mcp_tab_active = state.dashboard_panel == 3 && state.tools_detail_section == 0;
    match key.code {
        KeyCode::Enter | KeyCode::Char(' ')
            if mcp_tab_active && state.mcp_selected_tool.is_none() =>
        {
            toggle_mcp_server(state, state.mcp_selected_server);
        }
        KeyCode::Char('o') if mcp_tab_active => {
            let servers = crate::collect_mcp_servers(state);
            state.mcp_expanded_servers = servers.into_iter().collect();
        }
        KeyCode::Char('c') if mcp_tab_active => {
            state.mcp_expanded_servers.clear();
            state.mcp_selected_tool = None;
        }
        KeyCode::Down | KeyCode::Char('j') if mcp_tab_active => {
            let servers = crate::collect_mcp_servers(state);
            let max_server = servers.len().saturating_sub(1);
            let cur_server = servers.get(state.mcp_selected_server);
            let cur_tool_count = cur_server.map_or(0, |s| crate::mcp_tool_count(state, s));
            let cur_expanded = cur_server.is_some_and(|s| state.mcp_expanded_servers.contains(s));
            match state.mcp_selected_tool {
                None if cur_expanded && cur_tool_count > 0 => {
                    state.mcp_selected_tool = Some(0);
                }
                Some(t) if t + 1 < cur_tool_count => {
                    state.mcp_selected_tool = Some(t + 1);
                }
                _ => {
                    if state.mcp_selected_server < max_server {
                        state.mcp_selected_server += 1;
                        state.mcp_selected_tool = None;
                    }
                }
            }
            crate::adjust_mcp_scroll(state, &servers);
        }
        KeyCode::Up | KeyCode::Char('k') if mcp_tab_active => {
            let servers = crate::collect_mcp_servers(state);
            match state.mcp_selected_tool {
                Some(0) => {
                    state.mcp_selected_tool = None;
                }
                Some(t) => {
                    state.mcp_selected_tool = Some(t - 1);
                }
                None => {
                    if state.mcp_selected_server > 0 {
                        state.mcp_selected_server -= 1;
                        let new_server = servers.get(state.mcp_selected_server);
                        let expanded =
                            new_server.is_some_and(|s| state.mcp_expanded_servers.contains(s));
                        let count = new_server.map_or(0, |s| crate::mcp_tool_count(state, s));
                        state.mcp_selected_tool = if expanded && count > 0 {
                            Some(count - 1)
                        } else {
                            None
                        };
                    }
                }
            }
            crate::adjust_mcp_scroll(state, &servers);
        }
        KeyCode::Char('s') if (1..=4).contains(&state.dashboard_panel) => {
            toggle_panel_sort(state, state.dashboard_panel);
        }
        KeyCode::Enter if state.dashboard_panel == 1 => {
            open_project_detail(state, state.dashboard_scroll[1]);
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
            state.active_popup = crate::ActivePopup::None;
        }
        KeyCode::Char('w') if state.dashboard_panel == 5 => {
            state.activity_view_weekly = !state.activity_view_weekly;
            // Reset scroll so flipping modes always lands the user at the top
            // of the new dataset; otherwise an out-of-range index from the
            // longer (daily) list would silently saturate at the end.
            state.dashboard_scroll[5] = 0;
        }
        // Costs (0) and Daily-Activity (5) share the active-day date set; toggle
        // filling idle calendar days. Reset scroll for the same reason as `w`.
        KeyCode::Char('z')
            if state.dashboard_panel == 0
                || (state.dashboard_panel == 5 && !state.activity_view_weekly) =>
        {
            state.show_empty_days = !state.show_empty_days;
            state.dashboard_scroll[state.dashboard_panel] = 0;
        }
        KeyCode::Up | KeyCode::Char('k') if state.dashboard_scroll[state.dashboard_panel] > 0 => {
            state.dashboard_scroll[state.dashboard_panel] -= 1;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let max_items = crate::dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            if *scroll + 1 < max_items {
                *scroll += 1;
            }
        }
        KeyCode::PageUp | KeyCode::Char('u') => {
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            *scroll = scroll.saturating_sub(10);
        }
        KeyCode::PageDown | KeyCode::Char('d') => {
            let max_items = crate::dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            *scroll = scroll.saturating_add(10).min(max_items.saturating_sub(1));
        }
        KeyCode::Home | KeyCode::Char('g') => {
            state.dashboard_scroll[state.dashboard_panel] = 0;
        }
        KeyCode::End | KeyCode::Char('G') => {
            let max_items = crate::dashboard_max_items(state);
            state.dashboard_scroll[state.dashboard_panel] = max_items.saturating_sub(1);
        }
        KeyCode::Char(c @ '1'..='4') if state.dashboard_panel == 3 => {
            set_ecosystem_section(state, (c as u8 - b'1') as usize);
        }
        // Tab/BackTab + ←/→ + h/l all cycle the Ecosystem section.
        // Forward (Tab / Right / l) advances; backward (BackTab /
        // Left / h) retreats. Mirrors number keys 1-4.
        KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') if state.dashboard_panel == 3 => {
            set_ecosystem_section(state, (state.tools_detail_section + 1) % 4);
        }
        KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') if state.dashboard_panel == 3 => {
            set_ecosystem_section(state, (state.tools_detail_section + 3) % 4);
        }
        KeyCode::Left | KeyCode::Char('h') => step_dashboard_panel(state, false),
        KeyCode::Right | KeyCode::Char('l') => step_dashboard_panel(state, true),
        _ => {}
    }
}

/// `ActivePopup::Summary` branch — Esc/q close the popup, ↑↓/j/k scroll the
/// body, `r` re-runs the summary generation, `t` opens the session title
/// editor (closing it returns to this popup).
pub(crate) fn handle_summary_popup_key(state: &mut AppState, key: KeyEvent) {
    match key.code {
        // Enter closes too — parity with the other detail popups, where a
        // no-op Enter reads as an unresponsive UI.
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
            state.clear_summary();
        }
        KeyCode::Char('r') => {
            if state.summary_task.is_none()
                && let Some(summary_type) = state.summary_type.clone()
            {
                match summary_type {
                    SummaryType::Session(session) => {
                        handlers::tasks::start_session_summary(state, *session, true);
                    }
                    SummaryType::Day(group) => {
                        handlers::tasks::start_day_summary(state, group, true);
                    }
                }
            }
        }
        // `t` opens the title editor for the session (prefilled). Enter saves
        // the typed text; ^R AI-generates. Session summaries only.
        KeyCode::Char('t') => {
            if let Some(SummaryType::Session(session)) = state.summary_type.clone() {
                open_title_editor(state, &session, crate::TitleEditReturn::Summary);
            }
        }
        _ => {
            if let Some(action) = scroll_action_for_key(&key) {
                state.set_summary_scroll(apply_scroll_usize(state.summary_scroll(), action));
            }
        }
    }
}

/// Enter / double-click on a search result: preview the hit's session in a
/// pane, remembering where `/` was pressed so Esc can return there.
pub(crate) fn open_search_hit(state: &mut AppState, open_conversation_in_pane: fn(&mut AppState)) {
    state.search_select_all = false;
    let result = state.search_results[state.search_selected].clone();
    let query = state.search_input.text.clone();
    // Persist the query so ↑ on the next `/` open recalls it.
    state.search_history.push(&query);
    let is_content = matches!(result.match_type, search::SearchMatchType::Content);
    if state.search_saved_state.is_none() {
        state.search_saved_state = Some((
            state.tab,
            state.selected_day,
            state.selected_session,
            state.show_conversation,
        ));
    }
    state.selected_day = result.day_idx;
    state.selected_session = result.session_idx;
    state.tab = Tab::Daily;
    state.search_mode = false;
    state.search_preview_mode = true;
    // The content search keeps running: its hits are there on the way back.
    open_conversation_in_pane(state);
    // Seed the in-pane search with the free text only: filter
    // tokens (`filter:` / `project:` / ...) are popup syntax, and
    // carried over verbatim they'd search for the literal token.
    let (_, free_text) = crate::search::parse_search_query(&query);
    if is_content
        && !free_text.is_empty()
        && let Some(pane) = state.active_pane_mut()
    {
        pane.search_input.set(free_text);
        pane.search_mode = true;
        // Seeded query opens "selected", like a `/` reopen: the user
        // didn't type it here, so their first keypress replaces it
        // instead of appending to it. Enter still walks matches
        // (navigation keys don't consume the selection's text).
        pane.search_select_all = true;
        pane.search_current = 0;
        pane.search_seed_day = result.message_date;
        // Messages load async; the draw recomputes matches once they
        // land and this flag then peeks + centers the first hit.
        pane.pending_search_scroll = true;
    }
}

/// `search_mode` branch — Esc restores prior tab/day/session, Enter jumps
/// to the hit, char keys edit the input and re-run incrementally. The two
/// `fn` callbacks (`start_content_search` / `open_conversation_in_pane`)
/// are parameters so tests can stub them.
pub(crate) fn handle_search_mode_key(
    state: &mut AppState,
    key: KeyEvent,
    start_content_search: fn(&mut AppState),
    open_conversation_in_pane: fn(&mut AppState),
) {
    match key.code {
        KeyCode::Esc => crate::close_search(state),
        KeyCode::Enter if !state.search_results.is_empty() => {
            open_search_hit(state, open_conversation_in_pane);
        }
        // ↑↓ walk history only on blank input or while already recalling;
        // typing reverts ↑↓ to list nav so users aren't yanked into
        // history. j/k always navigate results (vim consistency).
        KeyCode::Up => {
            state.search_select_all = false;
            let in_history_mode =
                state.search_input.text.is_empty() || state.search_history.is_browsing();
            if state.search_selected > 0 {
                state.search_selected -= 1;
            } else if in_history_mode {
                let draft = state.search_input.text.clone();
                if let Some(prev) = state.search_history.step_back(&draft) {
                    state.search_input.set(prev);
                    crate::rerun_search(state, start_content_search);
                }
            } else if !state.search_results.is_empty() {
                state.search_selected = state.search_results.len() - 1;
            }
        }
        KeyCode::Down => {
            state.search_select_all = false;
            if state.search_history.is_browsing() && state.search_selected == 0 {
                if let Some(next) = state.search_history.step_forward() {
                    state.search_input.set(next);
                    crate::rerun_search(state, start_content_search);
                }
                // step_forward past the newest entry restores the draft and
                // clears the cursor; the next press lands in the result-nav
                // branch below.
            } else if !state.search_results.is_empty() {
                state.search_selected = (state.search_selected + 1) % state.search_results.len();
            }
        }
        // j / k intentionally NOT mapped to result nav here — the popup is
        // a text input first, and any single character must reach
        // `search_input.insert_char` so queries that contain those letters
        // are typeable at all. ↑/↓ alone navigate results once the user has
        // typed something.
        KeyCode::Backspace => {
            state.search_history.reset_cursor();
            if state.search_select_all {
                // Restored query is "selected": Backspace wipes it.
                state.search_select_all = false;
                state.search_input.set(String::new());
            } else {
                state.search_input.delete_back();
            }
            crate::rerun_search(state, start_content_search);
        }
        KeyCode::Left => {
            state.search_select_all = false;
            state.search_input.move_left();
        }
        KeyCode::Right => {
            state.search_select_all = false;
            state.search_input.move_right();
        }
        KeyCode::Home => {
            state.search_select_all = false;
            state.search_input.move_home();
        }
        KeyCode::End => {
            state.search_select_all = false;
            state.search_input.move_end();
        }
        KeyCode::Char(c) => {
            state.search_history.reset_cursor();
            if state.search_select_all {
                // First char replaces the restored query wholesale.
                state.search_select_all = false;
                state.search_input.set(String::new());
            }
            state.search_input.insert_char(c);
            crate::rerun_search(state, start_content_search);
        }
        _ => {}
    }
}

/// `ActivePopup::ProjectPopup` branch — Esc/q/p close, ↑↓/j/k navigate,
/// Enter applies the project filter (idx 0 = "All").
pub(crate) fn handle_project_popup_key(state: &mut AppState, key: KeyEvent) {
    let total = state.project_list.len() + 1;
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => {
            state.active_popup = crate::ActivePopup::None;
        }
        KeyCode::Up | KeyCode::Char('k') if state.project_popup_selected() > 0 => {
            state.set_project_popup_selected(state.project_popup_selected() - 1);
        }
        KeyCode::Down | KeyCode::Char('j') if state.project_popup_selected() < total - 1 => {
            state.set_project_popup_selected(state.project_popup_selected() + 1);
        }
        KeyCode::Enter => choose_project_row(state),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(state: &mut AppState, code: KeyCode, mods: KeyModifiers) {
        handle_default_key(state, KeyEvent::new(code, mods), |_| {});
    }

    #[test]
    fn a_detail_opened_from_live_pins_the_session_it_shows() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(); // lint-ok: date-literal
        let under_cursor = make_session("cursor", None, None);
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            day,
            vec![under_cursor.clone()],
        )]);
        let shown = make_session("shown", None, None);
        state.session_detail_override = Some(shown.clone());
        state.active_popup = crate::ActivePopup::Detail { back: None };
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Char(' ')));
        assert!(state.pins.is_pinned(&shown.file_path));
        assert!(!state.pins.is_pinned(&under_cursor.file_path));
    }

    #[test]
    fn a_detail_over_the_conversation_view_pins_the_focused_panes_session() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(); // lint-ok: date-literal
        let under_cursor = make_session("cursor", None, None);
        let in_pane = make_session("pane", None, None);
        let group = make_daily_group(day, vec![under_cursor.clone(), in_pane.clone()]);
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![group]);
        state.show_conversation = true;
        let mut pane = ConversationPane::default();
        pane.file_path = Some(in_pane.file_path.clone());
        state.panes = vec![pane];
        state.active_pane_index = Some(0);
        state.active_popup = crate::ActivePopup::Detail { back: None };
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Char(' ')));
        assert!(state.pins.is_pinned(&in_pane.file_path));
        assert!(!state.pins.is_pinned(&under_cursor.file_path));
    }

    #[test]
    fn a_conversation_summary_targets_the_lists_session_not_the_day_cursor() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        fn pinned_row(_: &AppState, _: usize) -> Option<std::path::PathBuf> {
            Some(std::path::PathBuf::from("/tmp/test/pinned/none.jsonl"))
        }
        let day = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(); // lint-ok: date-literal
        let group = make_daily_group(
            day,
            vec![
                make_session("cursor", None, None),
                make_session("pinned", None, None),
            ],
        );
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![group]);
        state.show_conversation = true;
        let picked = conv_summary_session(&state, pinned_row).map(|s| s.project_name);
        assert_eq!(picked.as_deref(), Some("pinned"));
    }

    #[test]
    fn shift_enter_focuses_the_pane_already_holding_the_session() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let session = make_session("proj", None, None);
        let path = session.file_path.clone();
        let day = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(); // lint-ok: date-literal
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            day,
            vec![session],
        )]);
        state.tab = Tab::Daily;
        let mut open = ConversationPane::default();
        open.file_path = Some(path);
        state.panes = vec![open];
        press(&mut state, KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(state.panes.len(), 1, "no duplicate pane");
        assert_eq!(state.active_pane_index, Some(0));
    }

    // A detail popup opens on the row its compact panel had selected.
    #[test]
    fn the_models_popup_opens_on_the_panels_cursor() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = Tab::Dashboard;
        state.dashboard_panel = 2;
        state.dashboard_scroll[2] = 3;
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(state.show_dashboard_detail());
        assert_eq!(state.dashboard_scroll[2], 3);

        state.active_popup = crate::ActivePopup::None;
        state.dashboard_panel = 1;
        state.dashboard_scroll[1] = 3;
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(state.dashboard_scroll[1], 3, "Projects keeps its cursor");
    }

    // The detail's Enter replaces the focused pane, so four open panes do not
    // block it; a pane still loading does, and says so.
    #[test]
    fn the_details_enter_opens_with_every_pane_slot_taken() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(); // lint-ok: date-literal
        let session = make_session("p", None, None);
        let path = session.file_path.clone();
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            day,
            vec![session],
        )]);
        state.tab = Tab::Daily;
        state.panes = (0..crate::state::MAX_PANES)
            .map(|n| {
                let mut p = ConversationPane::default();
                p.file_path = Some(std::path::PathBuf::from(format!("/tmp/other-{n}.jsonl")));
                p
            })
            .collect();
        state.active_pane_index = Some(1);
        state.panes[3].loading = true;
        state.active_popup = crate::ActivePopup::Detail { back: None };
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(state.show_detail(), "a loading pane holds the open");
        assert!(
            state
                .toast_message
                .as_deref()
                .unwrap_or("")
                .contains("loading")
        );

        state.panes[3].loading = false;
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(!state.show_detail());
        assert_eq!(state.panes.len(), crate::state::MAX_PANES);
        assert_eq!(state.panes[1].file_path.as_deref(), Some(path.as_path()));
    }

    // ↑ only reaches panels the last frame drew: a short terminal draws
    // neither, a narrow one no Activity chart.
    #[test]
    fn up_skips_daily_panels_that_are_not_on_screen() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = Tab::Daily;
        press(&mut state, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Sessions, "no panel drawn");
        state.layout.breakdown_panel_area = Some(ratatui::layout::Rect::default());
        press(&mut state, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Breakdown);
        press(&mut state, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            state.daily_panel,
            DailyPanel::Breakdown,
            "no Activity drawn"
        );
    }

    // Daily moves focus with ↑ / ↓ and opens the focused panel with Enter,
    // as Dashboard does.
    #[test]
    fn daily_focuses_panels_and_enter_opens_the_focused_one() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = Tab::Daily;
        // As the last frame left them: both panels on screen.
        state.layout.breakdown_panel_area = Some(ratatui::layout::Rect::default());
        state.layout.activity_panel_area = Some(ratatui::layout::Rect::default());
        press(&mut state, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Breakdown, "↑ past the top");
        press(&mut state, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Activity, "then the chart");
        press(&mut state, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Activity, "nothing above it");
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(state.show_day_activity(), "Enter opens the activity detail");
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert!(!state.show_day_activity());
        press(&mut state, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Breakdown);
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            state.daily_breakdown_open,
            "Enter opens the focused breakdown"
        );
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!state.daily_breakdown_open, "and closes it again");
        press(&mut state, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(state.daily_panel, DailyPanel::Sessions);
        assert_eq!(state.selected_session, 0);
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!state.daily_breakdown_open);
        assert!(
            state.show_detail(),
            "sessions focus opens the session detail"
        );
        assert!(!state.show_conversation);
        state.active_popup = crate::ActivePopup::None;
        press(&mut state, KeyCode::Char('b'), KeyModifiers::NONE);
        assert!(!state.daily_breakdown_open);
        press(&mut state, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(!state.show_detail(), "Enter is the one key for the detail");
    }

    // ←/→ in the day's activity detail steps the day as the Breakdown does,
    // the popup staying open on the new day from its top.
    #[test]
    fn the_activity_detail_steps_the_day_and_stays_open() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(); // lint-ok: date-literal
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![
            make_daily_group(day, vec![make_session("p", None, None)]),
            make_daily_group(day.pred_opt().unwrap(), vec![make_session("q", None, None)]),
        ]);
        state.tab = Tab::Daily;
        state.active_popup = crate::ActivePopup::DayActivity {
            scroll: 4,
            cursor: 0,
        };
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Left));
        assert_eq!(state.selected_day, 1, "← is the older day");
        assert!(state.show_day_activity());
        assert_eq!(state.day_activity_scroll(), 0);
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Left));
        assert_eq!(state.selected_day, 1, "no day older than the last");
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(state.selected_day, 0);
    }

    // A detail opened from the Daily list closes to the list, not to Activity.
    #[test]
    fn a_detail_from_the_daily_list_closes_to_the_list() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![
            crate::test_helpers::helpers::make_daily_group(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), // lint-ok: date-literal
                vec![crate::test_helpers::helpers::make_session("p", None, None)],
            ),
        ]);
        state.tab = Tab::Daily;
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE);
        assert!(state.show_detail());
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert_eq!(state.active_popup, crate::ActivePopup::None);
    }

    // The Activity popup picks a session row with ↑↓ (kept on the rows);
    // Enter opens its detail, which closes back to that row.
    #[test]
    fn the_activity_detail_picks_and_opens_a_session() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(); // lint-ok: date-literal
        let mut early = make_session("early", None, None);
        early.day_hourly_work_tokens.insert(8, 10);
        let mut late = make_session("late", None, None);
        late.day_hourly_work_tokens.insert(20, 10);
        let late_path = late.file_path.clone();
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            day,
            vec![late, early],
        )]);
        state.tab = Tab::Daily;
        state.active_popup = crate::ActivePopup::DayActivity {
            scroll: 0,
            cursor: 0,
        };
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Down));
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Down));
        assert_eq!(state.day_activity_cursor(), 1, "kept on the last row");
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(
            state.show_detail(),
            "Enter opens the detail, not the transcript"
        );
        let shown = crate::current_selected_session(&state).map(|s| s.file_path);
        assert_eq!(
            shown,
            Some(late_path.clone()),
            "the row under the cursor, not list order"
        );

        // Closing the detail lands back on the Activity row it came from,
        // also after a round trip through the title editor.
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Char('t')));
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert!(state.show_detail(), "the editor returns to the detail");
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert!(state.show_day_activity(), "back to Activity, not Daily");
        assert_eq!(state.day_activity_cursor(), 1, "on the same row");
        handle_day_activity_key(&mut state, KeyEvent::from(KeyCode::Enter));

        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(!state.show_detail());
        assert!(
            state.show_conversation,
            "the detail's Enter opens the transcript"
        );
        assert_eq!(state.panes.len(), 1);
        assert_eq!(
            state.panes[0].file_path.as_deref(),
            Some(late_path.as_path())
        );

        // Reopening a transcript a pane already shows focuses it, not a reload.
        state.panes[0].loading = false;
        state.show_conversation = false;
        state.active_popup = crate::ActivePopup::Detail { back: None };
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(state.show_conversation);
        assert_eq!(state.panes.len(), 1);
        assert!(!state.panes[0].loading, "the open pane was not reloaded");
    }

    #[test]
    fn ctrl_modified_letters_are_not_taken_as_their_bare_binding() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        press(&mut state, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(
            !state.show_filter_popup(),
            "Ctrl+F must not open the filter"
        );
        press(&mut state, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(
            !state.show_project_popup(),
            "Ctrl+P must not open the filter"
        );

        // The bare keys still bind.
        press(&mut state, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(state.show_filter_popup());
    }

    #[test]
    fn shift_stays_a_binding_since_uppercase_letters_carry_it() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        // A terminal sends Shift+F as the uppercase char plus the modifier.
        // `F` is not bound here, so the unknown-key toast is the observable
        // that dispatch was reached at all.
        press(&mut state, KeyCode::Char('F'), KeyModifiers::SHIFT);
        assert!(
            state.toast_time.is_some(),
            "SHIFT must still reach dispatch"
        );
    }

    #[test]
    fn title_editor_prefills_from_the_cache_over_the_stale_session_clone() {
        // Saving a title updates `session_titles` only; the SessionInfo in
        // `daily_groups` keeps the old value until the next data reload. A
        // re-edit inside that window must show the just-saved title, or Enter
        // would silently revert it.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        let mut session = crate::test_helpers::helpers::make_session_with_tokens(
            "~/proj",
            10,
            10,
            "claude-sonnet-4-20250514",
        );
        session.custom_title = Some("old title".to_string());
        state
            .session_titles
            .insert(session.file_path.clone(), "new title".to_string());
        open_title_editor(&mut state, &session, crate::TitleEditReturn::Root);
        let crate::ActivePopup::TitleEdit { input, .. } = &state.active_popup else {
            panic!("editor did not open");
        };
        assert_eq!(input.text, "new title");
    }

    #[test]
    fn step_live_view_snapshot_at_today_warns_when_walking_forward() {
        // Forward (-1) from offset 0 → "Already at today" toast, offset
        // unchanged. Mirror case (oldest) depends on real disk contents
        // and varies across runs, so not exercised here.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.live_view_snapshot_offset = 0;
        step_live_view_snapshot(&mut state, -1);
        assert_eq!(state.live_view_snapshot_offset, 0);
        assert_eq!(state.toast_message.as_deref(), Some("Already at today"));
    }

    // Closing ProjectDetail must return to the Projects-list DashboardDetail
    // popup (two-level drill-back), never the bare dashboard — mutual
    // exclusion in ActivePopup makes None the natural wrong edit here.
    #[test]
    fn project_detail_esc_returns_to_dashboard_detail() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::ProjectDetail {
            path: "~/proj".to_string(),
            scroll: 0,
            cursor: 0,
        };
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert_eq!(state.active_popup, crate::ActivePopup::DashboardDetail);
        assert!(state.project_detail_path().is_empty());
    }

    fn noop(_: &mut AppState) {}

    #[test]
    fn tab_switch_ignored_while_loading() {
        // No tab bar is drawn during the initial load, so switching keys
        // must be inert — the view stays on Dashboard until data arrives.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.loading = true;
        state.tab = Tab::Dashboard;
        for code in [
            KeyCode::Char('2'),
            KeyCode::Char('3'),
            KeyCode::Char('4'),
            KeyCode::Tab,
        ] {
            handle_default_key(&mut state, KeyEvent::from(code), noop);
            assert!(state.tab == Tab::Dashboard, "loading must block {code:?}");
        }
    }

    #[test]
    fn title_edit_empty_enter_is_rejected_not_a_save() {
        // Clearing the prefill then Enter must not close the popup nor drop the
        // target path — otherwise an accidental empty Enter would lose the edit.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input: crate::TextInput::default(),
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Root,
        };
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(
                state.active_popup,
                crate::ActivePopup::TitleEdit { ref path, .. } if path.ends_with("x.jsonl")
            ),
            "stays open with the target path preserved"
        );
        assert!(state.toast_message.is_some(), "guidance toast shown");
    }

    #[test]
    fn title_edit_returns_to_its_opener_on_esc_and_save() {
        // Esc from a Detail-opened editor lands back on Detail, not root.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input: crate::TextInput::default(),
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Detail(None),
        };
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            state.active_popup,
            crate::ActivePopup::Detail { back: None }
        );

        // A successful save restores the Summary popup for a Summary opener.
        let dir = std::env::temp_dir().join(format!("ccsight-titleret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let jsonl = dir.join("abc123.jsonl");
        std::fs::write(&jsonl, "{}\n").unwrap();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        let mut input = crate::TextInput::default();
        input.set("renamed".to_string());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input,
            path: jsonl,
            return_to: crate::TitleEditReturn::Summary,
        };
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(state.show_summary(), "save lands back on the Summary popup");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_edit_plain_r_inserts_not_ai_trigger() {
        // Only Ctrl+R triggers AI-generate; a bare 'r' is a literal insert.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input: crate::TextInput::default(),
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Root,
        };
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(state.title_input().unwrap().text, "r");
        assert!(state.show_title_edit());
    }

    #[test]
    fn tab_switch_works_once_loaded() {
        // After loading clears, the same keys switch tabs normally.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.loading = false;
        state.tab = Tab::Dashboard;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('2')), noop);
        assert!(state.tab == Tab::Live);
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('3')), noop);
        assert!(state.tab == Tab::Daily);
    }

    fn conv_test_pane(texts: &[&str]) -> crate::ConversationPane {
        let msgs: Vec<crate::ConversationMessage> = texts
            .iter()
            .map(|t| crate::ConversationMessage {
                role: "user".to_string(),
                blocks: vec![crate::ConversationBlock::Text((*t).to_string())],
                timestamp: Some("10:00".to_string()),
                timestamp_utc: None,
                model: None,
                tokens: None,
                billed: Vec::new(),
            })
            .collect();
        crate::ConversationPane {
            messages: std::sync::Arc::new(msgs),
            ..Default::default()
        }
    }

    fn conv_key(state: &mut AppState, code: KeyCode) {
        fn no_file(_: &AppState, _: usize) -> Option<std::path::PathBuf> {
            None
        }
        fn no_count(_: &AppState) -> usize {
            0
        }
        handle_conversation_key(state, KeyEvent::from(code), noop, noop, no_file, no_count);
    }

    // Inside a message taller than the pane, ↑↓ scroll a few lines per press
    // (a page would jump past what was being read); past its end they move
    // to the neighbouring message.
    #[test]
    fn arrows_scroll_a_tall_message_a_few_lines_at_a_time() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.show_conversation = true;
        let mut pane = conv_test_pane(&["tall", "next"]);
        let lines: Vec<ratatui::text::Line<'static>> = (0..100).map(|_| "x".into()).collect();
        pane.message_lines = vec![(0, 0), (80, 1)];
        pane.rendered = Some((lines, pane.message_lines.clone(), vec![false; 100]));
        pane.last_visible_height = Some(20);
        pane.scroll = 0;
        pane.selected_message = 0;
        state.panes = vec![pane];
        state.active_pane_index = Some(0);

        conv_key(&mut state, KeyCode::Down);
        assert_eq!(
            (state.panes[0].scroll, state.panes[0].selected_message),
            (CONV_LINE_STEP, 0)
        );
        for _ in 0..30 {
            conv_key(&mut state, KeyCode::Down);
            if state.panes[0].selected_message == 1 {
                break;
            }
        }
        assert_eq!(
            state.panes[0].scroll, 60,
            "stopped with the message's end on screen"
        );
        assert_eq!(
            state.panes[0].selected_message, 1,
            "then on to the next message"
        );

        state.panes[0].selected_message = 0;
        state.panes[0].scroll = 30;
        conv_key(&mut state, KeyCode::Up);
        assert_eq!(state.panes[0].scroll, 30 - CONV_LINE_STEP);
        assert_eq!(state.panes[0].selected_message, 0);
    }

    #[test]
    fn n_and_capital_j_both_step_to_the_next_message_start() {
        for code in [KeyCode::Char('n'), KeyCode::Char('J')] {
            let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
            state.show_conversation = true;
            let mut pane = conv_test_pane(&["a", "b", "c"]);
            pane.message_lines = vec![(0, 0), (10, 1), (20, 2)];
            pane.scroll = 0;
            state.panes = vec![pane];
            state.active_pane_index = Some(0);
            conv_key(&mut state, code);
            assert_eq!(state.panes[0].scroll, 10, "{code:?}");
            for back in [KeyCode::Char('N'), KeyCode::Char('K')] {
                state.panes[0].scroll = 20;
                conv_key(&mut state, back);
                assert_eq!(state.panes[0].scroll, 10, "{back:?}");
            }
        }
    }

    // The pins / preview list already loads the cursor's session into pane 0,
    // so a digit press there would open a byte-identical second pane.
    #[test]
    fn a_digit_focuses_the_pane_that_already_holds_that_session() {
        // The resolver is the mode-aware lookup the preview also uses, so this
        // covers the Pinned / Live lists too — indexing `daily_groups` there
        // resolves a different session than the one on screen.
        fn fixed_file(_: &AppState, _: usize) -> Option<std::path::PathBuf> {
            Some(std::path::PathBuf::from("/s/0.jsonl"))
        }
        fn no_count(_: &AppState) -> usize {
            0
        }
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.show_conversation = true;
        let mut preview = conv_test_pane(&["a"]);
        preview.file_path = Some(std::path::PathBuf::from("/s/0.jsonl"));
        state.panes = vec![preview];
        state.active_pane_index = None;

        handle_conversation_key(
            &mut state,
            KeyEvent::from(KeyCode::Char('2')),
            noop,
            noop,
            fixed_file,
            no_count,
        );
        assert_eq!(state.panes.len(), 1, "no duplicate pane");
        assert_eq!(
            state.active_pane_index,
            Some(0),
            "focus moved to the holder"
        );
    }

    // Esc is the only key whose direction used to depend on focus: with the
    // list focused it stepped INTO a pane. One key that sometimes advances and
    // sometimes retreats is the thing a first-time user cannot recover from.
    #[test]
    fn esc_only_ever_steps_one_level_outward() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.show_conversation = true;
        state.panes = vec![conv_test_pane(&["a"]), conv_test_pane(&["b"])];

        state.active_pane_index = Some(1);
        conv_key(&mut state, KeyCode::Esc);
        assert_eq!(state.active_pane_index, None, "pane focus → list");
        assert_eq!(state.panes.len(), 2, "Esc must not remove a pane");
        assert!(state.show_conversation);

        conv_key(&mut state, KeyCode::Esc);
        assert!(!state.show_conversation, "list focus → leave the view");
        assert!(state.panes.is_empty());
    }

    // A second pane of the transcript already on screen scrolls and searches
    // identically; `C` on it must focus the open pane instead of adding one.
    #[test]
    fn capital_c_focuses_the_pane_already_showing_the_session() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let mut s = make_session("~/proj", None, None);
        s.file_path = std::path::PathBuf::from("/tmp/test/open.jsonl");
        let day = chrono::Local::now().date_naive();
        let mut state =
            crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(day, vec![s])]);
        state.show_conversation = true;
        state.conv_list_mode = ConvListMode::Day;
        let mut open = conv_test_pane(&["a"]);
        open.file_path = Some(std::path::PathBuf::from("/tmp/test/open.jsonl"));
        state.panes = vec![conv_test_pane(&["b"]), open];
        state.active_pane_index = None; // the list has focus, cursor on the session
        state.selected_session = 0;

        // The real session lookup: `conv_key` stubs it out to `None`.
        handle_conversation_key(
            &mut state,
            KeyEvent::from(KeyCode::Char('C')),
            noop,
            noop,
            crate::get_conv_session_file,
            crate::get_conv_session_count,
        );
        assert_eq!(state.panes.len(), 2, "a duplicate pane was opened");
        assert_eq!(
            state.active_pane_index,
            Some(1),
            "the open pane was not focused"
        );
    }

    // Pairs with `C`. Esc no longer removes panes, so without this key a
    // second pane could only be dropped by closing the whole view.
    #[test]
    fn capital_x_closes_only_the_focused_pane() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.show_conversation = true;
        state.panes = vec![conv_test_pane(&["a"]), conv_test_pane(&["b"])];
        state.active_pane_index = Some(0);

        conv_key(&mut state, KeyCode::Char('X'));
        assert_eq!(state.panes.len(), 1);
        assert_eq!(state.active_pane_index, Some(0));
        assert!(state.show_conversation);

        conv_key(&mut state, KeyCode::Char('X'));
        assert!(state.panes.is_empty());
        assert!(
            !state.show_conversation,
            "last pane closed → leave the view"
        );
    }

    #[test]
    fn question_mark_opens_help_from_conversation_view() {
        // `?` is documented as Global; the conversation view consumes all
        // keys, so it needs its own arm or the claim is false there.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        state.panes = vec![conv_test_pane(&["hello"])];
        conv_key(&mut state, KeyCode::Char('?'));
        assert!(matches!(
            state.active_popup,
            crate::ActivePopup::Help { .. }
        ));
    }

    #[test]
    fn pane_search_esc_ends_search_and_reopen_restores_query_selected() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.show_conversation = true;
        state.active_pane_index = Some(0);
        state.panes = vec![conv_test_pane(&["alpha beta", "beta gamma beta"])];

        conv_key(&mut state, KeyCode::Char('/'));
        for c in "beta".chars() {
            conv_key(&mut state, KeyCode::Char(c));
        }
        assert_eq!(
            state.panes[0].search_matches.len(),
            3,
            "occurrence-level matches across messages"
        );

        // Esc = VS Code pure: matches (highlights) die, the query survives.
        conv_key(&mut state, KeyCode::Esc);
        {
            let pane = &state.panes[0];
            assert!(!pane.search_mode);
            assert!(pane.search_matches.is_empty(), "highlights die with Esc");
            assert_eq!(pane.search_input.text, "beta", "query kept for reopen");
        }

        // Reopen: query restored selected; highlights return; first char
        // replaces the text wholesale.
        conv_key(&mut state, KeyCode::Char('/'));
        assert!(state.panes[0].search_select_all);
        assert_eq!(state.panes[0].search_matches.len(), 3);
        conv_key(&mut state, KeyCode::Char('g'));
        assert_eq!(state.panes[0].search_input.text, "g");
        assert!(!state.panes[0].search_select_all);
    }

    #[test]
    fn search_enter_seeds_pane_search_with_free_text_only() {
        // Filter tokens are popup-only syntax; carried into the in-pane
        // search bar verbatim they'd search for the literal token text.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.search_mode = true;
        state
            .search_input
            .set("filter:month project:kernel scheduler race".to_string());
        state.search_results = vec![crate::search::SearchResult {
            day_idx: 0,
            session_idx: 0,
            snippet: Some("…".to_string()),
            match_type: crate::search::SearchMatchType::Content,
            session_path: None,
            message_date: None,
        }];
        fn opener(state: &mut AppState) {
            state.panes.push(crate::ConversationPane::default());
            state.active_pane_index = Some(0);
        }
        handle_search_mode_key(&mut state, KeyEvent::from(KeyCode::Enter), noop, opener);
        let pane = &state.panes[0];
        assert_eq!(pane.search_input.text, "scheduler race");
        assert!(pane.search_mode);
        // Seeded text was not typed by the user: it opens "selected" so the
        // first keypress replaces it instead of appending.
        assert!(pane.search_select_all);
    }

    #[test]
    fn search_popup_reopen_restores_query_selected_and_first_char_replaces() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.search_input.set("resume".to_string());
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('/')), noop);
        assert!(state.search_mode);
        assert!(state.search_select_all, "restored query opens selected");
        handle_search_mode_key(&mut state, KeyEvent::from(KeyCode::Char('x')), noop, noop);
        assert_eq!(
            state.search_input.text, "x",
            "first char must replace, not append"
        );
        assert!(!state.search_select_all);
    }

    #[test]
    fn search_popup_live_tab_seed_stays_appendable() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = crate::Tab::Live;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('/')), noop);
        assert_eq!(state.search_input.text, "filter:live ");
        assert!(
            !state.search_select_all,
            "the fresh seed is a prefix to type after, not a query to replace"
        );
    }

    #[test]
    fn paste_replaces_a_selected_restored_query() {
        // Search popup input.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.search_mode = true;
        state.search_input.set("old".to_string());
        state.search_select_all = true;
        let kind = crate::paste_into_active_input(&mut state, "new");
        assert_eq!(kind, Some(crate::InputKind::Search));
        assert_eq!(state.search_input.text, "new");

        // Pane search input.
        state.search_mode = false;
        state.show_conversation = true;
        let mut pane = crate::ConversationPane::default();
        pane.search_mode = true;
        pane.search_input.set("old".to_string());
        pane.search_select_all = true;
        state.panes = vec![pane];
        state.active_pane_index = Some(0);
        let kind = crate::paste_into_active_input(&mut state, "xy");
        assert_eq!(kind, Some(crate::InputKind::PaneSearch));
        assert_eq!(state.panes[0].search_input.text, "xy");
    }

    #[test]
    fn daily_t_opens_title_editor_and_shift_t_jumps_to_today() {
        use crate::test_helpers::helpers::{
            make_daily_group, make_session_with_tokens, make_test_app_state,
        };
        let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 15).unwrap(); // lint-ok: date-literal
        let older = chrono::NaiveDate::from_ymd_opt(2026, 3, 14).unwrap(); // lint-ok: date-literal
        let groups = vec![
            make_daily_group(today, vec![make_session_with_tokens("~/proj", 10, 5, "m")]),
            make_daily_group(older, vec![make_session_with_tokens("~/proj", 10, 5, "m")]),
        ];
        let mut state = make_test_app_state(groups);
        state.tab = Tab::Daily;
        state.selected_day = 1;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('t')), noop);
        assert!(
            matches!(
                state.active_popup,
                crate::ActivePopup::TitleEdit {
                    return_to: crate::TitleEditReturn::Root,
                    ..
                }
            ),
            "t on the list edits the selected session's title"
        );

        state.active_popup = crate::ActivePopup::None;
        state.selected_day = 1;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('T')), noop);
        assert_eq!(state.selected_day, 0, "T keeps the jump-to-today binding");
        assert!(!state.show_title_edit());
    }

    #[test]
    fn live_past_view_shift_t_returns_to_now() {
        // `T` = jump to today/now on every list; past view must not leave
        // `t` doing double duty.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = Tab::Live;
        state.live_view_snapshot_offset = 3;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('T')), noop);
        assert_eq!(
            state.live_view_snapshot_offset, 0,
            "T returns to the now view"
        );
        assert!(!state.show_title_edit());
    }

    #[test]
    fn live_enter_opens_the_detail_and_its_enter_the_live_conversation() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(); // lint-ok: date-literal
        let mut indexed = make_session("live", None, None);
        let jsonl = std::path::PathBuf::from("/tmp/live-detail.jsonl");
        indexed.file_path = jsonl.clone();
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            day,
            vec![indexed],
        )]);
        state.tab = Tab::Live;
        state
            .live_active
            .push(crate::infrastructure::live_sessions::LiveSession {
                session_id: "sess-1".to_string(),
                jsonl_path: Some(jsonl.clone()),
                cwd: std::path::PathBuf::from("/tmp"),
                name: None,
                status: None,
                pid: 1,
                started_at: None,
                updated_at: None,
                jsonl_mtime: None,
                is_live: true,
                was_recently_live: false,
            });
        state.live_selected = 0;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Enter), noop);
        assert!(state.show_detail(), "Enter opens the detail");
        assert!(!state.show_conversation);

        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(!state.show_detail());
        assert!(state.show_conversation);
        assert!(matches!(state.conv_list_mode, ConvListMode::Live));
        assert_eq!(state.panes[0].file_path.as_deref(), Some(jsonl.as_path()));

        // Not indexed yet: no detail to show, so Enter opens the transcript.
        state.show_conversation = false;
        state.panes.clear();
        state.original_daily_groups.clear();
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Enter), noop);
        assert!(!state.show_detail());
        assert!(state.show_conversation);
    }

    #[test]
    fn live_t_on_unindexed_session_toasts_instead_of_opening_editor() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = Tab::Live;
        state
            .live_active
            .push(crate::infrastructure::live_sessions::LiveSession {
                session_id: "sess-1".to_string(),
                jsonl_path: Some(std::path::PathBuf::from("/tmp/not-indexed.jsonl")),
                cwd: std::path::PathBuf::from("/tmp"),
                name: None,
                status: None,
                pid: 1,
                started_at: None,
                updated_at: None,
                jsonl_mtime: None,
                is_live: true,
                was_recently_live: false,
            });
        state.live_selected = 0;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('t')), noop);
        assert!(
            !state.show_title_edit(),
            "no editor without an indexed session"
        );
        assert!(
            state
                .toast_message
                .as_deref()
                .unwrap_or("")
                .contains("not yet indexed"),
            "toast explains why nothing opened: {:?}",
            state.toast_message
        );
    }

    #[test]
    fn unbound_printable_key_names_itself_in_a_toast() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.tab = Tab::Dashboard;
        handle_default_key(&mut state, KeyEvent::from(KeyCode::Char('Z')), noop);
        assert!(
            state.toast_message.as_deref().unwrap_or("").contains("'Z'"),
            "unbound key is named: {:?}",
            state.toast_message
        );

        // Modified chords stay silent — terminals emit many of them.
        state.toast_message = None;
        let mut ev = KeyEvent::from(KeyCode::Char('z'));
        ev.modifiers = KeyModifiers::CONTROL;
        handle_default_key(&mut state, ev, noop);
        assert!(state.toast_message.is_none(), "ctrl chord must not toast");
    }

    #[test]
    fn help_scroll_saturates_at_zero() {
        // k (line up) and u (page up) at scroll 0 must stay at 0, never wrap.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        handle_help_key(&mut state, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(state.help_scroll(), 0);
        handle_help_key(&mut state, KeyEvent::from(KeyCode::Char('u')));
        assert_eq!(state.help_scroll(), 0);
        handle_help_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(state.help_scroll(), 1);
        handle_help_key(&mut state, KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(state.help_scroll(), 11);
    }

    #[test]
    fn help_end_and_home_jump_to_edges() {
        // G saturates to u16::MAX (draw clamps against content height); g → 0.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::Help { scroll: 5 };
        handle_help_key(&mut state, KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(state.help_scroll(), u16::MAX);
        handle_help_key(&mut state, KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(state.help_scroll(), 0);
    }

    /// A project detail over 30 one-session days of `proj`.
    fn project_detail_state() -> AppState {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let today = chrono::Local::now().date_naive();
        let groups = (0..30)
            .map(|d| {
                make_daily_group(
                    today - chrono::Duration::days(d),
                    vec![make_session("proj", None, None)],
                )
            })
            .collect();
        let mut state = crate::test_helpers::helpers::make_test_app_state(groups);
        state.active_popup = crate::ActivePopup::ProjectDetail {
            path: "proj".to_string(),
            scroll: 0,
            cursor: 0,
        };
        state
    }

    // Enter opens the Session Detail of the row under the cursor; closing it
    // lands back on the same row.
    #[test]
    fn a_project_rows_enter_opens_its_detail_and_esc_returns() {
        let mut state = project_detail_state();
        let rows: Vec<std::path::PathBuf> = crate::ui::project_session_rows(&state, "proj")
            .iter()
            .map(|(_, s)| s.file_path.clone())
            .collect();
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(state.show_detail());
        let shown = state
            .session_detail_override
            .as_ref()
            .map(|s| s.file_path.clone());
        assert_eq!(
            shown,
            Some(rows[2].clone()),
            "the cursor's row, not the first"
        );
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert!(
            state.show_project_detail(),
            "back to the project, not the list"
        );
        assert_eq!(state.project_detail_cursor(), 2);
    }

    #[test]
    fn project_detail_cursor_saturates_at_zero() {
        let mut state = project_detail_state();
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(state.project_detail_cursor(), 0);
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Char('u')));
        assert_eq!(state.project_detail_cursor(), 0);
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(state.project_detail_cursor(), 1);
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(state.project_detail_cursor(), 11);
    }

    #[test]
    fn insights_detail_scroll_saturates_at_zero() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(state.insights_detail_scroll(), 0);
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(state.insights_detail_scroll(), 1);
    }

    #[test]
    fn summary_scroll_saturates_at_zero() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::Summary { scroll: 0 };
        handle_summary_popup_key(&mut state, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(state.summary_scroll(), 0);
        handle_summary_popup_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(state.summary_scroll(), 1);
    }

    // Every j/k-scrollable popup shares one key set via `scroll_action_for_key`
    // (PageUp/PageDown/Home/End alongside line-step) — pin it here so a
    // future popup can't end up with a narrower set than its siblings.
    #[test]
    fn project_detail_supports_page_and_edge_keys() {
        let mut state = project_detail_state();
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::End));
        assert_eq!(state.project_detail_cursor(), 29, "kept on the last row");
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::Home));
        assert_eq!(state.project_detail_cursor(), 0);
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::PageDown));
        assert_eq!(state.project_detail_cursor(), 10);
        handle_project_detail_key(&mut state, KeyEvent::from(KeyCode::PageUp));
        assert_eq!(state.project_detail_cursor(), 0);
    }

    #[test]
    fn insights_detail_supports_page_and_edge_keys() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 5 };
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(state.insights_detail_scroll(), 15);
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(state.insights_detail_scroll(), 0);
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(state.insights_detail_scroll(), usize::MAX);
    }

    #[test]
    fn session_detail_supports_page_and_edge_keys() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::Detail { back: None };
        state.session_detail_scroll = 5;
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::PageDown));
        assert_eq!(state.session_detail_scroll, 15);
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::Home));
        assert_eq!(state.session_detail_scroll, 0);
        handle_session_detail_key(&mut state, KeyEvent::from(KeyCode::End));
        assert_eq!(state.session_detail_scroll, usize::MAX);
    }

    #[test]
    fn summary_popup_supports_page_and_edge_keys() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::Summary { scroll: 5 };
        handle_summary_popup_key(&mut state, KeyEvent::from(KeyCode::PageUp));
        assert_eq!(state.summary_scroll(), 0);
        handle_summary_popup_key(&mut state, KeyEvent::from(KeyCode::End));
        assert_eq!(state.summary_scroll(), usize::MAX);
        handle_summary_popup_key(&mut state, KeyEvent::from(KeyCode::Home));
        assert_eq!(state.summary_scroll(), 0);
    }

    #[test]
    fn insights_detail_panel_cycle_wraps_and_resets_scroll() {
        // h/l cycle the panels with wraparound; every switch resets the
        // body scroll so the new panel starts at its top.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 5 };
        state.insights_panel = 0;
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('h')));
        assert_eq!(
            state.insights_panel,
            crate::ui::INSIGHTS_PANELS - 1,
            "left from panel 0 wraps to last"
        );
        assert_eq!(state.insights_detail_scroll(), 0, "switch resets scroll");
        handle_insights_detail_key(&mut state, KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(state.insights_panel, 0, "right from last panel wraps to 0");
    }

    fn filter_popup(selected: usize, input_mode: bool, text: &str) -> crate::ActivePopup {
        let mut input = crate::TextInput::default();
        input.set(text.to_string());
        crate::ActivePopup::FilterPopup {
            selected,
            input_mode,
            input,
            input_error: false,
        }
    }

    #[test]
    fn filter_popup_nav_clamps_to_row_range() {
        // Rows are the presets plus the Custom row; k at the top and j past
        // the bottom must both clamp instead of wrapping or overflowing.
        let total = PeriodFilter::ALL_VARIANTS.len() + 1;
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(0, false, "");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(state.filter_popup_selected(), 0, "k at top stays at top");
        for _ in 0..total + 5 {
            handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        }
        assert_eq!(
            state.filter_popup_selected(),
            total - 1,
            "j clamps at the Custom row"
        );
    }

    #[test]
    fn filter_popup_digit_on_custom_row_enters_input_mode() {
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(custom_idx, false, "");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Char('2')));
        assert!(
            state.filter_input_mode(),
            "digit on Custom row enters input"
        );
        assert_eq!(
            state.filter_input().unwrap().text,
            "2",
            "the triggering digit is the first character"
        );
    }

    #[test]
    fn filter_popup_digit_on_preset_row_jumps_to_custom_input() {
        // Starting to type a date from a preset row is unambiguous intent:
        // the cursor jumps to Custom and the digit lands in the field.
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(0, false, "");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Char('2')));
        assert!(state.filter_input_mode(), "digit starts date input");
        assert_eq!(state.filter_popup_selected(), custom_idx);
        assert_eq!(state.filter_input().map(|i| i.text.as_str()), Some("2"));
        // Non-date chars stay dead on preset rows (no accidental jump).
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(0, false, "");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Char('x')));
        assert!(!state.filter_input_mode());
        assert_eq!(state.filter_popup_selected(), 0);
    }

    #[test]
    fn filter_input_mode_ignores_non_date_chars() {
        // Only digits and the date separators reach the field; nav/popup keys
        // ('x'/'f'/'j') must neither edit the text nor close the popup.
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(custom_idx, true, "2026");
        for c in ['x', 'f', 'j', ' '] {
            handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(state.filter_input().unwrap().text, "2026");
        assert!(
            state.show_filter_popup(),
            "popup keys must not leak through"
        );
        assert!(state.filter_input_mode());
    }

    #[test]
    fn filter_enter_with_valid_year_applies_custom_filter() {
        // A bare `YYYY` expands to the whole calendar year, closes the popup,
        // and lands in `period_filter`.
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(custom_idx, true, "2026");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert_eq!(state.active_popup, crate::ActivePopup::None);
        let expected = PeriodFilter::Custom(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            Some(chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()), // lint-ok: date-literal
        );
        assert_eq!(state.period_filter, expected);
    }

    #[test]
    fn filter_enter_with_invalid_input_sets_error_and_stays_open() {
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(custom_idx, true, "2026-99");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert!(state.show_filter_popup(), "invalid input keeps the popup");
        assert!(state.filter_input_error(), "error flag set for the draw");
        assert_eq!(state.period_filter, PeriodFilter::All, "filter unchanged");
    }

    #[test]
    fn filter_esc_in_input_mode_returns_to_preset_list() {
        // First Esc backs out of the input field (popup stays, draft clears);
        // second Esc closes the popup — the two-step exit.
        let custom_idx = PeriodFilter::ALL_VARIANTS.len();
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        state.active_popup = filter_popup(custom_idx, true, "2026");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert!(state.show_filter_popup(), "first Esc keeps the popup open");
        assert!(!state.filter_input_mode());
        assert_eq!(state.filter_input().unwrap().text, "");
        handle_filter_popup_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert_eq!(state.active_popup, crate::ActivePopup::None);
    }

    fn state_with_projects(n: usize) -> AppState {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        state.project_list = (0..n).map(|i| (format!("proj{i}"), 100, date)).collect();
        state
    }

    #[test]
    fn project_popup_nav_clamps_to_row_range() {
        // Rows are "All" plus one per project; j past the last project and
        // k at "All" must both clamp.
        let mut state = state_with_projects(2);
        state.active_popup = crate::ActivePopup::ProjectPopup {
            selected: 0,
            scroll: 0,
        };
        handle_project_popup_key(&mut state, KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(state.project_popup_selected(), 0, "k at top stays at top");
        for _ in 0..10 {
            handle_project_popup_key(&mut state, KeyEvent::from(KeyCode::Char('j')));
        }
        assert_eq!(
            state.project_popup_selected(),
            2,
            "j clamps at the last project row"
        );
    }

    #[test]
    fn project_popup_enter_on_all_row_clears_filter() {
        let mut state = state_with_projects(2);
        state.project_filter = Some("proj1".to_string());
        state.active_popup = crate::ActivePopup::ProjectPopup {
            selected: 0,
            scroll: 0,
        };
        handle_project_popup_key(&mut state, KeyEvent::from(KeyCode::Enter));
        assert_eq!(state.project_filter, None, "row 0 = All clears the filter");
        assert_eq!(state.active_popup, crate::ActivePopup::None);
    }

    #[test]
    fn title_edit_cursor_keys_edit_variant_input() {
        // Cursor/edit keys must act on the input INSIDE the popup variant, so
        // a stale AppState-level field can't shadow it.
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        let mut input = crate::TextInput::default();
        input.set("abc".to_string());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input,
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Root,
        };
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Left));
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Backspace));
        assert_eq!(
            state.title_input().unwrap().text,
            "ac",
            "Left+Backspace removes the char before the cursor, not the tail"
        );
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Home));
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(state.title_input().unwrap().text, "xac");
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::End));
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Right));
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(
            state.title_input().unwrap().text,
            "xacz",
            "Right at end is a no-op; the char appends"
        );
    }

    #[test]
    fn title_edit_esc_closes_without_saving() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        let mut input = crate::TextInput::default();
        input.set("draft".to_string());
        state.active_popup = crate::ActivePopup::TitleEdit {
            input,
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            return_to: crate::TitleEditReturn::Root,
        };
        handle_title_edit_key(&mut state, KeyEvent::from(KeyCode::Esc));
        assert_eq!(state.active_popup, crate::ActivePopup::None);
        assert!(state.toast_message.is_none(), "no save/error toast on Esc");
    }

    #[test]
    fn esc_and_q_close_each_popup() {
        // Every popup that closes to None must do so on both Esc and q.
        // ProjectDetail (drill-back to DashboardDetail) and TitleEdit
        // (q is a literal insert) are covered by their own tests.
        type Handler = fn(&mut AppState, KeyEvent);
        let cases: Vec<(&str, crate::ActivePopup, Handler)> = vec![
            (
                "help",
                crate::ActivePopup::Help { scroll: 2 },
                handle_help_key,
            ),
            (
                "insights_detail",
                crate::ActivePopup::InsightsDetail { scroll: 2 },
                handle_insights_detail_key,
            ),
            (
                "summary",
                crate::ActivePopup::Summary { scroll: 2 },
                handle_summary_popup_key,
            ),
            (
                "filter",
                filter_popup(0, false, ""),
                handle_filter_popup_key,
            ),
            (
                "project",
                crate::ActivePopup::ProjectPopup {
                    selected: 0,
                    scroll: 0,
                },
                handle_project_popup_key,
            ),
            (
                "dashboard_detail",
                crate::ActivePopup::DashboardDetail,
                handle_dashboard_detail_key,
            ),
            (
                "session_detail",
                crate::ActivePopup::Detail { back: None },
                handle_session_detail_key,
            ),
        ];
        for (name, popup, handler) in &cases {
            for code in [KeyCode::Esc, KeyCode::Char('q')] {
                let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
                state.active_popup = popup.clone();
                handler(&mut state, KeyEvent::from(code));
                assert_eq!(
                    state.active_popup,
                    crate::ActivePopup::None,
                    "{name} must close on {code:?}"
                );
            }
        }
    }
}
