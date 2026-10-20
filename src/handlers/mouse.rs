//! Mouse-event handlers extracted from `main.rs::run`. Same conventions as
//! `keyboard.rs`: `&mut AppState` only, no return value, callers route by
//! `MouseEventKind`.

use crate::state::SCROLL_LINES;
use crate::{AppState, PeriodFilter, Tab};

/// `column`/`row` are inside the area iff both axes contain them. Used by
/// every popup/list/pane click test.
pub(crate) fn in_area(column: u16, row: u16, area: &ratatui::layout::Rect) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Handles a left-click — dispatches to the topmost popup, then to tab/panel
/// areas, finally to the conversation list. Click outside any handled region
/// dismisses the topmost overlay (per [`crate::dismiss_overlay`]).
pub(crate) fn handle_mouse_click(state: &mut AppState, column: u16, row: u16) {
    // Popups are topmost - check them first
    if state.show_filter_popup() {
        // Date-input mode is modal: a click can't move the selection, and
        // letting it fall through would close the popup underneath while
        // leaving filter_input_mode orphaned. dismiss_overlay exits input
        // mode first (the popup stays open), matching Esc.
        if state.filter_input_mode() {
            crate::dismiss_overlay(state);
            return;
        }
        if let Some(area) = state.layout.filter_popup_area
            && in_area(column, row, &area)
        {
            let relative_row = (row - area.y).saturating_sub(1) as usize;
            let total_items = PeriodFilter::ALL_VARIANTS.len() + 1;
            if relative_row < total_items {
                state.set_filter_popup_selected(relative_row);
            }
            return;
        }
        crate::dismiss_overlay(state);
        return;
    }

    if state.show_project_popup() {
        if let Some(area) = state.layout.project_popup_area
            && in_area(column, row, &area)
        {
            let relative_row = (row - area.y).saturating_sub(1) as usize;
            let clicked_idx = state.project_popup_scroll() + relative_row;
            let total = state.project_list.len() + 1;
            if clicked_idx < total {
                state.set_project_popup_selected(clicked_idx);
            }
            return;
        }
        crate::dismiss_overlay(state);
        return;
    }

    if state.search_mode
        && !state.search_results.is_empty()
        && let Some(area) = state.layout.search_results_area
        && in_area(column, row, &area)
    {
        let item_height = 2usize;
        let visible_items = area.height.saturating_sub(2) as usize / item_height;
        let scroll_start = if visible_items > 0 && state.search_selected >= visible_items {
            state.search_selected - visible_items + 1
        } else {
            0
        };
        // `search_results_area` is the inner sub-rect whose block has no top
        // border (Borders::LEFT|RIGHT|BOTTOM), so the first result line draws
        // at `area.y` itself — there is no header row to subtract. Each result
        // spans `item_height` rows.
        let relative_row = (row - area.y) as usize;
        let clicked_idx = scroll_start + relative_row / item_height;
        if clicked_idx < state.search_results.len() {
            state.search_selected = clicked_idx;
        }
        return;
    }

    // Projects detail popup (panel 1): clicking a project row moves the
    // cursor. Guarded by `!show_project_detail` so a click that falls through
    // an open per-project popup doesn't reset the cursor underneath.
    if state.show_dashboard_detail() && state.dashboard_panel == 1 && !state.show_project_detail() {
        for (idx, area) in state.layout.project_detail_row_areas.clone() {
            if in_area(column, row, &area) {
                state.dashboard_scroll[1] = idx;
                return;
            }
        }
    }

    // Ecosystem detail popup: tab click switches section.
    if state.show_dashboard_detail() && state.dashboard_panel == 3 {
        for (idx, area) in state.layout.tools_detail_tab_areas.clone() {
            if in_area(column, row, &area) {
                crate::handlers::keyboard::set_ecosystem_section(state, idx);
                return;
            }
        }
        // Tools tab: clicking a server row toggles expansion and moves cursor.
        if state.tools_detail_section == 0 {
            for (idx, area) in state.layout.mcp_server_row_areas.clone() {
                if in_area(column, row, &area) {
                    state.mcp_selected_server = idx;
                    state.mcp_selected_tool = None;
                    crate::handlers::keyboard::toggle_mcp_server(state, idx);
                    return;
                }
            }
        }
    }

    if let Some(idx) = cursor_row_at(state, column, row) {
        set_popup_cursor(state, idx);
        return;
    }

    // Detail-style popups (help/summary/detail/dashboard_detail/insights_detail):
    // click inside does nothing (preserves text selection), click outside dismisses.
    if crate::has_blocking_popup(state) {
        let popup_area = if state.show_summary() {
            state.layout.summary_popup_area
        } else {
            state.layout.active_popup_area
        };
        if popup_area.is_some_and(|a| in_area(column, row, &a)) {
            return;
        }
        crate::dismiss_overlay(state);
        return;
    }

    // Check if click is on help trigger button
    if let Some(area) = state.layout.help_trigger
        && in_area(column, row, &area)
    {
        state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        return;
    }

    // Check if click is on filter/project trigger buttons in tab bar
    if !state.show_conversation {
        if let Some(area) = state.layout.filter_popup_area_trigger
            && in_area(column, row, &area)
        {
            crate::handlers::keyboard::open_filter_popup(state);
            return;
        }
        if let Some(area) = state.layout.project_popup_area_trigger
            && in_area(column, row, &area)
        {
            crate::handlers::keyboard::open_project_popup(state);
            return;
        }
        if let Some(area) = state.layout.pin_view_trigger
            && in_area(column, row, &area)
            && !state.pins.entries().is_empty()
        {
            crate::handlers::keyboard::open_pinned_view(
                state,
                crate::handlers::pane::preview_conversation_in_pane,
            );
            return;
        }
    }

    // Check if click is on a tab (only when not showing conversation)
    if !state.show_conversation {
        let clicked_tab = state.layout.tab_areas.iter().find_map(|(tab, area)| {
            if in_area(column, row, area) {
                Some(*tab)
            } else {
                None
            }
        });
        if let Some(tab) = clicked_tab {
            // clear_summary() must run BEFORE the popup is cleared — it reads
            // show_summary() to decide whether there is summary state to drop.
            if state.show_summary() || state.generating_summary {
                state.clear_summary();
            }
            state.active_popup = crate::ActivePopup::None;
            state.daily_breakdown_open = false;
            state.daily_breakdown_scroll = 0;
            state.tab = tab;
            return;
        }
    }

    // Check if click is on session list title (mode toggle) in conversation view
    if state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, _, _)) = state.layout.session_list_area
        && column >= area.x
        && column < area.x + area.width
        && area.y > 0
        && row == area.y - 1
    {
        state.active_pane_index = None;
        crate::handlers::keyboard::cycle_conv_list_mode(
            state,
            crate::handlers::pane::preview_conversation_in_pane,
        );
        return;
    }

    // Check if click is on session list (in conversation view)
    if state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.layout.session_list_area
        && in_area(column, row, &area)
    {
        let relative_y = (row - area.y) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        let session_count = crate::get_conv_session_count(state);
        if clicked_idx < session_count {
            state.selected_session = clicked_idx;
        }
        state.active_pane_index = None;
        return;
    }

    // Check if click is on a pane (in conversation view)
    if state.show_conversation {
        // [i] button: per-pane right-aligned indicator on the info-area
        // summary row. Coords must match draw_conversation_pane's info_rect
        // (top-right of pane, occupies 3 cells just inside the right border).
        for (idx, area) in state.layout.pane_areas.iter().enumerate() {
            let has_session = state.panes.get(idx).is_some_and(|p| p.file_path.is_some());
            if !has_session {
                continue;
            }
            let btn_left = area.x + area.width.saturating_sub(4);
            let btn_right = area.x + area.width.saturating_sub(1);
            let btn_row = area.y + 1;
            if row == btn_row && column >= btn_left && column < btn_right {
                state.active_pane_index = Some(idx);
                state.active_popup = crate::ActivePopup::Detail { back: None };
                state.session_detail_scroll = 0;
                return;
            }
        }

        for (idx, area) in state.layout.pane_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.active_pane_index = Some(idx);
                // Conversation lines start below the top border AND the
                // session info header (3 lines when the pane has a loaded
                // session, 0 otherwise) — mirror `draw_conversation_pane`'s
                // `inner.y = area.y + 1 + info_height`. Computed before the
                // `get_mut` below to avoid a borrow conflict.
                let info_height: u16 =
                    if state.panes.get(idx).is_some_and(|p| p.file_path.is_some()) {
                        3
                    } else {
                        0
                    };
                let content_y = area.y + 1 + info_height;
                if row >= content_y
                    && let Some(pane) = state.panes.get_mut(idx)
                    && !pane.message_lines.is_empty()
                {
                    let clicked_line = pane.scroll + (row - content_y) as usize;
                    if let Some(msg_idx) = pane
                        .message_lines
                        .iter()
                        .rposition(|&(start, _)| start <= clicked_line)
                    {
                        pane.selected_message = msg_idx;
                    }
                }
                return;
            }
        }
    }

    // Tools panel: click on a category row (▶ marker) opens the detail popup with
    // that section pre-selected. Checked BEFORE the generic panel-click handler so
    // the row click takes priority over panel selection.
    if state.tab == Tab::Dashboard && !state.show_conversation {
        let category_areas = state.layout.tools_panel_category_areas.clone();
        for (section_idx, area) in category_areas {
            if in_area(column, row, &area) {
                state.dashboard_panel = 3;
                state.active_popup = crate::ActivePopup::DashboardDetail;
                crate::handlers::keyboard::set_ecosystem_section(state, section_idx);
                return;
            }
        }
    }

    // Check if click is on a dashboard panel
    if state.tab == Tab::Dashboard && !state.show_conversation {
        for (idx, area) in state.layout.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.dashboard_panel = idx;
                return;
            }
        }
    }

    // Check if click is on an insights panel
    if state.tab == Tab::Insights && !state.show_conversation {
        for (idx, area) in state.layout.insights_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.insights_panel = idx;
                return;
            }
        }
    }

    // Check if click is on Daily header (left/right navigation)
    if state.tab == Tab::Daily
        && !state.show_conversation
        && let Some(area) = state.layout.daily_header_area
        && in_area(column, row, &area)
    {
        let mid = area.x + area.width / 2;
        if column < mid {
            // Left half: go to older day
            if state.selected_day < state.daily_groups.len().saturating_sub(1) {
                state.selected_day += 1;
                state.selected_session = 0;
            }
        } else {
            // Right half: go to newer day
            if state.selected_day > 0 {
                state.selected_day -= 1;
                state.selected_session = 0;
            }
        }
        return;
    }

    // Check if click is on a session in Daily view
    if state.tab == Tab::Daily
        && !state.show_conversation
        && let Some((area, scroll, item_height)) = state.layout.session_list_area
        && in_area(column, row, &area)
    {
        // `session_list_area` records the OUTER bordered chunk (Borders::ALL),
        // so the first list line draws at `area.y + 1` (inside the top border).
        // Subtract that border row before dividing by `item_height`, otherwise
        // a click on a session's line2 resolves to the next session.
        let relative_y = (row - area.y).saturating_sub(1) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        if let Some(group) = state.daily_groups.get(state.selected_day) {
            let session_count = group.user_sessions().count();
            if clicked_idx < session_count {
                state.selected_session = clicked_idx;
                state.daily_panel = crate::state::DailyPanel::Sessions;
            }
        }
        return;
    }

    // Live tab session click. Active now and Recently paused render as two
    // separate framed panels (`draw_live`); each panel body is `line 0 =
    // header, rows at 1 + r*3`. Resolve which panel was clicked, then the
    // shared per-row math gives the global `live_selected` index. Past view
    // sets only `live_list_area` (single frame, base 0).
    if state.tab == Tab::Live {
        let active_count = state.live_active.len();
        // (frame_inner_rect, scroll, row_count, global_base)
        let frame = if let Some((area, scroll, count)) = state
            .layout
            .live_list_area
            .filter(|(a, _, _)| in_area(column, row, a))
        {
            Some((area, scroll, count, 0usize))
        } else if let Some((area, scroll)) = state
            .layout
            .live_paused_list_area
            .filter(|(a, _)| in_area(column, row, a))
        {
            Some((area, scroll, state.live_paused.len(), active_count))
        } else {
            None
        };
        if let Some((area, scroll, count, base)) = frame {
            const ROW_H: usize = 3;
            let line_idx = scroll + (row - area.y) as usize;
            // Line 0 is the panel's info header; rows start at line 1. A click
            // on the header (or on a filler line when the panel is empty)
            // resolves to no session.
            if line_idx >= 1 {
                let r = (line_idx - 1) / ROW_H;
                if r < count {
                    state.live_selected = base + r;
                }
            }
            return;
        }
    }

    // Click on empty area dismisses topmost overlay
    crate::dismiss_overlay(state);
}

/// The session row under the pointer in a popup with a session cursor.
fn cursor_row_at(state: &AppState, column: u16, row: u16) -> Option<usize> {
    let models = state.show_dashboard_detail() && state.dashboard_panel == 2;
    if !(state.show_day_activity() || state.show_project_detail() || models) {
        return None;
    }
    state
        .layout
        .cursor_row_areas
        .iter()
        .find(|(_, area)| in_area(column, row, area))
        .map(|(idx, _)| *idx)
}

fn set_popup_cursor(state: &mut AppState, idx: usize) {
    if state.show_day_activity() {
        state.set_day_activity_cursor(idx);
    } else if state.show_project_detail() {
        state.set_project_detail_cursor(idx);
    } else {
        state.dashboard_scroll[2] = idx;
    }
}

/// Handles a left-button double-click — popups, the Daily / Live session
/// lists (open the detail), and the day panels.
pub(crate) fn handle_double_click(state: &mut AppState, column: u16, row: u16) {
    if state.show_summary() {
        if let Some(popup_area) = state.layout.summary_popup_area
            && !in_area(column, row, &popup_area)
        {
            state.clear_summary();
        }
        return;
    }

    if state.show_filter_popup() {
        // Same modal rule as single-click: never let a double-click fall
        // through to the UI underneath while date input is active.
        if state.filter_input_mode() {
            crate::dismiss_overlay(state);
            return;
        }
        crate::handlers::keyboard::choose_filter_row(state);
        return;
    }

    if state.show_title_edit() {
        // Modal text input: a double-click must never fall through to open a
        // session underneath — absorb it (dismiss, matching Esc).
        crate::dismiss_overlay(state);
        return;
    }

    if state.show_project_popup() {
        crate::handlers::keyboard::choose_project_row(state);
        return;
    }

    if state.search_mode && !state.search_results.is_empty() {
        crate::handlers::keyboard::open_search_hit(state, crate::open_conversation_in_pane);
        return;
    }

    // Double-click on a project row → per-project popup (mirrors Enter).
    if state.show_dashboard_detail() && state.dashboard_panel == 1 && !state.show_project_detail() {
        for (idx, area) in state.layout.project_detail_row_areas.clone() {
            if in_area(column, row, &area) {
                state.dashboard_scroll[1] = idx;
                crate::handlers::keyboard::open_project_detail(state, idx);
                return;
            }
        }
    }

    if let Some(idx) = cursor_row_at(state, column, row) {
        set_popup_cursor(state, idx);
        if state.show_day_activity() {
            crate::handlers::keyboard::open_day_activity_session(state);
        } else if state.show_project_detail() {
            crate::handlers::keyboard::open_project_session_detail(state);
        }
        return;
    }

    if crate::has_blocking_popup(state) {
        return;
    }

    if state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.layout.session_list_area
        && in_area(column, row, &area)
    {
        let relative_y = (row - area.y) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        let session_count = crate::get_conv_session_count(state);
        if clicked_idx < session_count {
            state.selected_session = clicked_idx;
            crate::open_conversation_in_pane(state);
        }
        return;
    }

    if !state.show_conversation
        && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.layout.session_list_area
        && in_area(column, row, &area)
    {
        // `session_list_area` is the OUTER bordered chunk, so the first row sits
        // at `area.y + 1`; drop that border row before dividing (mirrors the
        // single-click handler — without it line2 opens the next session).
        let relative_y = (row - area.y).saturating_sub(1) as usize;
        let clicked_idx = scroll + relative_y / item_height;
        if let Some(group) = state.daily_groups.get(state.selected_day) {
            let session_count = group.user_sessions().count();
            if clicked_idx < session_count {
                state.selected_session = clicked_idx;
                state.active_popup = crate::ActivePopup::Detail { back: None };
                state.session_detail_scroll = 0;
            }
        }
        return;
    }

    if !state.show_conversation
        && state.tab == Tab::Daily
        && let Some(area) = state.layout.activity_panel_area
        && in_area(column, row, &area)
    {
        state.daily_panel = crate::state::DailyPanel::Activity;
        state.active_popup = crate::ActivePopup::DayActivity {
            scroll: 0,
            cursor: 0,
        };
        return;
    }

    if !state.show_conversation
        && state.tab == Tab::Daily
        && let Some(area) = state.layout.breakdown_panel_area
        && in_area(column, row, &area)
    {
        state.daily_panel = crate::state::DailyPanel::Breakdown;
        state.daily_breakdown_open = true;
        state.daily_breakdown_scroll = 0;
        return;
    }

    // The double-click's first click already moved `live_selected` to the row.
    if !state.show_conversation && state.tab == Tab::Live {
        let in_list = state
            .layout
            .live_list_area
            .is_some_and(|(a, _, _)| in_area(column, row, &a))
            || state
                .layout
                .live_paused_list_area
                .is_some_and(|(a, _)| in_area(column, row, &a));
        if in_list {
            crate::handlers::pane::open_live_session_detail(state);
        }
        return;
    }

    if !state.show_conversation && state.tab == Tab::Dashboard {
        for (idx, area) in state.layout.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.dashboard_panel = idx;
                crate::handlers::keyboard::open_dashboard_detail(state);
                return;
            }
        }
    }

    if !state.show_conversation && state.tab == Tab::Insights {
        for (idx, area) in state.layout.insights_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.insights_panel = idx;
                state.active_popup = crate::ActivePopup::InsightsDetail { scroll: 0 };
                return;
            }
        }
    }
}

/// Handles a wheel scroll event — popup-aware, panel-aware. `up = true` means
/// the user scrolled the wheel toward themselves (content moves down visually).
pub(crate) fn handle_mouse_scroll(state: &mut AppState, column: u16, row: u16, up: bool) {
    // Text-entry modals capture everything: a wheel tick while typing a
    // title or a custom date must not move state behind the input.
    if state.show_title_edit() {
        return;
    }
    if state.show_filter_popup() {
        if state.filter_input_mode() {
            return;
        }
        let max = PeriodFilter::ALL_VARIANTS.len();
        if up {
            state.set_filter_popup_selected(state.filter_popup_selected().saturating_sub(1));
        } else if state.filter_popup_selected() < max {
            state.set_filter_popup_selected(state.filter_popup_selected() + 1);
        }
        return;
    }

    if state.show_project_popup() {
        let max = state.project_list.len().saturating_sub(1);
        if up {
            state.set_project_popup_selected(state.project_popup_selected().saturating_sub(1));
        } else if state.project_popup_selected() < max {
            state.set_project_popup_selected(state.project_popup_selected() + 1);
        }
        return;
    }

    if state.show_project_detail() {
        // The view follows the session cursor, so the wheel steps the cursor.
        let action = if up {
            crate::handlers::keyboard::ScrollAction::Up(1)
        } else {
            crate::handlers::keyboard::ScrollAction::Down(1)
        };
        crate::handlers::keyboard::move_project_detail_cursor(state, action);
        return;
    }

    if state.show_summary() {
        if up {
            state.set_summary_scroll(state.summary_scroll().saturating_sub(SCROLL_LINES));
        } else {
            state.set_summary_scroll(state.summary_scroll() + SCROLL_LINES);
        }
        return;
    }

    if state.search_mode && !state.search_results.is_empty() {
        let max = state.search_results.len().saturating_sub(1);
        if up {
            state.search_selected = state.search_selected.saturating_sub(1);
        } else if state.search_selected < max {
            state.search_selected += 1;
        }
        return;
    }

    if state.show_help() {
        if up {
            state.set_help_scroll(state.help_scroll().saturating_sub(1));
        } else {
            state.set_help_scroll(state.help_scroll().saturating_add(1));
        }
        return;
    }

    if state.show_detail() {
        if up {
            state.session_detail_scroll = state.session_detail_scroll.saturating_sub(1);
        } else {
            state.session_detail_scroll = state.session_detail_scroll.saturating_add(1);
        }
        return;
    }

    if state.show_day_activity() {
        use crate::handlers::keyboard::ScrollAction;
        let step = if up {
            ScrollAction::Up(1)
        } else {
            ScrollAction::Down(1)
        };
        crate::handlers::keyboard::move_day_activity_cursor(state, step);
        return;
    }

    if state.show_insights_detail() {
        if up {
            state.set_insights_detail_scroll(
                state.insights_detail_scroll().saturating_sub(SCROLL_LINES),
            );
        } else {
            state.set_insights_detail_scroll(
                state.insights_detail_scroll().saturating_add(SCROLL_LINES),
            );
        }
        return;
    }

    if state.show_dashboard_detail() {
        if up {
            state.dashboard_scroll[state.dashboard_panel] =
                state.dashboard_scroll[state.dashboard_panel].saturating_sub(1);
        } else {
            let max_items = crate::dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            if *scroll + 1 < max_items {
                *scroll += 1;
            }
        }
        return;
    }

    if state.show_conversation {
        if let Some((area, _, _)) = state.layout.session_list_area
            && in_area(column, row, &area)
        {
            let max = crate::get_conv_session_count(state).saturating_sub(1);
            if up {
                state.selected_session = state.selected_session.saturating_sub(1);
            } else if state.selected_session < max {
                state.selected_session += 1;
            }
            return;
        }

        for (idx, area) in state.layout.pane_areas.iter().enumerate() {
            if in_area(column, row, area) {
                if let Some(pane) = state.panes.get_mut(idx) {
                    if up {
                        pane.scroll = pane.scroll.saturating_sub(SCROLL_LINES);
                    } else {
                        // Clamp eagerly: a scroll burst would otherwise drive
                        // `selected_message` (computed before the next draw)
                        // onto the last message, not the visible row.
                        let max_scroll =
                            pane.rendered.as_ref().map_or(usize::MAX, |(lines, _, _)| {
                                lines.len().saturating_sub(area.height as usize)
                            });
                        pane.scroll = pane.scroll.saturating_add(SCROLL_LINES).min(max_scroll);
                    }
                    if !pane.message_lines.is_empty() {
                        let msg_idx = if up {
                            pane.message_lines
                                .iter()
                                .rposition(|&(start, _)| start <= pane.scroll)
                                .unwrap_or(0)
                        } else {
                            pane.message_lines
                                .iter()
                                .position(|&(start, _)| start >= pane.scroll)
                                .unwrap_or(pane.message_lines.len() - 1)
                        };
                        pane.selected_message = msg_idx;
                    }
                }
                return;
            }
        }

        return;
    }

    if state.tab == Tab::Dashboard {
        for (idx, area) in state.layout.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                let (scroll_up, scroll_down) = if idx == 5 { (!up, up) } else { (up, !up) };
                if scroll_up {
                    state.dashboard_scroll[idx] = state.dashboard_scroll[idx].saturating_sub(1);
                } else if scroll_down {
                    let saved = state.dashboard_panel;
                    state.dashboard_panel = idx;
                    let max_items = crate::dashboard_max_items(state);
                    state.dashboard_panel = saved;
                    if state.dashboard_scroll[idx] + 1 < max_items {
                        state.dashboard_scroll[idx] += 1;
                    }
                }
                return;
            }
        }
    }

    if state.tab == Tab::Live {
        // Scroll = j/k. The draw pass moves `live_scroll` to keep the
        // selection in view, so only `live_selected` needs the nudge.
        crate::step_live_selection(state, up);
        return;
    }

    if state.tab == Tab::Daily {
        if state.daily_breakdown_open {
            if up {
                state.daily_breakdown_scroll =
                    state.daily_breakdown_scroll.saturating_sub(SCROLL_LINES);
            } else if state.daily_breakdown_scroll < state.daily_breakdown_max_scroll {
                state.daily_breakdown_scroll = (state.daily_breakdown_scroll + SCROLL_LINES)
                    .min(state.daily_breakdown_max_scroll);
            }
        } else {
            let max = state
                .daily_groups
                .get(state.selected_day)
                .map_or(0, |g| g.user_sessions().count().saturating_sub(1));
            if up {
                state.selected_session = state.selected_session.saturating_sub(1);
            } else if state.selected_session < max {
                state.selected_session += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::helpers::{make_daily_group, make_session, make_test_app_state};
    use ratatui::layout::Rect;

    fn daily_state_for_click() -> AppState {
        // The mouse-click math is independent of the date; pick a fixed
        // reference so the fixture does not couple to the wall clock.
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let group = make_daily_group(date, vec![make_session("proj", None, None)]);
        let mut state = make_test_app_state(vec![group]);
        state.tab = Tab::Daily;
        state.show_conversation = false;
        // session_list_area: full-width 140 col panel, height 10, starting at row 13.
        // item_height = 2 (line1 + line2 per session).
        state.layout.session_list_area = Some((
            Rect {
                x: 0,
                y: 13,
                width: 140,
                height: 10,
            },
            0,
            2,
        ));
        state
    }

    #[test]
    fn a_trigger_click_opens_its_popup_on_the_active_value() {
        let mut state = daily_state_for_click();
        let trigger = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 1,
        };
        state.period_filter = PeriodFilter::ALL_VARIANTS[1];
        state.layout.filter_popup_area_trigger = Some(trigger);
        handle_mouse_click(&mut state, 1, 0);
        assert_eq!(state.filter_popup_selected(), 1);

        state.active_popup = crate::ActivePopup::None;
        state.layout.filter_popup_area_trigger = None;
        state.project_filter = Some("proj".to_string());
        state.layout.project_popup_area_trigger = Some(trigger);
        handle_mouse_click(&mut state, 1, 0);
        assert!(
            matches!(
                state.active_popup,
                crate::ActivePopup::ProjectPopup { selected: 1, .. }
            ),
            "{:?}",
            state.active_popup
        );
    }

    // A click selects; the detail opens on Enter or a double-click, so the
    // row's right edge is no hidden button.
    #[test]
    fn a_session_click_only_selects() {
        for column in [10, 137] {
            let mut state = daily_state_for_click();
            state.daily_panel = crate::state::DailyPanel::Breakdown;
            handle_mouse_click(&mut state, column, 14);
            assert!(!state.show_detail(), "column {column}");
            assert_eq!(state.selected_session, 0);
            assert_eq!(state.daily_panel, crate::state::DailyPanel::Sessions);
        }
    }

    #[test]
    fn click_on_line2_of_info_column_does_not_open() {
        let mut state = daily_state_for_click();
        // Row 15 = line2 of session 0 (relative_y=2, line_in_session=1).
        // Even at the info-button column, line2 must not trigger the popup.
        handle_mouse_click(&mut state, 137, 15);
        assert!(
            !state.show_detail(),
            "line2 click in info column must not open detail"
        );
    }

    fn conv_state_for_click() -> AppState {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let session = make_session("proj", None, None);
        let group = make_daily_group(date, vec![session.clone()]);
        let mut state = make_test_app_state(vec![group]);
        state.tab = Tab::Daily;
        state.show_conversation = true;
        // One pane occupying right half: x=70, width=70. The info-area [i]
        // button hits at row=area.y+1=6, cols [70+66, 70+69) = [136, 139).
        let mut pane = crate::state::ConversationPane::default();
        pane.file_path = Some(session.file_path.clone());
        state.panes.push(pane);
        state.layout.pane_areas = vec![Rect {
            x: 70,
            y: 5,
            width: 70,
            height: 30,
        }];
        state
    }

    #[test]
    fn conv_info_button_click_opens_detail() {
        let mut state = conv_state_for_click();
        // row=6 (area.y+1), col=137 → inside [136, 139)
        handle_mouse_click(&mut state, 137, 6);
        assert!(
            state.show_detail(),
            "[i] in conv pane should open session detail"
        );
        assert_eq!(state.active_pane_index, Some(0));
    }

    #[test]
    fn conv_pane_body_click_does_not_open_detail() {
        let mut state = conv_state_for_click();
        // Click inside pane body (not on the info button row/column).
        handle_mouse_click(&mut state, 100, 10);
        assert!(
            !state.show_detail(),
            "body click in conv pane should not open session detail"
        );
        assert_eq!(state.active_pane_index, Some(0));
    }

    #[test]
    fn conv_info_button_click_ignored_without_session() {
        let mut state = conv_state_for_click();
        // Drop the file_path so the pane has no session.
        state.panes[0].file_path = None;
        handle_mouse_click(&mut state, 137, 6);
        assert!(
            !state.show_detail(),
            "without a session, [i] click must not open detail"
        );
    }

    // The list block has a top border, so list line N draws at area.y+1+N.
    // A click on a session's line2 must select THAT session, not the next.
    #[test]
    fn daily_click_on_line2_selects_same_session() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let group = make_daily_group(
            date,
            vec![
                make_session("p0", None, None),
                make_session("p1", None, None),
            ],
        );
        let mut state = make_test_app_state(vec![group]);
        state.tab = Tab::Daily;
        state.show_conversation = false;
        state.layout.session_list_area = Some((
            Rect {
                x: 0,
                y: 13,
                width: 140,
                height: 10,
            },
            0,
            2,
        ));
        // border y=13; session0 line1=14 line2=15; session1 line1=16 line2=17.
        handle_mouse_click(&mut state, 10, 15);
        assert_eq!(
            state.selected_session, 0,
            "line2 of session 0 must select session 0"
        );
        handle_mouse_click(&mut state, 10, 16);
        assert_eq!(
            state.selected_session, 1,
            "line1 of session 1 must select session 1"
        );
    }

    fn mk_search_result() -> crate::search::SearchResult {
        crate::search::SearchResult {
            day_idx: 0,
            session_idx: 0,
            snippet: None,
            match_type: crate::search::SearchMatchType::Content,
            session_path: None,
            message_date: None,
        }
    }

    #[test]
    fn a_search_result_double_click_previews_it_like_enter() {
        let mut state = daily_state_for_click();
        state.tab = Tab::Insights;
        state.search_mode = true;
        state.search_input.set("proj".to_string());
        state.search_results = vec![mk_search_result()];
        handle_double_click(&mut state, 5, 5);
        assert!(state.search_preview_mode, "hit not previewed");
        assert!(state.show_conversation);
        assert!(
            state.search_saved_state.is_some(),
            "Esc has nowhere to return"
        );
    }

    // The results block has NO top border, so result N's first line draws at
    // area.y + N*item_height. A click must select the clicked result, not the
    // previous one.
    #[test]
    fn search_result_click_selects_clicked_row() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let group = make_daily_group(date, vec![make_session("p", None, None)]);
        let mut state = make_test_app_state(vec![group]);
        state.search_mode = true;
        state.search_results = vec![mk_search_result(), mk_search_result(), mk_search_result()];
        state.layout.search_results_area = Some(Rect {
            x: 0,
            y: 6,
            width: 100,
            height: 20,
        });
        // No top border: result0 line1=6; result1 line1=8; result2 line1=10.
        handle_mouse_click(&mut state, 10, 8);
        assert_eq!(state.search_selected, 1, "click on result 1 must select 1");
        handle_mouse_click(&mut state, 10, 6);
        assert_eq!(state.search_selected, 0, "click on result 0 must select 0");
        handle_mouse_click(&mut state, 10, 11);
        assert_eq!(
            state.search_selected, 2,
            "click on result 2 line2 must select 2"
        );
    }

    fn paused_live(id: &str) -> crate::infrastructure::live_sessions::LiveSession {
        use std::path::PathBuf;
        crate::infrastructure::live_sessions::LiveSession {
            session_id: id.to_string(),
            jsonl_path: Some(PathBuf::from(format!("/tmp/{id}.jsonl"))),
            cwd: PathBuf::from("/tmp"),
            name: None,
            status: None,
            pid: 0,
            started_at: None,
            updated_at: None,
            jsonl_mtime: None,
            is_live: false,
            was_recently_live: false,
        }
    }

    // Active now and Recently paused are two separate frames. The paused
    // frame's inner body is `line 0 = info header, rows at 1 + r*3`, and a
    // paused row's global index is `active_count + r`. The info header must
    // not be clickable.
    #[test]
    fn live_paused_frame_click_selects_correct_global_index() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let group = make_daily_group(date, vec![make_session("p", None, None)]);
        let mut state = make_test_app_state(vec![group]);
        state.tab = Tab::Live;
        state.live_active = vec![paused_live("act")]; // active_count == 1
        state.live_paused = vec![paused_live("a"), paused_live("b")];
        // Active frame (inner): line 0 = status, row 0 at body lines 1-3.
        state.layout.live_list_area = Some((
            Rect {
                x: 1,
                y: 1,
                width: 138,
                height: 5,
            },
            0,
            1,
        ));
        // Paused frame (inner) below: body line 0 = info, rows at 1 + r*3.
        state.layout.live_paused_list_area = Some((
            Rect {
                x: 1,
                y: 7,
                width: 138,
                height: 20,
            },
            0,
        ));
        // Active row 0 body line 1 → screen row 2 → global index 0.
        handle_mouse_click(&mut state, 10, 2);
        assert_eq!(state.live_selected, 0, "active row 0 → global index 0");
        // Paused row 0 occupies body lines 1-3 → screen rows 8-10.
        handle_mouse_click(&mut state, 10, 8);
        assert_eq!(
            state.live_selected, 1,
            "paused row 0 → global index active_count + 0 = 1"
        );
        // Paused row 1 occupies body lines 4-6 → screen rows 11-13.
        handle_mouse_click(&mut state, 10, 11);
        assert_eq!(state.live_selected, 2, "paused row 1 → global index 2");
        // Paused info header (body line 0 → screen row 7) selects nothing.
        state.live_selected = 99;
        handle_mouse_click(&mut state, 10, 7);
        assert_eq!(
            state.live_selected, 99,
            "paused info header click must not select a session"
        );
    }

    // Conversation lines render below the 3-line session info header, so a
    // body click must map to the message at (scroll + row - (area.y+1+3)),
    // and clicks on the header rows must not move the selection.
    #[test]
    fn conv_body_click_maps_past_info_header() {
        let mut state = conv_state_for_click();
        // Distinct message per line so an off-by-3 would pick the wrong one.
        state.panes[0].message_lines = vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)];
        // pane y=5, border+info header = 4 rows, so content starts at y=9.
        handle_mouse_click(&mut state, 100, 9);
        assert_eq!(
            state.panes[0].selected_message, 0,
            "first content row selects message 0"
        );
        handle_mouse_click(&mut state, 100, 11);
        assert_eq!(
            state.panes[0].selected_message, 2,
            "third content row selects message 2"
        );
        // A click on the info-header row (y=7) must not move the selection.
        state.panes[0].selected_message = 2;
        handle_mouse_click(&mut state, 100, 7);
        assert_eq!(
            state.panes[0].selected_message, 2,
            "info-header click must not change message selection"
        );
    }

    #[test]
    fn scroll_with_popup_open_never_falls_through_to_the_tab() {
        // Every scrollable popup guard must consume the wheel event; the
        // underlying tab state observing a change means a fall-through.
        let popups: Vec<crate::ActivePopup> = vec![
            crate::ActivePopup::Help { scroll: 0 },
            crate::ActivePopup::InsightsDetail { scroll: 0 },
            crate::ActivePopup::Summary { scroll: 0 },
            crate::ActivePopup::ProjectDetail {
                path: "proj".to_string(),
                scroll: 0,
                cursor: 0,
            },
            crate::ActivePopup::FilterPopup {
                selected: 0,
                input_mode: false,
                input: crate::TextInput::default(),
                input_error: false,
            },
            crate::ActivePopup::ProjectPopup {
                selected: 0,
                scroll: 0,
            },
        ];
        for popup in popups {
            let mut state = make_test_app_state(Vec::new());
            let day_before = state.selected_day;
            let dash_before = state.dashboard_scroll;
            state.active_popup = popup.clone();
            handle_mouse_scroll(&mut state, 5, 5, false);
            assert_eq!(state.selected_day, day_before, "fall-through: {popup:?}");
            assert_eq!(
                state.dashboard_scroll, dash_before,
                "fall-through: {popup:?}"
            );
        }
    }

    #[test]
    fn scroll_down_advances_the_open_popup_scroll() {
        let mut state = make_test_app_state(Vec::new());
        state.active_popup = crate::ActivePopup::Help { scroll: 0 };
        handle_mouse_scroll(&mut state, 5, 5, false);
        assert_eq!(state.help_scroll(), 1);

        state.active_popup = crate::ActivePopup::FilterPopup {
            selected: 0,
            input_mode: false,
            input: crate::TextInput::default(),
            input_error: false,
        };
        handle_mouse_scroll(&mut state, 5, 5, false);
        assert_eq!(state.filter_popup_selected(), 1);
        handle_mouse_scroll(&mut state, 5, 5, true);
        handle_mouse_scroll(&mut state, 5, 5, true);
        assert_eq!(
            state.filter_popup_selected(),
            0,
            "scroll up saturates at the first row"
        );
    }

    #[test]
    fn click_during_filter_input_mode_exits_input_but_keeps_the_popup() {
        // Date-input mode is modal: a click anywhere behaves like Esc (back
        // to the preset list), never selecting rows or closing the popup.
        let mut state = make_test_app_state(Vec::new());
        let mut input = crate::TextInput::default();
        input.set("2026".to_string());
        state.active_popup = crate::ActivePopup::FilterPopup {
            selected: 0,
            input_mode: true,
            input,
            input_error: true,
        };
        handle_mouse_click(&mut state, 5, 5);
        assert!(state.show_filter_popup(), "popup must stay open");
        assert!(!state.filter_input_mode(), "input mode must exit");
        assert!(!state.filter_input_error(), "error must clear");
        assert_eq!(
            state.filter_input().map(|i| i.text.as_str()),
            Some(""),
            "typed date must clear on exit"
        );
    }
}
