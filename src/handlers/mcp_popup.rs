//! MCP detail-popup helpers — server enumeration, cursor row math, scroll
//! clamping. Used by the keyboard handler (cursor nav) and the popup renderer
//! (rendered server-list order).

use crate::AppState;
use crate::aggregator::{ToolCategory, classify_tool};

/// Synthetic group name for built-in tools when they are rendered alongside MCP
/// servers in the Tools detail popup (active==0). Treated as a single
/// expandable group with the same row layout as an MCP server.
pub(crate) const BUILTIN_GROUP_NAME: &str = "Built-in";

/// Group names in Tools detail (Built-in + logged MCP servers + stale-never).
/// Order MUST match `detail_panel_ecosystem` in src/ui/dashboard.rs — both
/// pin Built-in first, then sort by `dashboard_ecosystem_sort` (last_used
/// desc or call desc), name as tiebreak. Divergence selects the wrong row.
pub(crate) fn collect_mcp_servers(state: &AppState) -> Vec<String> {
    use std::collections::HashMap;
    // (calls, last_used) per server. last_used comes from mcp_status, matching
    // the renderer (servers absent from mcp_status have no recency signal).
    let mut per_server: HashMap<String, (usize, Option<chrono::DateTime<chrono::Utc>>)> =
        HashMap::new();
    let mut builtin_total: usize = 0;
    for (key, count) in &state.stats.tool_usage {
        match classify_tool(key) {
            ToolCategory::Mcp { server } => {
                per_server.entry(server).or_insert((0, None)).0 += count;
            }
            ToolCategory::BuiltIn => {
                builtin_total += count;
            }
            _ => {}
        }
    }
    // Attach `last_used` from mcp_status to BOTH used and configured-but-unused
    // servers. The renderer in `detail_panel_ecosystem` does the same — gating
    // this update on `status.configured` desynced the recency sort for servers
    // that were used historically but are no longer in the active config
    // (cursor would land on a different row than the visual highlight).
    for status in &state.mcp_status {
        if status.configured {
            // Stale-never servers absent from tool_usage still need a row.
            per_server.entry(status.name.clone()).or_insert((0, None));
        }
        if let Some(entry) = per_server.get_mut(&status.name) {
            entry.1 = status.last_used;
        }
    }
    let by_recency = state.dashboard_ecosystem_sort == crate::state::RankSort::Recency;
    let mut rows: Vec<(String, usize, Option<chrono::DateTime<chrono::Utc>>)> = per_server
        .into_iter()
        .map(|(n, (c, lu))| (n, c, lu))
        .collect();
    rows.sort_by(|a, b| {
        if by_recency {
            b.2.cmp(&a.2)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        } else {
            b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0))
        }
    });
    let mut names: Vec<String> = rows.into_iter().map(|(n, ..)| n).collect();
    // Built-in is pinned first (synthetic aggregate; the renderer does the
    // same via `is_builtin` so the "MCP servers" divider sits after it).
    if builtin_total > 0 {
        names.insert(0, BUILTIN_GROUP_NAME.to_string());
    }
    names
}

/// Number of distinct tools attributed to the given group in `tool_usage`.
/// For the synthetic Built-in group, counts every BuiltIn-classified tool.
pub(crate) fn mcp_tool_count(state: &AppState, server: &str) -> usize {
    if server == BUILTIN_GROUP_NAME {
        return state
            .stats
            .tool_usage
            .keys()
            .filter(|k| matches!(classify_tool(k), ToolCategory::BuiltIn))
            .count();
    }
    state
        .stats
        .tool_usage
        .keys()
        .filter(|k| matches!(classify_tool(k), ToolCategory::Mcp { server: ref s } if s == server))
        .count()
}

/// Row span (1 header row + optional expanded tool rows) of a given server in the MCP
/// tab rendering. Used to keep the cursor visible while navigating with j/k.
fn row_span_of_server(state: &AppState, server: &str) -> usize {
    if !state.mcp_expanded_servers.contains(server) {
        return 1;
    }
    1 + mcp_tool_count(state, server)
}

/// Logical row offset (0-origin) of the current MCP cursor within the tab body, counting
/// every server header + each expanded tool row above it.
fn mcp_cursor_row(state: &AppState, servers: &[String]) -> usize {
    let mut row = 0usize;
    for (i, server) in servers.iter().enumerate() {
        if i == state.mcp_selected_server {
            if let Some(t) = state.mcp_selected_tool {
                row += 1 + t;
            }
            return row;
        }
        row += row_span_of_server(state, server);
        // The renderer's "── MCP servers ──" divider follows the Built-in group.
        if server == BUILTIN_GROUP_NAME && i + 1 < servers.len() {
            row += 1;
        }
    }
    row
}

/// Whether the Tools tab shows its "Built-in N · MCP M" ratio line: only when
/// both sides have calls. The renderer, the row count and the cursor all ask.
pub(crate) fn tools_ratio_shown(state: &AppState) -> bool {
    let (mut builtin, mut mcp) = (0usize, 0usize);
    for (key, count) in &state.stats.tool_usage {
        match classify_tool(key) {
            ToolCategory::BuiltIn => builtin += count,
            ToolCategory::Mcp { .. } => mcp += count,
            _ => {}
        }
    }
    builtin > 0 && mcp > 0
}

/// Pre-group header row count in Tools tab body — must mirror
/// `draw_dashboard_detail_popup`'s active==0 branch (summary, optional
/// ratio line, optional stale legend). Coord-unity: `dashboard_scroll[crate::DashPanel::Ecosystem]` and
/// `mcp_cursor_row(...) + mcp_pre_server_offset(...)` MUST share "row 0";
/// drift causes off-by-N invisibility of the cursor.
fn mcp_pre_server_offset(state: &AppState) -> usize {
    // Delegate the "stale" definition to `McpServerStatus::is_underutilized`
    // — the canonical source-of-truth — so this offset, the legend, the
    // per-row ⚠ marker, and the Tier 3 alert all share one threshold.
    // Reimplementing the check inline (e.g. `> 30`) drifts off-by-one
    // against the legend (`>= 30`) and produces mismatched displays.
    let stale_at = state.stale_reference();
    let any_stale = state
        .mcp_status
        .iter()
        .any(|s| s.is_underutilized(stale_at, 30));
    1 + usize::from(tools_ratio_shown(state)) + usize::from(any_stale)
}

/// Clamp `dashboard_scroll[crate::DashPanel::Ecosystem]` so the MCP cursor stays in the visible
/// body slice (popup inner − 4 pinned header rows). Scroll and cursor
/// share row-0 = summary; servers begin at `pre_server_offset`.
pub(crate) fn adjust_mcp_scroll(state: &mut AppState, servers: &[String]) {
    // The summary (and optional legend) lives in body[0..] and scrolls with the rest.
    const HEADER_ROWS: usize = crate::ui::dashboard::ECOSYSTEM_HEADER_ROWS;
    let visible_body = state.layout.active_popup_area.map_or(20, |a| {
        (a.height as usize).saturating_sub(2 + HEADER_ROWS).max(1)
    });
    // mcp_cursor_row returns server-relative (server 0 = row 0); add the pre-server
    // offset (summary + optional legend) so body_cursor matches the rendered slice.
    let body_cursor = mcp_cursor_row(state, servers) + mcp_pre_server_offset(state);
    let scroll = &mut state.dashboard_scroll[crate::DashPanel::Ecosystem];
    *scroll = crate::ui::follow_row(*scroll, body_cursor, 1, visible_body, false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::McpServerStatus;
    use chrono::{Duration, Utc};

    // The MCP cursor stays inside the visible body: moving past either edge
    // scrolls just far enough, moving within it leaves the scroll alone.
    #[test]
    fn the_mcp_scroll_keeps_the_cursor_in_the_visible_body() {
        let mut state = crate::test_helpers::helpers::make_test_app_state(Vec::new());
        for i in 0..30 {
            state
                .stats
                .tool_usage
                .insert(format!("mcp__server{i:02}__do"), 1);
        }
        // Inner height 12 = 10 body rows; 4 of them are pinned headers.
        state.layout.active_popup_area = Some(ratatui::layout::Rect::new(0, 0, 80, 12));
        let servers = crate::collect_mcp_servers(&state);
        let visible = 12 - 2 - 4;
        let body =
            |state: &AppState| mcp_cursor_row(state, &servers) + mcp_pre_server_offset(state);
        let scroll = |state: &AppState| state.dashboard_scroll[crate::DashPanel::Ecosystem];

        state.mcp_selected_server = 20;
        adjust_mcp_scroll(&mut state, &servers);
        assert_eq!(scroll(&state), body(&state) + 1 - visible, "bottom edge");
        let held = scroll(&state);
        state.mcp_selected_server = 19;
        adjust_mcp_scroll(&mut state, &servers);
        assert_eq!(scroll(&state), held, "inside the view: no scroll");
        state.mcp_selected_server = 0;
        adjust_mcp_scroll(&mut state, &servers);
        assert_eq!(scroll(&state), body(&state), "top edge");
    }

    fn make_state() -> AppState {
        crate::test_helpers::helpers::make_test_app_state(Vec::new())
    }

    #[test]
    fn collect_orders_recency_for_used_but_unconfigured_server() {
        // Used-but-unconfigured servers must inherit last_used so the
        // cursor list and the renderer agree on order — otherwise the
        // visual highlight and Enter target diverge.
        let mut state = make_state();
        state.dashboard_ecosystem_sort = crate::state::RankSort::Recency;
        state
            .stats
            .tool_usage
            .insert("mcp__server1__action".to_string(), 1);
        state
            .stats
            .tool_usage
            .insert("mcp__server2__action".to_string(), 1);
        let now = Utc::now();
        state.mcp_status = vec![
            McpServerStatus {
                name: "server1".to_string(),
                configured: true,
                last_used: Some(now - Duration::hours(48)),
                total_calls: 1,
            },
            McpServerStatus {
                name: "server2".to_string(),
                configured: false,
                last_used: Some(now - Duration::minutes(5)),
                total_calls: 1,
            },
        ];
        let order = collect_mcp_servers(&state);
        let recent_idx = order
            .iter()
            .position(|n| n == "server2")
            .expect("server2 present");
        let older_idx = order
            .iter()
            .position(|n| n == "server1")
            .expect("server1 present");
        assert!(
            recent_idx < older_idx,
            "more-recent unconfigured server must sort above an older configured one — \
             got order {order:?}"
        );
    }
}
