use std::collections::HashMap;

use chrono::{Local, NaiveDate};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::PeriodFilter;
use crate::aggregator::{CostCalculator, DailyGroup, DailyGrouper};
use crate::conversation::{ConversationBlock, ConversationMessage, load_conversation};
use crate::infrastructure::FileDiscovery;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CcsightServer {
    limit: usize,
    fixed_groups: Option<Vec<DailyGroup>>,
    /// Reload-on-change memoization: every tool call recomputes a cheap
    /// on-disk signature (per-file mtime + size, same discovery as the
    /// loader) and reuses the parsed data only while it matches. The server
    /// lives as long as the MCP client session, so a once-only or TTL cache
    /// would serve stale totals for hours. `Arc`: rmcp clones the server.
    cached: std::sync::Arc<std::sync::Mutex<Option<(u64, std::sync::Arc<LoadedData>)>>>,
}

#[derive(Debug)]
struct LoadedData {
    daily_groups: Vec<DailyGroup>,
    summary_cache: HashMap<PathBuf, Option<String>>,
}

fn load_fresh_data(limit: usize) -> LoadedData {
    let files = FileDiscovery::find_jsonl_files_with_limit(limit).unwrap_or_default();
    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    build_loaded_data(daily_groups)
}

fn build_loaded_data(daily_groups: Vec<DailyGroup>) -> LoadedData {
    let mut summary_cache = HashMap::new();

    for group in &daily_groups {
        for session in &group.sessions {
            let path = &session.file_path;
            if session.summary.is_none() && !summary_cache.contains_key(path) {
                summary_cache.insert(path.clone(), derive_summary(path));
            }
        }
    }

    LoadedData {
        daily_groups,
        summary_cache,
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StatsParams {
    #[schemars(
        description = "Period filter: \"today\", \"week\", \"month\", \"all\" (default). Ignored when date_from is set."
    )]
    period: Option<String>,
    #[schemars(
        description = "Start date for range filter (YYYY-MM-DD). Takes priority over period."
    )]
    date_from: Option<String>,
    #[schemars(
        description = "End date for range filter (YYYY-MM-DD). Defaults to today if omitted."
    )]
    date_to: Option<String>,
    #[schemars(
        description = "Group results by: \"day\" (returns daily breakdown). Omit for totals only. The per-project list is returned only with compact=false."
    )]
    group_by: Option<String>,
    #[schemars(
        description = "Defaults to true: the per-entity lists (mcp_servers, projects, languages) are dropped and the totals, model breakdown, hourly pattern and top tools kept — those three lists are the bulk of the response. Set false to include them."
    )]
    compact: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchParams {
    #[schemars(
        description = "Search query. Searches across all session conversations (full-text) and metadata (project name, summary, branch, date)."
    )]
    query: String,
    #[schemars(description = "Filter by project name (partial match)")]
    project: Option<String>,
    #[schemars(description = "Start date for range filter (YYYY-MM-DD)")]
    date_from: Option<String>,
    #[schemars(description = "End date for range filter (YYYY-MM-DD). Omit for no upper limit.")]
    date_to: Option<String>,
    #[schemars(description = "Maximum number of results (default: 20)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct LiveSessionsParams {
    #[schemars(
        description = "Filter by project name / cwd substring (case-sensitive). Omit for all projects."
    )]
    project: Option<String>,
    #[schemars(
        description = "Filter by status tier: \"busy\" (process actively responding), \"today\" (alive, touched today in local calendar), \"older\" (alive but not touched today), \"paused\" (process gone, JSONL within 24h or in snapshot). Omit for all."
    )]
    status: Option<String>,
    #[schemars(
        description = "When true, return only sessions needing human intervention (state != \"active\"): awaiting_input / stalled / error / exited. Default false (all)."
    )]
    actionable_only: Option<bool>,
    #[schemars(
        description = "When true, drop heavy fields (ai_title, last_user_message, tokens, cost, model) and return a minimal row {session_id, project, cwd, status, state, age_secs, reason} — keeps the caller's context small. Default false."
    )]
    compact: Option<bool>,
    #[schemars(
        description = "Idle seconds (since last JSONL write) before a finished/owed-output session counts as awaiting_input / stalled. Default 600 (10 min)."
    )]
    idle_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SessionsParams {
    #[schemars(
        description = "Session ID (first 8 chars) to get detail. When set, returns conversation, tool usage, and files touched."
    )]
    session_id: Option<String>,
    #[schemars(
        description = "Filter by keyword (matches project, summary, branch, date metadata only). For full-text content search, use 'search' tool."
    )]
    query: Option<String>,
    #[schemars(description = "Filter by project name (partial match)")]
    project: Option<String>,
    #[schemars(
        description = "Filter by exact date (YYYY-MM-DD). For ranges, use date_from/date_to instead."
    )]
    date: Option<String>,
    #[schemars(description = "Start date for range filter (YYYY-MM-DD)")]
    date_from: Option<String>,
    #[schemars(description = "End date for range filter (YYYY-MM-DD). Defaults to today.")]
    date_to: Option<String>,
    #[schemars(
        description = "Sort order: \"date\" (default), \"cost\", \"tokens\". \"cost\" ranks by an API-equivalent estimate (tokens × list price), NOT actual subscription spend."
    )]
    sort: Option<String>,
    #[schemars(description = "Maximum number of results (default: 20)")]
    limit: Option<usize>,
    #[schemars(
        description = "Max conversation entries in session_detail. Defaults to a bounded head+tail window with an omission marker naming the offset to resume from; raise it to widen, or set 0 to omit the conversation entirely."
    )]
    conversation_limit: Option<usize>,
    #[schemars(
        description = "Skip first N conversation entries (for pagination). Use with conversation_limit."
    )]
    conversation_offset: Option<usize>,
    #[schemars(
        description = "Search within conversation. Returns only matching messages with 1 message of context before/after. Each message includes its offset. More useful than conversation_offset for finding specific content."
    )]
    conversation_query: Option<String>,
    #[schemars(
        description = "Return each conversation message's whole text instead of a short preview (with conversation_query: instead of a window around the match). Pair with conversation_offset + conversation_limit=1 to read one long message in full."
    )]
    full_text: Option<bool>,
    #[schemars(description = "Filter to pinned sessions only")]
    pinned: Option<bool>,
}

fn period_to_filter(period: Option<&str>) -> PeriodFilter {
    match period {
        Some("today") => PeriodFilter::Today,
        Some("week") => PeriodFilter::Last7d,
        Some("month") => PeriodFilter::Last30d,
        _ => PeriodFilter::All,
    }
}

/// Shape one `live_sessions` row. `state` and `reason` are always
/// present; `reason` is omitted when empty (an `active` session). `total` is the
/// session's lifetime (cost, work_tokens); `meta` is its newest-day metadata
/// (model / titles). Compact mode drops both so the caller's context stays small.
#[allow(clippy::too_many_arguments)]
fn build_live_row(
    s: &crate::infrastructure::live_sessions::LiveSession,
    tier: &str,
    age_secs: Option<i64>,
    state: crate::session_state::SessionState,
    reason: &str,
    compact: bool,
    total: Option<(f64, u64, bool)>,
    meta: Option<&crate::aggregator::SessionInfo>,
) -> serde_json::Value {
    let project = s
        .cwd
        .file_name()
        .and_then(|n| n.to_str())
        .map_or_else(|| s.cwd.display().to_string(), str::to_string);
    let mut row = if compact {
        serde_json::json!({
            "session_id": s.session_id,
            "cwd": s.cwd.display().to_string(),
            "project": project,
            "status": tier,
            "state": state.as_str(),
            "age_secs": age_secs,
        })
    } else {
        let (cost, tokens, _) = total.unwrap_or((0.0, 0, false));
        let model_normalized = meta
            .and_then(|i| i.model.as_ref())
            .map(|m| crate::aggregator::normalize_model_name(m));
        serde_json::json!({
            "session_id": s.session_id,
            "cwd": s.cwd.display().to_string(),
            "project": project,
            "status": tier,
            "state": state.as_str(),
            "age_secs": age_secs,
            "pid": s.pid,
            "was_recently_live": s.was_recently_live,
            "tokens": tokens,
            "cost": cost,
            "model": model_normalized,
            "ai_title": meta.and_then(|i| i.ai_title.clone()),
            "last_user_message": meta.and_then(|i| i.last_user_message.clone()),
        })
    };
    if !reason.is_empty() {
        row["reason"] = serde_json::json!(reason);
    }
    // A contributing day used a model without pricing, so `cost` is a lower
    // bound. The TUI marks this with `$N*`; emit a flag so MCP callers aren't
    // misled by an understated total. Omitted (not false) when fully priced,
    // and absent in compact mode where `cost` itself is dropped.
    if !compact && total.is_some_and(|(_, _, unpriced)| unpriced) {
        row["cost_is_lower_bound"] = serde_json::json!(true);
    }
    row
}

fn filter_groups_by_date_range(
    groups: &[DailyGroup],
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Vec<&DailyGroup> {
    groups
        .iter()
        .filter(|g| {
            if let Some(s) = start
                && g.date < s
            {
                return false;
            }
            if let Some(e) = end
                && g.date > e
            {
                return false;
            }
            true
        })
        .collect()
}

#[tool_router]
impl CcsightServer {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            fixed_groups: None,
            cached: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[cfg(test)]
    fn from_groups(daily_groups: Vec<DailyGroup>) -> Self {
        Self {
            limit: 0,
            fixed_groups: Some(daily_groups),
            cached: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Fold (mtime, size) of every discovered session JSONL into one value.
    /// Appends bump mtime/size and created/deleted files change the fold, so
    /// any on-disk change invalidates the memoized load on the next call.
    fn disk_signature(&self) -> u64 {
        if self.fixed_groups.is_some() {
            return 0;
        }
        let files = FileDiscovery::find_jsonl_files_with_limit(self.limit).unwrap_or_default();
        let mut sig: u64 = files.len() as u64;
        for f in &files {
            if let Ok(meta) = std::fs::metadata(f) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                sig = sig
                    .wrapping_mul(31)
                    .wrapping_add(mtime)
                    .wrapping_mul(31)
                    .wrapping_add(meta.len());
            }
        }
        sig
    }

    fn load(&self) -> std::sync::Arc<LoadedData> {
        let sig = self.disk_signature();
        let mut guard = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((cached_sig, data)) = guard.as_ref()
            && *cached_sig == sig
        {
            return data.clone();
        }
        let data = std::sync::Arc::new(match &self.fixed_groups {
            Some(groups) => build_loaded_data(groups.clone()),
            None => load_fresh_data(self.limit),
        });
        *guard = Some((sig, data.clone()));
        data
    }

    #[tool(
        description = "Get aggregated usage statistics: cost, tokens (input/output/cache), model breakdown, projects, hourly patterns, tool usage, languages. Use group_by=day for daily breakdown. All dates/times are in local timezone. Cost (all `*_cost`/`cost_usd` fields) is an API-equivalent estimate (tokens × published list price), NOT actual subscription/plan spend — a Pro/Max subscriber's real outlay differs."
    )]
    fn stats(
        &self,
        Parameters(params): Parameters<StatsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let (start, end) = if let Some(ref sd) = params.date_from {
            let s = sd.parse::<NaiveDate>().ok();
            let e = params
                .date_to
                .as_deref()
                .and_then(|d| d.parse::<NaiveDate>().ok());
            (s, e)
        } else {
            let filter = period_to_filter(params.period.as_deref());
            filter.date_range()
        };
        let data = self.load();
        let filtered = filter_groups_by_date_range(&data.daily_groups, start, end);

        let calculator = CostCalculator::global();
        let mut total_cost = 0.0;
        let mut total_input = 0u64;
        let mut total_output = 0u64;
        // Dedup by file_path: `filtered` holds one entry per (session, active
        // day), so counting per entry would report session-days. Distinct paths
        // give the true session count (matches search / Daily list).
        let mut session_paths: std::collections::HashSet<&std::path::PathBuf> =
            std::collections::HashSet::new();
        // Separate from `session_paths`: project counts include subagents
        // (matching the TUI's ProjectStats), the deduped total excludes them.
        let mut project_files: std::collections::HashSet<&std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut model_agg: HashMap<String, (f64, u64)> = HashMap::new();
        let mut project_agg: HashMap<String, (usize, u64, f64)> = HashMap::new();
        let mut hourly_agg: HashMap<u8, u64> = HashMap::new();
        let group_by = params.group_by.as_deref();
        let mut daily_agg: HashMap<NaiveDate, (f64, usize, u64)> = HashMap::new();
        let mut total_cache_creation = 0u64;
        let mut total_cache_read = 0u64;
        let mut tool_counts: HashMap<String, usize> = HashMap::new();
        let mut lang_counts: HashMap<String, usize> = HashMap::new();

        // Track models encountered in the filtered window that lack a pricing entry,
        // so the stats response can surface the silent-$0 risk to the caller.
        let mut models_without_pricing: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for group in &filtered {
            for session in &group.sessions {
                let mut session_cost = 0.0;
                for (model, tokens) in &session.day_tokens_by_model {
                    let cost = calculator
                        .calculate_cost(tokens, Some(model))
                        .unwrap_or(0.0);
                    if !calculator.has_pricing(Some(model)) {
                        models_without_pricing
                            .insert(crate::aggregator::normalize_model_name(model));
                    }
                    session_cost += cost;
                    total_cost += cost;
                    total_cache_creation += tokens.cache_creation_tokens;
                    total_cache_read += tokens.cache_read_tokens;
                    let entry = model_agg
                        .entry(crate::aggregator::normalize_model_name(model))
                        .or_default();
                    entry.0 += cost;
                    entry.1 += tokens.work_tokens();
                }

                total_input += session.day_input_tokens;
                total_output += session.day_output_tokens;

                // Subagent-inclusive (matches total_cost_usd / --daily / TUI
                // project_stats). Subagent skip below gates only session_count
                // and hourly / tool / language tallies.
                if group_by == Some("day") {
                    let day = daily_agg.entry(group.date).or_default();
                    day.0 += session_cost;
                    day.1 += 1;
                    day.2 += session.work_tokens();
                }

                let proj = project_agg.entry(session.project_name.clone()).or_default();
                // A multi-day session appears once per active day; count the
                // FILE once so `sum(projects[].sessions)` reconciles with the
                // deduped `total_sessions`. Tokens/cost are per-day slices and
                // still sum across every appearance.
                if project_files.insert(&session.file_path) {
                    proj.0 += 1;
                }
                proj.1 += session.work_tokens();
                proj.2 += session_cost;

                if session.is_subagent {
                    continue;
                }

                session_paths.insert(&session.file_path);

                for (hour, tokens) in &session.day_hourly_work_tokens {
                    *hourly_agg.entry(*hour).or_default() += tokens;
                }

                for (tool, count) in &session.day_tool_usage {
                    *tool_counts.entry(tool.clone()).or_default() += count;
                }
                for (lang, count) in &session.day_language_usage {
                    *lang_counts.entry(lang.clone()).or_default() += count;
                }
            }
        }

        let mut model_breakdown: Vec<serde_json::Value> = model_agg
            .into_iter()
            .map(|(model, (cost, tokens))| {
                serde_json::json!({
                    "model": model,
                    "cost_usd": round_cents(cost),
                    "tokens": tokens,
                })
            })
            .collect();
        model_breakdown.sort_by(|a, b| {
            b["cost_usd"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["cost_usd"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut projects: Vec<serde_json::Value> = project_agg
            .into_iter()
            .map(|(project, (sessions, work_tokens, cost))| {
                serde_json::json!({
                    "project": project,
                    "sessions": sessions,
                    "work_tokens": work_tokens,
                    "cost_usd": round_cents(cost),
                })
            })
            .collect();
        projects.sort_by(|a, b| {
            b["cost_usd"]
                .as_f64()
                .unwrap_or(0.0)
                .partial_cmp(&a["cost_usd"].as_f64().unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut hourly_pattern: HashMap<String, u64> = HashMap::new();
        for (hour, tokens) in &hourly_agg {
            hourly_pattern.insert(hour.to_string(), *tokens);
        }

        let today = Local::now().date_naive();
        let period_label = if params.date_from.is_some() {
            "custom"
        } else {
            params.period.as_deref().unwrap_or("all")
        };
        let date_range = match (start, end) {
            (Some(s), Some(e)) => serde_json::json!({"start": s.to_string(), "end": e.to_string()}),
            (Some(s), None) => {
                serde_json::json!({"start": s.to_string(), "end": today.to_string()})
            }
            _ => serde_json::json!(null),
        };

        let cache_hit_rate = if total_input + total_cache_read > 0 {
            (total_cache_read as f64 / (total_input + total_cache_read) as f64 * 1000.0).round()
                / 10.0
        } else {
            0.0
        };

        let total_tool_calls: usize = tool_counts.values().sum();
        let mut top_tools: Vec<serde_json::Value> = tool_counts
            .into_iter()
            .map(|(tool, count)| serde_json::json!({"tool": tool, "count": count}))
            .collect();
        top_tools.sort_by(|a, b| {
            b["count"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["count"].as_u64().unwrap_or(0))
        });
        top_tools.truncate(10);

        let mut languages: Vec<serde_json::Value> = lang_counts
            .into_iter()
            .map(|(lang, count)| serde_json::json!({"language": lang, "count": count}))
            .collect();
        languages.sort_by(|a, b| {
            b["count"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["count"].as_u64().unwrap_or(0))
        });

        // Surface models that lack a pricing entry. Without this hint, consumers of the
        // stats tool would see a `total_cost_usd` that silently excludes any spend on
        // unknown models (e.g., a newly released model before `pricing.rs` is updated).
        let pricing_gap_json = if models_without_pricing.is_empty() {
            serde_json::Value::Null
        } else {
            let mut names: Vec<&String> = models_without_pricing.iter().collect();
            names.sort();
            serde_json::json!({
                "count": names.len(),
                "models": names,
                "note": "total_cost_usd excludes spend on these models because pricing is not yet defined",
            })
        };

        // Per-MCP-server adoption snapshot: configured, last_used, total_calls,
        // status (active / stale-idle / stale-never / inactive). Mirrors what
        // the Tools popup shows. Useful for users asking "which servers are
        // I paying mental tax on but never invoking" via the MCP API.
        let mcp_status_json = {
            let now = chrono::Utc::now();
            let owned: Vec<crate::aggregator::DailyGroup> =
                filtered.iter().map(|g| (*g).clone()).collect();
            let mut entries: Vec<serde_json::Value> =
                crate::infrastructure::compute_mcp_status(&owned)
                    .into_iter()
                    .map(|s| {
                        // `is_underutilized(now, 30)` is the canonical "stale"
                        // predicate; reusing it here keeps the MCP API field
                        // in lockstep with the TUI legend / popup ⚠ marker.
                        let status = if !s.configured {
                            "inactive"
                        } else if s.last_used.is_none() {
                            "stale-never"
                        } else if s.is_underutilized(now, 30) {
                            "stale-idle"
                        } else {
                            "active"
                        };
                        serde_json::json!({
                            "name": s.name,
                            "configured": s.configured,
                            "last_used": s.last_used.map(|t| t.to_rfc3339()),
                            "total_calls": s.total_calls,
                            "status": status,
                        })
                    })
                    .collect();
            entries.sort_by(|a, b| {
                a["status"]
                    .as_str()
                    .cmp(&b["status"].as_str())
                    .then_with(|| {
                        b["total_calls"]
                            .as_u64()
                            .unwrap_or(0)
                            .cmp(&a["total_calls"].as_u64().unwrap_or(0))
                    })
                    .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
            });
            entries
        };

        let mut result = serde_json::json!({
            "period": period_label,
            "date_range": date_range,
            "total_cost_usd": round_cents(total_cost),
            "pricing_gap": pricing_gap_json,
            "mcp_servers": mcp_status_json,
            "total_sessions": session_paths.len(),
            "tokens": {
                "input": total_input,
                "output": total_output,
                "cache_creation": total_cache_creation,
                "cache_read": total_cache_read,
                "total_work": total_input + total_output,
            },
            "cache_hit_rate_pct": cache_hit_rate,
            "model_breakdown": model_breakdown,
            "projects": projects,
            "hourly_pattern": hourly_pattern,
            "tool_calls": total_tool_calls,
            "top_tools": top_tools,
            "languages": languages,
        });

        // The three per-entity lists dominate the payload, and a caller asking
        // for totals pays for them in context it cannot use.
        if params.compact.unwrap_or(true)
            && let Some(map) = result.as_object_mut()
        {
            for key in ["mcp_servers", "projects", "languages"] {
                map.remove(key);
            }
        }

        if group_by == Some("day") {
            let mut daily: Vec<serde_json::Value> = daily_agg
                .into_iter()
                .map(|(date, (cost, sessions, tokens))| {
                    serde_json::json!({
                        "date": date.to_string(),
                        "cost_usd": round_cents(cost),
                        "sessions": sessions,
                        "work_tokens": tokens,
                    })
                })
                .collect();
            daily.sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
            result["daily"] = serde_json::json!(daily);
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&result).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Full-text search over session conversations plus metadata (project, summary, branch, date). One row per session with a snippet and session_id; to read the matching messages, pass that id to `sessions` with the same term as conversation_query. `content_index` says whether message text was searched. Figures are per day as in `sessions`, with `session_total` for a multi-day session. All dates/times local."
    )]
    fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let data = self.load();
        let limit = params.limit.unwrap_or(20);
        let query = &params.query;

        // Floored, not just scaled: the index returns one hit per matching
        // message, and a session that mentions the term throughout collapses
        // dozens of hits into one result — so a small `limit` would starve on
        // its own over-fetch while other sessions never get looked at.
        let fetch_limit =
            if params.date_from.is_some() || params.date_to.is_some() || params.project.is_some() {
                // Those filters run after the index answers, so a narrow one
                // would otherwise starve on the unfiltered fetch.
                FILTERED_INDEX_FETCH
            } else {
                (limit * 5).max(MIN_INDEX_FETCH)
            };
        // MCP server has no UI live-session state; the `filter:live` /
        // `filter:busy` / `filter:paused` predicates resolve to "nothing
        // matches" via empty path sets. Period and property filters still
        // work since they don't need any live context.
        let empty_paths = std::collections::HashSet::new();
        let ctx = crate::search::SearchFiltersContext {
            today: chrono::Local::now().date_naive(),
            live_paths: &empty_paths,
            busy_paths: &empty_paths,
            paused_paths: &empty_paths,
        };
        let mut meta_results = crate::search::perform_search(&data.daily_groups, query, &ctx);

        let date_from = params
            .date_from
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let date_to = params
            .date_to
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let in_range =
            |d: NaiveDate| date_from.is_none_or(|f| d >= f) && date_to.is_none_or(|t| d <= t);
        let day_in_range = |day_idx: usize| {
            data.daily_groups
                .get(day_idx)
                .is_some_and(|g| in_range(g.date))
        };

        // `filter:` / `project:` tokens would match literally in the content
        // index, so only the free text goes to it (perform_search above already
        // applied the filters). The index is only read here; building or
        // catching it up happens off the request, since either can take longer
        // than a caller's tool timeout.
        let (_, free_text) = crate::search::parse_search_query(query);
        // The on-disk index describes the on-disk corpus. A server handed
        // fixed groups must neither read it (its hits belong to other
        // sessions) nor refresh it (that rewrites it to the fixed set).
        let use_index = !free_text.is_empty() && self.fixed_groups.is_none();
        let index = use_index
            .then(|| crate::infrastructure::SearchIndex::open_for_read(&data.daily_groups))
            .flatten();
        // A missing index is built at once; an out-of-date one waits out the
        // cool-down, since a session being worked in is written continuously
        // and every call would otherwise re-parse it in full.
        let refreshing = use_index
            && match index {
                Some((_, 0)) => false,
                Some(_) => catch_up_cooled_down() && spawn_index_refresh(&data),
                None => spawn_index_refresh(&data),
            };
        if let Some((index, _)) = &index {
            // Index hits carry the positions captured when they were indexed,
            // which drift as sessions and days are added; the stored path is
            // the stable key.
            let position_of = positions_by_path(&data.daily_groups);
            // Resolve a result to its session file_path. Used to dedupe a
            // multi-day session, which lives in one DailyGroup per active day
            // with the same path — otherwise it returns once per day and
            // starves other matches up to `limit`.
            let path_of = |r: &crate::search::SearchResult| -> Option<std::path::PathBuf> {
                data.daily_groups.get(r.day_idx).and_then(|g| {
                    g.sessions
                        .iter()
                        .filter(|s| !s.is_subagent)
                        .nth(r.session_idx)
                        .map(|s| s.file_path.clone())
                })
            };
            // Only in-range metadata hits may claim a session: one outside
            // the range is dropped below and would take the session's content
            // hit with it.
            let mut seen: std::collections::HashSet<std::path::PathBuf> = meta_results
                .iter()
                .filter(|r| day_in_range(r.day_idx))
                .filter_map(path_of)
                .collect();
            for mut result in index.search(&free_text, fetch_limit, 200) {
                if let Some(stored) = result.session_path.as_deref() {
                    // The newest active day inside the date range: a session
                    // matches a range it was active in, not only its last day.
                    // None — gone since indexing, or never active in range.
                    let Some((d, i)) = position_of
                        .get(std::path::Path::new(stored))
                        .and_then(|days| newest_in_range(days, day_in_range))
                    else {
                        continue;
                    };
                    result.day_idx = d;
                    result.session_idx = i;
                }
                match path_of(&result) {
                    Some(p) => {
                        if seen.insert(p) {
                            meta_results.push(result);
                        }
                    }
                    None => meta_results.push(result),
                }
            }
        }

        let calculator = CostCalculator::global();
        let totals = crate::aggregator::cumulative_by_path(&data.daily_groups);
        let mut results_json: Vec<serde_json::Value> = Vec::new();
        // Counted past `limit` so the caller can tell a narrow page from an
        // exhausted one. The walk continues but the per-result JSON, which is
        // what costs anything here, stops at the limit.
        let mut matched_total = 0usize;

        for result in &meta_results {
            let Some(group) = data.daily_groups.get(result.day_idx) else {
                continue;
            };
            if let Some(from) = date_from
                && group.date < from
            {
                continue;
            }
            if let Some(to) = date_to
                && group.date > to
            {
                continue;
            }
            let Some(session) = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .nth(result.session_idx)
            else {
                continue;
            };
            if let Some(ref proj) = params.project
                && !session
                    .project_name
                    .to_lowercase()
                    .contains(&proj.to_lowercase())
            {
                continue;
            };

            matched_total += 1;
            if results_json.len() >= limit {
                continue;
            }

            let cost: f64 = session.cost(calculator);

            let session_id: String = session
                .file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .chars()
                .take(8)
                .collect();

            let match_type = match result.match_type {
                crate::search::SearchMatchType::ProjectName => "project",
                crate::search::SearchMatchType::Summary => "summary",
                crate::search::SearchMatchType::GitBranch => "branch",
                crate::search::SearchMatchType::SessionId => "session_id",
                crate::search::SearchMatchType::Date => "date",
                crate::search::SearchMatchType::Content => "content",
            };

            let total = totals.get(session.file_path.as_path());
            let titled = total.map_or(session, |(_, newest)| *newest);
            let mut row = serde_json::json!({
                "date": group.date.to_string(),
                "project": session.project_name,
                "summary": titled.display_title(),
                "snippet": result.snippet,
                "match_type": match_type,
                "tokens": session.work_tokens(),
                "cost_usd": round_cents(cost),
                "session_id": session_id,
                "git_branch": session.git_branch,
            });
            if let Some(t) = total.and_then(session_total_json) {
                row["session_total"] = t;
            }
            results_json.push(row);
        }

        let mut output = serde_json::json!({
            "query": query,
            "total_results": results_json.len(),
            "matched_total": matched_total,
            "results": results_json,
        });
        // Without the index only session metadata is searched, so a thin
        // result set means "content not searched", not "nothing matched".
        // The caller cannot tell the two apart from the results alone.
        if !free_text.is_empty()
            && let Some(map) = output.as_object_mut()
        {
            let status = match &index {
                Some((_, 0)) => "ready".to_string(),
                Some((_, behind)) => format!(
                    "ready; {behind} recently-written session(s) missing their newest messages{}",
                    if refreshing {
                        " (catching up in the background)"
                    } else {
                        ""
                    }
                ),
                None if refreshing || index_refresh_running() => {
                    "building: metadata-only results for now; content search is included \
                     once the build finishes"
                        .to_string()
                }
                None => "unavailable: metadata-only results".to_string(),
            };
            map.insert("content_index".into(), status.into());
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "Currently running and recently disconnected Claude Code sessions, one row each, with a `state` (active | awaiting_input | stalled | error | exited) and a `reason` for the non-active ones. tokens/cost cover the session's whole lifetime; cost is an API list-price estimate, not subscription spend. actionable_only=true keeps only the sessions that need attention; compact=true drops the heavy fields. All times local."
    )]
    fn live_sessions(
        &self,
        Parameters(params): Parameters<LiveSessionsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        use crate::infrastructure::live_diagnostic::LiveDiagnostic;
        use crate::infrastructure::live_sessions::{
            LiveSession, discover_live, discover_recently_paused, mark_was_recently_live,
        };

        let active = discover_live();
        let active_ids: std::collections::HashSet<String> =
            active.iter().map(|s| s.session_id.clone()).collect();
        let mut paused = discover_recently_paused(
            &active_ids,
            std::time::Duration::from_secs(24 * 3600),
            std::time::SystemTime::now(),
        );
        // MCP is read-only with no boot-freeze of its own; it reads the
        // TUI's latest persisted snapshot live as the restorable source.
        let prior_alive = crate::infrastructure::live_snapshots::latest_snapshot_alive();
        mark_was_recently_live(&mut paused, &prior_alive);
        let paused_ids: std::collections::HashSet<String> =
            paused.iter().map(|s| s.session_id.clone()).collect();
        let recovered = LiveDiagnostic::recover_missing(&active_ids, &paused_ids, &prior_alive);
        paused.extend(recovered);
        mark_was_recently_live(&mut paused, &prior_alive);

        // Lifetime cumulative per session (cost/tokens summed across EVERY day
        // the file appears, metadata from the newest day) — the same build-once
        // map the TUI Live tab indexes, so both surfaces report identical totals.
        let data = self.load();
        let cum_map = crate::aggregator::cumulative_by_path(&data.daily_groups);

        let now = chrono::Utc::now();
        let classify_tier = |s: &LiveSession| -> &'static str {
            if !s.is_live {
                "paused"
            } else if s.status.as_deref() == Some("busy") {
                "busy"
            } else {
                let t = s
                    .jsonl_mtime
                    .or(s.updated_at)
                    .or(s.started_at)
                    .unwrap_or(now);
                if crate::infrastructure::live_sessions::is_today(t, now) {
                    "today"
                } else {
                    "older"
                }
            }
        };

        let want_status = params.status.as_deref();
        let want_project = params.project.as_deref();

        let idle_threshold = params.idle_secs.unwrap_or(600);
        let actionable_only = params.actionable_only.unwrap_or(false);
        let compact = params.compact.unwrap_or(false);

        // Apply status/project filters, classify the session state (single source
        // shared with `--wait`), drop non-actionable rows when requested, then
        // shape the row (compact projection in `build_live_row`).
        let make_row = |s: &LiveSession, tier: &'static str| -> Option<serde_json::Value> {
            if let Some(want) = want_status
                && want != tier
            {
                return None;
            }
            if let Some(proj) = want_project
                && !s.cwd.to_string_lossy().contains(proj)
            {
                return None;
            }
            let age_secs = s
                .updated_at
                .or(s.jsonl_mtime)
                .or(s.started_at)
                .map(|t| (now - t).num_seconds().max(0));
            // The session-state idle MUST come from jsonl_mtime (last conversation
            // entry), matching `--wait`'s probe, so both surfaces classify
            // awaiting_input / stalled identically. age_secs above stays
            // updated_at-based — it's the row's display "age", not the turn idle.
            let idle = s
                .jsonl_mtime
                .map_or(0, |m| (now - m).num_seconds().max(0) as u64);
            let (state, tail) = crate::session_state::live_state(
                s.is_live,
                s.status.as_deref() == Some("busy"),
                idle,
                idle_threshold,
                s.jsonl_path.as_deref(),
            );
            if actionable_only && state == crate::session_state::SessionState::Active {
                return None;
            }
            let reason = crate::session_state::reason_for(state, tail, idle);
            let entry = s.jsonl_path.as_deref().and_then(|p| cum_map.get(p));
            let total = entry.map(|(c, _)| (c.cost, c.input_tokens + c.output_tokens, c.unpriced));
            let meta = entry.map(|(_, m)| *m);
            Some(build_live_row(
                s, tier, age_secs, state, &reason, compact, total, meta,
            ))
        };

        let mut rows: Vec<serde_json::Value> = Vec::new();
        for s in &active {
            let tier = classify_tier(s);
            if let Some(r) = make_row(s, tier) {
                rows.push(r);
            }
        }
        for s in &paused {
            if let Some(r) = make_row(s, "paused") {
                rows.push(r);
            }
        }

        let output = serde_json::json!({
            "active_count": active.len(),
            "paused_count": paused.len(),
            "sessions": rows,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&output).unwrap_or_default(),
        )]))
    }

    #[tool(
        description = "List sessions or get one session's detail. Rows describe one day of a session (`date`, `cost_usd`, `tokens`); a session active on several days also carries `session_total`. With session_id: conversation, tool usage and files touched over the whole session; conversation_query matches whole message text and returns a window around each match. Without: lists sessions filtered by query (metadata only: project/summary/branch/date), project, date range, sort; use `search` for content. All dates/times local."
    )]
    fn sessions(
        &self,
        Parameters(params): Parameters<SessionsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let data = self.load();

        if let Some(ref sid) = params.session_id {
            return Self::session_detail_inner_with_data(
                &data,
                sid,
                params.date.as_deref(),
                params.conversation_limit,
                params.conversation_offset,
                params.conversation_query.as_deref(),
                params.full_text.unwrap_or(false),
            );
        }

        let limit = params.limit.unwrap_or(20);
        let calculator = CostCalculator::global();
        let mut sessions_out: Vec<serde_json::Value> = Vec::new();

        let date_exact = params
            .date
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let date_from = params
            .date_from
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());
        let date_to = params
            .date_to
            .as_deref()
            .and_then(|d| d.parse::<NaiveDate>().ok());

        let date_matches = |group_date: NaiveDate| -> bool {
            if let Some(exact) = date_exact {
                return group_date == exact;
            }
            if let Some(from) = date_from
                && group_date < from
            {
                return false;
            }
            if let Some(to) = date_to
                && group_date > to
            {
                return false;
            }
            true
        };

        let pins = if params.pinned == Some(true) {
            Some(crate::pins::Pins::load().unwrap_or_else(|_| crate::pins::Pins::empty()))
        } else {
            None
        };

        let should_include =
            |group: &DailyGroup, session: &crate::aggregator::SessionInfo| -> bool {
                if !date_matches(group.date) {
                    return false;
                }
                if let Some(ref project) = params.project
                    && !session
                        .project_name
                        .to_lowercase()
                        .contains(&project.to_lowercase())
                {
                    return false;
                }
                if let Some(ref p) = pins
                    && !p.is_pinned(&session.file_path)
                {
                    return false;
                }
                true
            };

        let totals = crate::aggregator::cumulative_by_path(&data.daily_groups);
        for group in &data.daily_groups {
            for session in &group.sessions {
                if session.is_subagent {
                    continue;
                }
                if !should_include(group, session) {
                    continue;
                }
                let mut json = build_session_json(
                    group.date,
                    session,
                    calculator,
                    &data.summary_cache,
                    totals.get(session.file_path.as_path()),
                );
                // Judged on the first prompt, not the title: a generated
                // session keeps its tool prompt there even once titled.
                let auto = session
                    .summary
                    .as_deref()
                    .is_some_and(|s| AUTO_PREFIXES.iter().any(|p| s.starts_with(p)));
                if auto {
                    json["auto"] = serde_json::json!(true);
                }
                if let Some(ref query) = params.query {
                    let q = query.to_lowercase();
                    // Both the title shown and the first prompt: a titled
                    // session keeps its prompt's words searchable.
                    let first_prompt = session
                        .summary
                        .as_deref()
                        .or_else(|| data.summary_cache.get(&session.file_path)?.as_deref());
                    let meta_match = session.project_name.to_lowercase().contains(&q)
                        || json["summary"]
                            .as_str()
                            .is_some_and(|s| s.to_lowercase().contains(&q))
                        || first_prompt.is_some_and(|s| s.to_lowercase().contains(&q))
                        || session
                            .git_branch
                            .as_deref()
                            .is_some_and(|b| b.to_lowercase().contains(&q))
                        || group.date.to_string().contains(&q);
                    if !meta_match {
                        continue;
                    }
                }
                sessions_out.push(json);
            }
        }

        match params.sort.as_deref() {
            Some("cost") => sessions_out.sort_by(|a, b| {
                b["cost_usd"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .partial_cmp(&a["cost_usd"].as_f64().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            Some("tokens") => sessions_out.sort_by(|a, b| {
                let ta = a["tokens"]["input"].as_u64().unwrap_or(0)
                    + a["tokens"]["output"].as_u64().unwrap_or(0);
                let tb = b["tokens"]["input"].as_u64().unwrap_or(0)
                    + b["tokens"]["output"].as_u64().unwrap_or(0);
                tb.cmp(&ta)
            }),
            _ => {}
        }

        sessions_out.truncate(limit);

        let result = serde_json::json!({
            "count": sessions_out.len(),
            "sessions": sessions_out,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&result).unwrap_or_default(),
        )]))
    }
}

impl CcsightServer {
    fn session_detail_inner_with_data(
        data: &LoadedData,
        session_id: &str,
        date_str: Option<&str>,
        conv_limit: Option<usize>,
        conv_offset: Option<usize>,
        conv_query: Option<&str>,
        full_text: bool,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let date_filter = date_str.and_then(|d| d.parse::<NaiveDate>().ok());

        let found = data
            .daily_groups
            .iter()
            .filter(|g| date_filter.is_none() || Some(g.date) == date_filter)
            .flat_map(|g| g.sessions.iter().map(move |s| (g.date, s)))
            .find(|(_, s)| {
                s.file_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(session_id))
            });

        let Some((date, session)) = found else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::json!({"error": "Session not found"}).to_string(),
            )]));
        };

        let calculator = CostCalculator::global();
        let totals = crate::aggregator::cumulative_by_path(&data.daily_groups);
        let session_json = build_session_json(
            date,
            session,
            calculator,
            &data.summary_cache,
            totals.get(session.file_path.as_path()),
        );
        // Summed over every day: the conversation and `files_touched` below
        // cover the whole transcript, so a one-day tool count beside them
        // would describe a different span.
        let mut tool_usage: HashMap<&str, usize> = HashMap::new();
        for s in data
            .daily_groups
            .iter()
            .flat_map(|g| g.sessions.iter())
            .filter(|s| s.file_path == session.file_path)
        {
            for (tool, n) in &s.day_tool_usage {
                *tool_usage.entry(tool.as_str()).or_default() += n;
            }
        }

        let messages = load_conversation(&session.file_path).unwrap_or_default();
        let (files_touched, files_touched_total) = extract_files_touched(&messages);

        let entries = conversation_entries(&messages);
        let total = entries.len();
        let preview = |i: usize| entries[i].preview_json(full_text);

        let conversation = if let Some(q) = conv_query {
            let q_lower = q.to_lowercase();
            let match_indices: Vec<usize> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.text.to_lowercase().contains(&q_lower))
                .map(|(i, _)| i)
                .collect();

            let limit = conv_limit.unwrap_or(30);
            let mut include = std::collections::BTreeSet::new();
            for &idx in &match_indices {
                if idx > 0 {
                    include.insert(idx - 1);
                }
                include.insert(idx);
                if idx + 1 < total {
                    include.insert(idx + 1);
                }
            }

            let mut result_conv = Vec::new();
            let mut prev_idx: Option<usize> = None;
            for &idx in &include {
                if result_conv.len() >= limit {
                    break;
                }
                if let Some(prev) = prev_idx
                    && idx > prev + 1
                {
                    result_conv.push(serde_json::json!({
                        "role": "...",
                        "text": format!("({} entries omitted)", idx - prev - 1),
                    }));
                }
                let is_match = match_indices.binary_search(&idx).is_ok();
                let mut entry = if is_match {
                    entries[idx].match_json(&q_lower, full_text)
                } else {
                    preview(idx)
                };
                entry["offset"] = serde_json::json!(idx);
                if is_match {
                    entry["match"] = serde_json::json!(true);
                }
                result_conv.push(entry);
                prev_idx = Some(idx);
            }
            result_conv
        } else {
            let max_conv = conv_limit.unwrap_or(DEFAULT_CONVERSATION_ENTRIES);
            let offset = conv_offset.unwrap_or(0);
            if max_conv == 0 {
                vec![]
            } else if offset > 0 {
                let start = offset.min(total);
                let end = (start + max_conv).min(total);
                (start..end).map(preview).collect()
            } else if total <= max_conv {
                (0..total).map(preview).collect()
            } else {
                let head = max_conv * 2 / 3;
                let tail = max_conv - head;
                let mut v: Vec<serde_json::Value> = (0..head).map(preview).collect();
                v.push(serde_json::json!({
                    "role": "...",
                    "text": format!("({} entries omitted. Use conversation_offset to paginate, e.g. offset={} limit={})", total - head - tail, head, max_conv),
                }));
                v.extend((total - tail..total).map(preview));
                v
            }
        };

        let result = serde_json::json!({
            "session": session_json,
            "conversation": conversation,
            "conversation_total": total,
            "tool_usage": tool_usage,
            "files_touched": files_touched,
            "files_touched_total": files_touched_total,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&result).unwrap_or_default(),
        )]))
    }
}

/// Every `(day_idx, session_idx)` a user session occupies — the positions
/// `search` results address — newest day first, since groups run newest-first.
fn positions_by_path(groups: &[DailyGroup]) -> HashMap<&std::path::Path, Vec<(usize, usize)>> {
    let mut out: HashMap<&std::path::Path, Vec<(usize, usize)>> = HashMap::new();
    for (day_idx, group) in groups.iter().enumerate() {
        for (session_idx, s) in group.user_sessions().enumerate() {
            out.entry(s.file_path.as_path())
                .or_default()
                .push((day_idx, session_idx));
        }
    }
    out
}

/// First position whose day passes `in_range`; `days` runs newest first.
fn newest_in_range(
    days: &[(usize, usize)],
    in_range: impl Fn(usize) -> bool,
) -> Option<(usize, usize)> {
    days.iter().copied().find(|(d, _)| in_range(*d))
}

/// Index hits fetched when a date or project filter narrows the results.
const FILTERED_INDEX_FETCH: usize = 2000;

/// Set while a background index build or catch-up runs, so repeated `search`
/// calls start at most one.
static INDEX_REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Unix seconds at which the last background refresh finished; 0 = never.
static LAST_REFRESH_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Seconds a finished refresh holds off the next catch-up. Each one re-parses
/// every session written since the last, whole — seconds of CPU and hundreds
/// of MB for one long transcript.
const CATCH_UP_COOL_DOWN_SECS: u64 = 60;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn catch_up_cooled_down() -> bool {
    cooled_down(
        LAST_REFRESH_DONE.load(std::sync::atomic::Ordering::Acquire),
        unix_now(),
    )
}

fn cooled_down(last_done: u64, now: u64) -> bool {
    now.saturating_sub(last_done) >= CATCH_UP_COOL_DOWN_SECS
}

fn index_refresh_running() -> bool {
    INDEX_REFRESH.load(std::sync::atomic::Ordering::Acquire)
}

/// Build or catch up the content index off the request. Returns whether this
/// call started the work. A writer lock held by another ccsight process makes
/// the attempt fail harmlessly; the next `search` retries.
fn spawn_index_refresh(data: &std::sync::Arc<LoadedData>) -> bool {
    use std::sync::atomic::Ordering;
    struct Reset;
    impl Drop for Reset {
        // Runs on unwind too, so a panicking build cannot wedge the flag on
        // and block every later refresh.
        fn drop(&mut self) {
            LAST_REFRESH_DONE.store(unix_now(), Ordering::Release);
            INDEX_REFRESH.store(false, Ordering::Release);
        }
    }
    if INDEX_REFRESH.swap(true, Ordering::AcqRel) {
        return false;
    }
    let data = std::sync::Arc::clone(data);
    std::thread::spawn(move || {
        let _reset = Reset;
        let _ = crate::infrastructure::SearchIndex::update_or_build(&data.daily_groups);
    });
    true
}

/// Index hits fetched before dedup, when `limit * 5` would be smaller. One hit
/// is one matching message, so a session that mentions the term throughout
/// collapses many hits into one result.
const MIN_INDEX_FETCH: usize = 200;

/// Conversation entries `sessions` returns for one session when the caller
/// names no limit. An unbounded default hands a whole transcript to a caller
/// that asked only "what happened here"; the omission marker in the middle
/// names `conversation_offset` for the rest.
const DEFAULT_CONVERSATION_ENTRIES: usize = 60;

/// Round a dollar figure to whole cents for JSON `cost_usd` output. Clamps at
/// 0 so a zero-token session can't emit `-0.0` (the TUI clamps via format_cost).
fn round_cents(cost: f64) -> f64 {
    (cost.max(0.0) * 100.0).round() / 100.0
}

fn clean_summary(text: &str) -> String {
    let text = text.trim();
    if text.starts_with("Implement the following plan:") || text.starts_with("/plan") {
        for line in text.lines().skip(1) {
            let line = line.trim();
            if let Some(title) = line.strip_prefix("# ") {
                return truncate(title.trim(), 120);
            }
            if !line.is_empty() && !line.starts_with('#') && line.len() > 5 {
                return truncate(line, 120);
            }
        }
    }
    truncate(text, 120)
}

fn truncate(text: &str, max: usize) -> String {
    let truncated: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{truncated}...")
    } else {
        truncated
    }
}

/// One conversation message with its whole text. Matching runs on this, and
/// only the JSON built from it is shortened: searching the shortened copy
/// misses every term past its cut, even where `search` located one.
struct ConvEntry {
    role: String,
    time: Option<String>,
    text: String,
}

/// Chars a preview keeps per message; user prompts carry the intent, so they
/// get the larger share.
fn preview_len(role: &str) -> usize {
    if role == "user" { 300 } else { 200 }
}

/// Chars of a matched message returned around its first occurrence. A
/// message can run to tens of thousands of chars, so the whole of it is
/// returned only on request (`full_text`).
const MATCH_WINDOW_CHARS: usize = 2000;

fn conversation_entries(messages: &[ConversationMessage]) -> Vec<ConvEntry> {
    messages
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter_map(|msg| {
            let text: String = msg
                .blocks
                .iter()
                .filter_map(|b| match b {
                    ConversationBlock::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                return None;
            }
            // Dated: one transcript can span months, so a bare clock time
            // cannot say which day a message belongs to.
            let time = msg
                .timestamp_utc
                .map(|t| {
                    t.with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .or_else(|| msg.timestamp.clone());
            Some(ConvEntry {
                role: msg.role.clone(),
                time,
                text,
            })
        })
        .collect()
}

impl ConvEntry {
    fn preview_json(&self, full_text: bool) -> serde_json::Value {
        let text = if full_text {
            self.text.clone()
        } else {
            truncate(&self.text, preview_len(&self.role))
        };
        serde_json::json!({ "role": self.role, "time": self.time, "text": text })
    }

    /// The matched message, cut to a window that is guaranteed to contain
    /// the first occurrence of `q_lower` rather than the message's head.
    fn match_json(&self, q_lower: &str, full_text: bool) -> serde_json::Value {
        let chars: Vec<char> = self.text.chars().collect();
        if full_text || chars.len() <= MATCH_WINDOW_CHARS {
            return self.preview_json(true);
        }
        let lower = self.text.to_lowercase();
        let hit = lower
            .find(q_lower)
            .map_or(0, |b| lower[..b].chars().count())
            .min(chars.len());
        let start = hit
            .saturating_sub(MATCH_WINDOW_CHARS / 2)
            .min(chars.len() - MATCH_WINDOW_CHARS);
        let end = start + MATCH_WINDOW_CHARS;
        let mut text: String = chars[start..end].iter().collect();
        if start > 0 {
            text.insert_str(0, "...");
        }
        if end < chars.len() {
            text.push_str("...");
        }
        serde_json::json!({
            "role": self.role,
            "time": self.time,
            "text": text,
            "text_truncated": true,
        })
    }
}

/// Files `files_touched` lists, most-touched first. A long session touches
/// hundreds, and the order is what lets a capped list still name the ones the
/// session was about.
const MAX_FILES_TOUCHED: usize = 40;

/// Scratch locations: a session's working files, not what it changed.
const SCRATCH_PREFIXES: &[&str] = &[
    "/tmp/",
    "/private/tmp/",
    "/private/var/folders/",
    "/var/folders/",
];

/// `(listed, total)`: the capped, most-touched-first list and how many
/// distinct non-scratch files there were in all.
fn extract_files_touched(messages: &[ConversationMessage]) -> (Vec<String>, usize) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for msg in messages {
        for block in &msg.blocks {
            if let ConversationBlock::ToolUse {
                name,
                input_summary,
            } = block
                && matches!(name.as_str(), "Read" | "Edit" | "Write" | "MultiEdit")
                && !input_summary.is_empty()
                && !SCRATCH_PREFIXES
                    .iter()
                    .any(|p| input_summary.starts_with(p))
            {
                *counts.entry(input_summary.as_str()).or_default() += 1;
            }
        }
    }
    let total = counts.len();
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let listed = ranked
        .into_iter()
        .take(MAX_FILES_TOUCHED)
        .map(|(f, _)| f.to_string())
        .collect();
    (listed, total)
}

const AUTO_PREFIXES: &[&str] = &["You are a rule generator", "Generate a git commit message"];

fn derive_summary(file_path: &std::path::Path) -> Option<String> {
    use crate::domain::EntryType;
    use crate::parser::JsonlParser;

    let entries = JsonlParser::parse_file(file_path).ok()?;

    for entry in entries.iter().take(50) {
        if entry.entry_type != EntryType::User {
            continue;
        }
        let message = entry.message.as_ref()?;
        let text = message.content.extract_text();
        let text = text.trim();
        if text.is_empty() || text.starts_with('<') || text.starts_with('[') {
            continue;
        }
        if AUTO_PREFIXES.iter().any(|p| text.starts_with(p)) {
            continue;
        }
        return Some(clean_summary(text));
    }
    None
}

/// Whole-session figures for a row that describes one day of the session.
/// `None` when it was active on a single day — the row's own figures are then
/// the session's, and repeating them would only cost the caller context.
fn session_total_json(
    total: &(
        crate::aggregator::SessionCumulative,
        &crate::aggregator::SessionInfo,
    ),
) -> Option<serde_json::Value> {
    let (cum, newest) = total;
    if cum.days <= 1 {
        return None;
    }
    let mut v = serde_json::json!({
        "cost_usd": round_cents(cum.cost),
        "tokens": {
            "input": cum.input_tokens,
            "output": cum.output_tokens,
            "cache_creation": cum.cache_creation,
            "cache_read": cum.cache_read,
        },
        "active_days": cum.days,
        "first_date": cum
            .earliest_start
            .map(|t| t.with_timezone(&Local).date_naive().to_string()),
        "last_date": newest
            .day_last_timestamp
            .with_timezone(&Local)
            .date_naive()
            .to_string(),
    });
    if cum.unpriced {
        v["cost_is_lower_bound"] = serde_json::json!(true);
    }
    Some(v)
}

fn build_session_json(
    date: NaiveDate,
    session: &crate::aggregator::SessionInfo,
    calculator: &CostCalculator,
    summary_cache: &HashMap<PathBuf, Option<String>>,
    total: Option<&(
        crate::aggregator::SessionCumulative,
        &crate::aggregator::SessionInfo,
    )>,
) -> serde_json::Value {
    let mut cost = 0.0;
    let mut cache_creation = 0u64;
    let mut cache_read = 0u64;
    for (model, tokens) in &session.day_tokens_by_model {
        cost += calculator
            .calculate_cost(tokens, Some(model))
            .unwrap_or(0.0);
        cache_creation += tokens.cache_creation_tokens;
        cache_read += tokens.cache_read_tokens;
    }

    let first_local = session.day_first_timestamp.with_timezone(&Local);
    let last_local = session.day_last_timestamp.with_timezone(&Local);
    let time_range = format!(
        "{}–{}",
        first_local.format("%H:%M"),
        last_local.format("%H:%M")
    );
    let duration_mins = (session.day_last_timestamp - session.day_first_timestamp).num_minutes();

    let session_id = session
        .file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .chars()
        .take(8)
        .collect::<String>();

    let model_display = session
        .model
        .as_deref()
        .map(crate::aggregator::normalize_model_name)
        .unwrap_or_default();

    // The newest day's slice carries the current title: a rename or a late AI
    // title lands there, and the TUI and `search` read it from the same place.
    let titled = total.map_or(session, |(_, newest)| *newest);
    let summary = titled
        .display_title()
        .map(str::to_string)
        .or_else(|| summary_cache.get(&session.file_path).and_then(Clone::clone));

    let mut row = serde_json::json!({
        "session_id": session_id,
        "date": date.to_string(),
        "time": time_range,
        "project": session.project_name,
        "model": model_display,
        "tokens": {
            "input": session.day_input_tokens,
            "output": session.day_output_tokens,
            "cache_creation": cache_creation,
            "cache_read": cache_read,
        },
        "cost_usd": round_cents(cost),
        "summary": summary,
        "git_branch": session.git_branch,
        "duration_mins": duration_mins,
    });
    if let Some(t) = total.and_then(session_total_json) {
        row["session_total"] = t;
    }
    row
}

#[tool_handler]
impl ServerHandler for CcsightServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ccsight", env!("CARGO_PKG_VERSION")))
            .with_instructions("Claude Code usage analytics. Use 'stats' for aggregated metrics, 'sessions' to search sessions. Tool guide: 'search' to find past conversations by content (full-text, returns snippets with session_id) -> 'sessions' with session_id to get conversation detail. 'stats' for cost/token/model summaries. All tools accept date_from/date_to (YYYY-MM-DD) for date filtering.")
    }
}

pub async fn run_mcp_server(limit: usize) -> anyhow::Result<()> {
    let server = CcsightServer::new(limit);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::test_helpers::helpers::{make_daily_group, make_session, make_session_with_tokens};

    struct TempFile(PathBuf);
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    impl TempFile {
        fn new(prefix: &str) -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!("{prefix}_{id}.jsonl")))
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn write_jsonl(&self, entries: &[serde_json::Value]) {
            let mut f = std::fs::File::create(&self.0).unwrap();
            for e in entries {
                writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
            }
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).ok();
        }
    }

    fn extract_conversation(messages: &[ConversationMessage]) -> Vec<serde_json::Value> {
        conversation_entries(messages)
            .iter()
            .map(|e| e.preview_json(false))
            .collect()
    }

    fn extract_json(result: &CallToolResult) -> serde_json::Value {
        let text = &result.content[0]
            .as_text()
            .expect("expected text content")
            .text;
        serde_json::from_str(text).unwrap()
    }

    fn call_stats(server: &CcsightServer, period: Option<&str>) -> serde_json::Value {
        let params = StatsParams {
            period: period.map(String::from),
            date_from: None,
            date_to: None,
            group_by: None,
            compact: Some(false),
        };
        extract_json(&server.stats(Parameters(params)).unwrap())
    }

    // `mcp_servers` + `projects` + `languages` are the bulk of the payload, and
    // a caller after totals pays for them in context it cannot use.
    #[test]
    fn stats_compact_drops_the_per_entity_lists_and_keeps_the_totals() {
        let server = CcsightServer::from_groups(make_test_groups());
        let call = |compact: Option<bool>| {
            extract_json(
                &server
                    .stats(Parameters(StatsParams {
                        period: None,
                        date_from: None,
                        date_to: None,
                        group_by: None,
                        compact,
                    }))
                    .unwrap(),
            )
        };
        let full = call(Some(false));
        let compact = call(None);
        assert_eq!(compact, call(Some(true)), "an unset compact is compact");

        for key in ["mcp_servers", "projects", "languages"] {
            assert!(full.get(key).is_some(), "{key} missing from the full form");
            assert!(compact.get(key).is_none(), "{key} survived compact");
        }
        for key in [
            "total_cost_usd",
            "total_sessions",
            "tokens",
            "model_breakdown",
            "hourly_pattern",
            "top_tools",
        ] {
            assert!(compact.get(key).is_some(), "compact dropped {key}");
        }
        assert_eq!(
            compact["total_cost_usd"], full["total_cost_usd"],
            "compact must not change the numbers"
        );
        assert!(
            compact.to_string().len() < full.to_string().len(),
            "compact has to be smaller"
        );
    }

    fn call_stats_range(
        server: &CcsightServer,
        start: &str,
        end: Option<&str>,
    ) -> serde_json::Value {
        let params = StatsParams {
            period: None,
            date_from: Some(start.to_string()),
            date_to: end.map(String::from),
            group_by: None,
            compact: Some(false),
        };
        extract_json(&server.stats(Parameters(params)).unwrap())
    }

    fn call_sessions(
        server: &CcsightServer,
        query: Option<&str>,
        project: Option<&str>,
        date: Option<&str>,
        limit: Option<usize>,
    ) -> serde_json::Value {
        let params = SessionsParams {
            session_id: None,
            query: query.map(String::from),
            project: project.map(String::from),
            date: date.map(String::from),
            date_from: None,
            date_to: None,
            sort: None,
            limit,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        extract_json(&server.sessions(Parameters(params)).unwrap())
    }

    fn call_session_detail(
        server: &CcsightServer,
        session_id: &str,
        date: Option<&str>,
    ) -> serde_json::Value {
        let params = SessionsParams {
            session_id: Some(session_id.to_string()),
            query: None,
            project: None,
            date: date.map(String::from),
            date_from: None,
            date_to: None,
            sort: None,
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        extract_json(&server.sessions(Parameters(params)).unwrap())
    }

    fn make_test_groups() -> Vec<DailyGroup> {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let last_week = today - chrono::Duration::days(8);

        // Distinct file_path per session: total_sessions dedups by path, so the
        // default shared `/tmp/test.jsonl` would collapse every session to one.
        let with_path = |mut s: crate::aggregator::SessionInfo, path: &str| {
            s.file_path = std::path::PathBuf::from(path);
            s
        };
        vec![
            make_daily_group(
                today,
                vec![
                    with_path(
                        make_session_with_tokens(
                            "~/projects/app-a",
                            100_000,
                            50_000,
                            "claude-sonnet-4-20250514",
                        ),
                        "/tmp/app-a-today.jsonl",
                    ),
                    {
                        let mut s = with_path(
                            make_session_with_tokens(
                                "~/projects/other",
                                50_000,
                                25_000,
                                "claude-sonnet-4-20250514",
                            ),
                            "/tmp/other-today.jsonl",
                        );
                        s.summary = Some("Fix login bug".to_string());
                        s.git_branch = Some("fix/login".to_string());
                        s
                    },
                ],
            ),
            make_daily_group(
                yesterday,
                vec![with_path(
                    make_session_with_tokens(
                        "~/projects/app-a",
                        80_000,
                        40_000,
                        "claude-opus-4-5-20251101",
                    ),
                    "/tmp/app-a-yesterday.jsonl",
                )],
            ),
            make_daily_group(
                last_week,
                vec![with_path(
                    make_session_with_tokens(
                        "~/projects/old-project",
                        30_000,
                        10_000,
                        "claude-sonnet-4-20250514",
                    ),
                    "/tmp/old-project.jsonl",
                )],
            ),
        ]
    }

    #[test]
    fn test_period_to_filter_mappings() {
        assert_eq!(period_to_filter(Some("today")), PeriodFilter::Today);
        assert_eq!(period_to_filter(Some("week")), PeriodFilter::Last7d);
        assert_eq!(period_to_filter(Some("month")), PeriodFilter::Last30d);
        assert_eq!(period_to_filter(Some("all")), PeriodFilter::All);
        assert_eq!(period_to_filter(None), PeriodFilter::All);
        assert_eq!(period_to_filter(Some("unknown")), PeriodFilter::All);
    }

    #[test]
    fn test_filter_groups_by_date_range_no_filter() {
        let groups = make_test_groups();
        let filtered = filter_groups_by_date_range(&groups, None, None);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_filter_groups_by_date_range_start_only() {
        let groups = make_test_groups();
        let today = Local::now().date_naive();
        let filtered = filter_groups_by_date_range(&groups, Some(today), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].date, today);
    }

    #[test]
    fn test_filter_groups_by_date_range_start_and_end() {
        let groups = make_test_groups();
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let filtered = filter_groups_by_date_range(&groups, Some(yesterday), Some(today));
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_groups_empty_result() {
        let groups = make_test_groups();
        let future = Local::now().date_naive() + chrono::Duration::days(30);
        let filtered = filter_groups_by_date_range(&groups, Some(future), None);
        assert!(filtered.is_empty());
    }

    fn live_session(id: &str) -> crate::infrastructure::live_sessions::LiveSession {
        crate::infrastructure::live_sessions::LiveSession {
            session_id: id.to_string(),
            jsonl_path: None,
            cwd: std::path::PathBuf::from("/Users/me/proj"),
            name: None,
            status: None,
            pid: 5,
            started_at: None,
            updated_at: None,
            jsonl_mtime: None,
            is_live: true,
            was_recently_live: false,
        }
    }

    #[test]
    fn build_live_row_compact_keeps_state_drops_heavy_fields() {
        let s = live_session("1f3a9c2e");
        let row = build_live_row(
            &s,
            "today",
            Some(120),
            crate::session_state::SessionState::AwaitingInput,
            "assistant turn finished; no input for 2m",
            true,
            None,
            None,
        );
        assert_eq!(row["state"], "awaiting_input");
        assert_eq!(row["age_secs"], 120);
        assert_eq!(row["project"], "proj");
        assert!(row.get("reason").is_some());
        for k in ["tokens", "cost", "model", "ai_title", "last_user_message"] {
            assert!(row.get(k).is_none(), "compact must drop {k}");
        }
    }

    #[test]
    fn build_live_row_full_has_state_and_heavy_fields_and_omits_empty_reason() {
        let s = live_session("x");
        let row = build_live_row(
            &s,
            "today",
            Some(0),
            crate::session_state::SessionState::Active,
            "",
            false,
            None,
            None,
        );
        assert_eq!(row["state"], "active");
        assert!(row.get("tokens").is_some());
        assert!(row.get("cost").is_some());
        assert!(row.get("last_user_message").is_some());
        // An active session has no reason → field omitted, not empty string.
        assert!(row.get("reason").is_none());
    }

    #[test]
    fn build_live_row_reports_cross_day_lifetime_total_not_a_single_day() {
        // The MCP-side wiring: index the shared cumulative_by_path map and
        // forward (cost, input+output) into the row. Pins that the row carries
        // the cross-day SUM and the newest day's model — guarding the exact seam
        // a per-day / single-day regression would silently revert.
        let newer = Local::now().date_naive();
        let older = newer - chrono::Duration::days(1);
        // Same default file_path on both days, so cumulative_by_path folds them.
        let groups = vec![
            make_daily_group(
                newer,
                vec![make_session_with_tokens("p", 3000, 800, "claude-opus-4-8")],
            ),
            make_daily_group(
                older,
                vec![make_session_with_tokens(
                    "p",
                    1000,
                    200,
                    "claude-sonnet-4-6",
                )],
            ),
        ];
        let map = crate::aggregator::cumulative_by_path(&groups);
        let path = std::path::Path::new("/tmp/test.jsonl");
        let (cum, meta) = map.get(path).expect("folded session");
        let total = Some((cum.cost, cum.input_tokens + cum.output_tokens, cum.unpriced));

        let s = live_session("x");
        let row = build_live_row(
            &s,
            "today",
            Some(0),
            crate::session_state::SessionState::Active,
            "",
            false,
            total,
            Some(meta),
        );
        // Lifetime total 3000+800+1000+200, NOT the newest day's 3800.
        assert_eq!(row["tokens"], 5000);
        assert_ne!(row["tokens"], 3800);
        assert!(row["cost"].as_f64().unwrap() > 0.0);
        // Representative model is the newest day's slice (Opus, not Sonnet).
        assert_eq!(row["model"], "Opus 4.8");
        // All models priced → no lower-bound flag (parity with the TUI's no-`*`).
        assert!(row.get("cost_is_lower_bound").is_none());
    }

    #[test]
    fn build_live_row_flags_cost_is_lower_bound_for_unpriced_model() {
        // An unpriced model makes `cost` an undercount; the row must signal it
        // (the MCP analogue of the TUI's `$N*`), and only in non-compact mode.
        let s = live_session("x");
        let unpriced_total = Some((0.0, 1234, true));
        let row = build_live_row(
            &s,
            "today",
            Some(0),
            crate::session_state::SessionState::Active,
            "",
            false,
            unpriced_total,
            None,
        );
        assert_eq!(row["cost_is_lower_bound"], true);

        // Compact mode drops cost entirely, so the flag must not appear there.
        let compact = build_live_row(
            &s,
            "today",
            Some(0),
            crate::session_state::SessionState::Active,
            "",
            true,
            unpriced_total,
            None,
        );
        assert!(compact.get("cost_is_lower_bound").is_none());
    }

    #[test]
    fn test_build_session_json_fields() {
        let session = make_session_with_tokens(
            "~/projects/myapp",
            100_000,
            50_000,
            "claude-sonnet-4-20250514",
        );
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let calculator = CostCalculator::global();

        let json = build_session_json(date, &session, calculator, &HashMap::new(), None);

        assert_eq!(json["date"], "2026-03-19"); // lint-ok: date-literal
        assert_eq!(json["project"], "~/projects/myapp");
        assert_eq!(json["model"], "Sonnet 4");
        assert_eq!(json["tokens"]["input"], 100_000);
        assert_eq!(json["tokens"]["output"], 50_000);
        assert!(json["cost_usd"].as_f64().unwrap() > 0.0);
        assert!(json["duration_mins"].is_number());
        assert!(json["time"].is_string());
    }

    #[test]
    fn test_build_session_json_with_summary_and_branch() {
        let mut session = make_session(
            "~/projects/app",
            Some("Added MCP support"),
            Some("feature/mcp"),
        );
        session.model = Some("claude-sonnet-4-20250514".to_string());
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let json = build_session_json(
            date,
            &session,
            CostCalculator::global(),
            &HashMap::new(),
            None,
        );

        assert_eq!(json["summary"], "Added MCP support");
        assert_eq!(json["git_branch"], "feature/mcp");
    }

    #[test]
    fn test_build_session_json_null_optional_fields() {
        let session =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let json = build_session_json(
            date,
            &session,
            CostCalculator::global(),
            &HashMap::new(),
            None,
        );

        assert!(json["summary"].is_null());
        assert!(json["git_branch"].is_null());
    }

    #[test]
    fn test_build_session_json_session_id_truncated() {
        let mut session =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        session.file_path = std::path::PathBuf::from("/tmp/abcdefghijklmnop.jsonl");
        let date = NaiveDate::from_ymd_opt(2026, 3, 19).unwrap(); // lint-ok: date-literal
        let json = build_session_json(
            date,
            &session,
            CostCalculator::global(),
            &HashMap::new(),
            None,
        );

        assert_eq!(json["session_id"].as_str().unwrap().len(), 8);
        assert_eq!(json["session_id"], "abcdefgh");
    }

    #[test]
    fn load_memoizes_on_matching_signature_and_reloads_on_change() {
        let server = CcsightServer::from_groups(Vec::new());
        // Same signature → the memoized Arc is reused, not rebuilt.
        let a = server.load();
        let b = server.load();
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        // A signature mismatch (as if the on-disk state changed) → rebuild.
        {
            let mut guard = server.cached.lock().unwrap();
            let (sig, _) = guard.as_mut().unwrap();
            *sig = sig.wrapping_add(1);
        }
        let c = server.load();
        assert!(!std::sync::Arc::ptr_eq(&a, &c));
    }

    #[test]
    fn test_stats_total_sessions_dedups_multiday_session() {
        // One session resumed across two days appears in both day groups; it is
        // one session, not two. total_sessions must dedup by file_path.
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let mut day1 =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        day1.file_path = std::path::PathBuf::from("/tmp/resumed.jsonl");
        let mut day2 =
            make_session_with_tokens("~/projects/app", 2000, 1000, "claude-sonnet-4-20250514");
        day2.file_path = std::path::PathBuf::from("/tmp/resumed.jsonl");
        let server = CcsightServer::from_groups(vec![
            make_daily_group(yesterday, vec![day1]),
            make_daily_group(today, vec![day2]),
        ]);
        let json = call_stats(&server, None);
        assert_eq!(
            json["total_sessions"], 1,
            "multi-day session counts once, not per active day"
        );
        // The per-project count must reconcile with the deduped total in the
        // same response, while tokens still sum both day slices.
        assert_eq!(json["projects"][0]["sessions"], 1);
        assert_eq!(json["projects"][0]["work_tokens"], 4500);
    }

    #[test]
    fn test_stats_all_period() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, None);

        assert_eq!(json["period"], "all");
        assert_eq!(json["total_sessions"], 4);
        assert!(json["total_cost_usd"].as_f64().unwrap() > 0.0);
        assert!(json["tokens"]["input"].as_u64().unwrap() > 0);
        assert!(json["tokens"]["output"].as_u64().unwrap() > 0);
        assert!(!json["model_breakdown"].as_array().unwrap().is_empty());
        assert!(!json["projects"].as_array().unwrap().is_empty());
        assert!(json["projects"][0]["cost_usd"].as_f64().is_some());
        assert!(json["cache_hit_rate_pct"].as_f64().is_some());
        assert!(json["tokens"]["cache_creation"].as_u64().is_some());
        assert!(json["tokens"]["cache_read"].as_u64().is_some());
    }

    #[test]
    fn test_stats_today_period() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, Some("today"));

        assert_eq!(json["period"], "today");
        assert_eq!(json["total_sessions"], 2);
    }

    #[test]
    fn test_stats_week_period() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, Some("week"));

        assert_eq!(json["period"], "week");
        assert_eq!(
            json["total_sessions"], 3,
            "should include today + yesterday but not last_week"
        );
    }

    #[test]
    fn test_stats_subagent_cost_included_sessions_excluded() {
        let today = Local::now().date_naive();
        let mut subagent =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        subagent.is_subagent = true;

        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514"),
                subagent,
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        assert_eq!(
            json["total_sessions"], 1,
            "session count excludes subagents"
        );
        assert_eq!(
            json["tokens"]["input"], 2000,
            "token counts include subagents"
        );
        assert!(
            json["total_cost_usd"].as_f64().unwrap() > 0.0,
            "cost includes subagents"
        );

        let single = call_stats(
            &CcsightServer::from_groups(vec![make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/app",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            )]),
            None,
        );
        let cost_without = single["total_cost_usd"].as_f64().unwrap();
        let cost_with = json["total_cost_usd"].as_f64().unwrap();
        assert!(
            cost_with > cost_without,
            "cost with subagent should be higher"
        );
    }

    #[test]
    fn test_stats_projects_returns_all() {
        let today = Local::now().date_naive();
        let sessions: Vec<_> = (0..8)
            .map(|i| {
                make_session_with_tokens(
                    &format!("~/projects/project-{i}"),
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )
            })
            .collect();
        let server = CcsightServer::from_groups(vec![make_daily_group(today, sessions)]);
        let json = call_stats(&server, None);

        assert_eq!(json["projects"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn test_stats_empty_groups() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_stats(&server, None);

        assert_eq!(json["total_sessions"], 0);
        assert_eq!(json["total_cost_usd"], 0.0);
        assert_eq!(json["tokens"]["input"], 0);
        // No sessions → no pricing gap to report.
        assert!(json["pricing_gap"].is_null());
    }

    #[test]
    fn test_stats_pricing_gap_flagged_for_unknown_model() {
        // Regression: when a session uses a model not in `pricing.rs`, the stats tool
        // must surface `pricing_gap` so callers know `total_cost_usd` is undercounted.
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![make_session_with_tokens(
                "~/projects/app",
                100_000,
                50_000,
                "claude-future-experimental-x",
            )],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        assert!(
            !json["pricing_gap"].is_null(),
            "pricing_gap should be present when an unknown model is used. Got: {json}"
        );
        assert_eq!(json["pricing_gap"]["count"], 1);
        let models = json["pricing_gap"]["models"]
            .as_array()
            .expect("models is array");
        assert_eq!(models.len(), 1);
        // The model name is normalized — future families preserve their raw name.
        assert!(
            models[0]
                .as_str()
                .is_some_and(|s| s.contains("future-experimental-x")),
            "pricing_gap.models should include the raw/normalized unknown model name. Got: {json}"
        );
    }

    #[test]
    fn test_stats_no_pricing_gap_when_all_models_priced() {
        // Regression: when every model in the window has pricing, `pricing_gap` must be
        // null so downstream consumers don't warn unnecessarily.
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![make_session_with_tokens(
                "~/projects/app",
                100_000,
                50_000,
                "claude-opus-4-5-20251101",
            )],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        assert!(
            json["pricing_gap"].is_null(),
            "pricing_gap should be null when all models are priced. Got: {json}"
        );
    }

    #[test]
    fn test_stats_model_breakdown_sorted_by_cost() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens(
                    "~/projects/app",
                    100_000,
                    50_000,
                    "claude-opus-4-5-20251101",
                ),
                make_session_with_tokens(
                    "~/projects/app",
                    100_000,
                    50_000,
                    "claude-sonnet-4-20250514",
                ),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats(&server, None);

        let breakdown = json["model_breakdown"].as_array().unwrap();
        assert!(breakdown.len() >= 2);
        let first_cost = breakdown[0]["cost_usd"].as_f64().unwrap();
        let second_cost = breakdown[1]["cost_usd"].as_f64().unwrap();
        assert!(first_cost >= second_cost);
    }

    #[test]
    fn test_stats_hourly_work_tokens() {
        let today = Local::now().date_naive();
        let mut session =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        session.day_hourly_work_tokens.insert(10, 5000);
        session.day_hourly_work_tokens.insert(14, 3000);

        let server = CcsightServer::from_groups(vec![make_daily_group(today, vec![session])]);
        let json = call_stats(&server, None);

        let hourly = json["hourly_pattern"].as_object().unwrap();
        assert_eq!(hourly["10"], 5000);
        assert_eq!(hourly["14"], 3000);
    }

    #[test]
    fn test_stats_date_range_for_all() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_stats(&server, Some("all"));
        assert!(json["date_range"].is_null());
    }

    #[test]
    fn test_stats_date_range_for_today() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_stats(&server, Some("today"));
        let today = Local::now().date_naive().to_string();
        assert_eq!(json["date_range"]["start"], today);
        assert_eq!(json["date_range"]["end"], today);
    }

    #[test]
    fn test_stats_tokens_total_work_equals_input_plus_output() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, None);

        let input = json["tokens"]["input"].as_u64().unwrap();
        let output = json["tokens"]["output"].as_u64().unwrap();
        let total_work = json["tokens"]["total_work"].as_u64().unwrap();
        assert_eq!(total_work, input + output);
    }

    #[test]
    fn test_sessions_no_filter() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, None, None, None);

        assert_eq!(json["count"], 4);
        assert_eq!(json["sessions"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn test_sessions_project_filter() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, Some("app-a"), None, None);

        assert_eq!(json["count"], 2);
        for s in json["sessions"].as_array().unwrap() {
            assert!(s["project"].as_str().unwrap().contains("app-a"));
        }
    }

    #[test]
    fn test_sessions_date_filter() {
        let today = Local::now().date_naive();
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, None, Some(&today.to_string()), None);

        assert_eq!(json["count"], 2);
        for s in json["sessions"].as_array().unwrap() {
            assert_eq!(s["date"].as_str().unwrap(), today.to_string());
        }
    }

    #[test]
    fn test_sessions_query_filter() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, Some("login"), None, None, None);

        assert_eq!(json["count"], 1);
        assert_eq!(json["sessions"][0]["summary"], "Fix login bug");
    }

    #[test]
    fn test_sessions_limit() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, None, None, Some(2));
        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_sessions_excludes_subagents() {
        let today = Local::now().date_naive();
        let mut subagent =
            make_session_with_tokens("~/projects/app", 1000, 500, "claude-sonnet-4-20250514");
        subagent.is_subagent = true;

        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/app", 2000, 1000, "claude-sonnet-4-20250514"),
                subagent,
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, None, None, None, None);
        assert_eq!(json["count"], 1);
    }

    #[test]
    fn test_sessions_empty_groups() {
        let server = CcsightServer::from_groups(vec![]);
        let json = call_sessions(&server, None, None, None, None);

        assert_eq!(json["count"], 0);
        assert!(json["sessions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_sessions_no_match_query() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, Some("nonexistent-xyz-123"), None, None, None);
        assert_eq!(json["count"], 0);
    }

    #[test]
    fn test_sessions_project_case_insensitive() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_sessions(&server, None, Some("APP-A"), None, None);
        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_sessions_query_excludes_subagents() {
        let today = Local::now().date_naive();
        let mut subagent = make_session("~/projects/app", Some("Fix login bug"), Some("main"));
        subagent.is_subagent = true;
        let normal = make_session("~/projects/app", Some("Fix login issue"), Some("main"));

        let groups = vec![make_daily_group(today, vec![normal, subagent])];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("login"), None, None, None);

        assert_eq!(json["count"], 1, "query results should exclude subagents");
    }

    #[test]
    fn test_sessions_query_with_project_filter() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session("~/projects/app-a", Some("Fix login bug"), None),
                make_session("~/projects/app-b", Some("Fix login issue"), None),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("login"), Some("app-a"), None, None);

        assert_eq!(json["count"], 1);
        assert_eq!(json["sessions"][0]["project"], "~/projects/app-a");
    }

    #[test]
    fn test_sessions_query_with_date_filter() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session(
                    "~/projects/app",
                    Some("Fix login today"),
                    None,
                )],
            ),
            make_daily_group(
                yesterday,
                vec![make_session(
                    "~/projects/app",
                    Some("Fix login yesterday"),
                    None,
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("login"), None, Some(&today.to_string()), None);

        assert_eq!(json["count"], 1);
        assert_eq!(json["sessions"][0]["date"], today.to_string());
    }

    #[test]
    fn test_sessions_query_with_limit() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session("~/projects/app", Some("Fix bug one"), None),
                make_session("~/projects/app", Some("Fix bug two"), None),
                make_session("~/projects/app", Some("Fix bug three"), None),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("bug"), None, None, Some(2));

        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_server_info() {
        let server = CcsightServer::from_groups(vec![]);
        let info = server.get_info();

        assert_eq!(info.server_info.name, "ccsight");
        assert!(info.instructions.is_some());
        assert!(info.instructions.unwrap().contains("stats"));
    }

    #[test]
    fn test_session_detail_not_found() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_session_detail(&server, "nonexist", None);
        assert_eq!(json["error"], "Session not found");
    }

    #[test]
    fn test_session_detail_found_by_id() {
        let today = Local::now().date_naive();
        let mut session =
            make_session_with_tokens("~/projects/app", 5000, 2000, "claude-sonnet-4-20250514");
        session.file_path = std::path::PathBuf::from("/tmp/abcd1234-rest-of-id.jsonl");
        session.summary = Some("Test summary".to_string());
        session.day_tool_usage.insert("Read".to_string(), 5);
        session.day_tool_usage.insert("Edit".to_string(), 3);

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);
        let json = call_session_detail(&server, "abcd1234", None);

        assert!(json["error"].is_null(), "should find the session");
        assert_eq!(json["session"]["session_id"], "abcd1234");
        assert_eq!(json["session"]["project"], "~/projects/app");
        assert_eq!(json["tool_usage"]["Read"], 5);
        assert_eq!(json["tool_usage"]["Edit"], 3);
    }

    #[test]
    fn test_session_detail_with_date_filter() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let mut s1 =
            make_session_with_tokens("~/projects/a", 1000, 500, "claude-sonnet-4-20250514");
        s1.file_path = std::path::PathBuf::from("/tmp/sameid12-today.jsonl");
        let mut s2 =
            make_session_with_tokens("~/projects/b", 2000, 1000, "claude-sonnet-4-20250514");
        s2.file_path = std::path::PathBuf::from("/tmp/sameid12-yesterday.jsonl");

        let groups = vec![
            make_daily_group(today, vec![s1]),
            make_daily_group(yesterday, vec![s2]),
        ];
        let server = CcsightServer::from_groups(groups);

        let json = call_session_detail(&server, "sameid12", Some(&today.to_string()));
        assert_eq!(json["session"]["project"], "~/projects/a");

        let json2 = call_session_detail(&server, "sameid12", Some(&yesterday.to_string()));
        assert_eq!(json2["session"]["project"], "~/projects/b");
    }

    #[test]
    fn test_extract_conversation_basic() {
        let messages = vec![
            ConversationMessage {
                role: "user".to_string(),
                blocks: vec![ConversationBlock::Text(
                    "Please fix the login bug".to_string(),
                )],
                timestamp: Some("10:00:00".to_string()),
                model: None,
                tokens: None,
                timestamp_utc: None,
                usage: None,
            },
            ConversationMessage {
                role: "assistant".to_string(),
                blocks: vec![ConversationBlock::Text("I'll fix that.".to_string())],
                timestamp: Some("10:00:05".to_string()),
                model: Some("claude-sonnet-4".to_string()),
                tokens: Some((1000, 500)),
                timestamp_utc: None,
                usage: None,
            },
            ConversationMessage {
                role: "user".to_string(),
                blocks: vec![ConversationBlock::Text("Now add tests".to_string())],
                timestamp: Some("10:05:00".to_string()),
                model: None,
                tokens: None,
                timestamp_utc: None,
                usage: None,
            },
        ];
        let conv = extract_conversation(&messages);
        assert_eq!(conv.len(), 3);
        assert_eq!(conv[0]["role"], "user");
        assert!(conv[0]["text"].as_str().unwrap().contains("login bug"));
        assert_eq!(conv[1]["role"], "assistant");
        assert!(conv[1]["text"].as_str().unwrap().contains("fix that"));
        assert_eq!(conv[2]["role"], "user");
        assert!(conv[2]["text"].as_str().unwrap().contains("add tests"));
    }

    #[test]
    fn test_extract_conversation_includes_assistant() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Text("response text".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let conv = extract_conversation(&messages);
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0]["role"], "assistant");
    }

    #[test]
    fn test_extract_conversation_skips_tool_only_assistant() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::ToolUse {
                name: "Read".to_string(),
                input_summary: "/tmp/x".to_string(),
            }],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let conv = extract_conversation(&messages);
        assert!(
            conv.is_empty(),
            "tool-only assistant messages should be skipped"
        );
    }

    #[test]
    fn test_extract_conversation_truncates_long_text() {
        let long_text = "a".repeat(500);
        let messages = vec![ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::Text(long_text)],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let conv = extract_conversation(&messages);
        let text = conv[0]["text"].as_str().unwrap();
        assert!(text.len() <= 304);
        assert!(text.ends_with("..."));
    }

    #[test]
    fn test_extract_conversation_skips_empty() {
        let messages = vec![ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::ToolUse {
                name: "Read".to_string(),
                input_summary: "/tmp/x".to_string(),
            }],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let conv = extract_conversation(&messages);
        assert!(conv.is_empty());
    }

    #[test]
    fn test_extract_files_touched() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/src/main.rs".to_string(),
                },
                ConversationBlock::ToolUse {
                    name: "Edit".to_string(),
                    input_summary: "/src/lib.rs".to_string(),
                },
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/src/main.rs".to_string(),
                },
                ConversationBlock::ToolUse {
                    name: "Bash".to_string(),
                    input_summary: "cargo test".to_string(),
                },
            ],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let (files, total) = extract_files_touched(&messages);
        // Touch count orders the list, so a capped list keeps the files the
        // session worked on most.
        assert_eq!(files, vec!["/src/main.rs", "/src/lib.rs"]);
        assert_eq!(total, 2);
    }

    #[test]
    fn files_touched_drops_scratch_paths_and_caps_but_counts_every_file() {
        let mut blocks: Vec<ConversationBlock> = (0..MAX_FILES_TOUCHED + 5)
            .map(|i| ConversationBlock::ToolUse {
                name: "Edit".to_string(),
                input_summary: format!("/src/f{i:03}.rs"),
            })
            .collect();
        blocks.push(ConversationBlock::ToolUse {
            name: "Write".to_string(),
            input_summary: "/tmp/scratch.txt".to_string(),
        });
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks,
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let (files, total) = extract_files_touched(&messages);
        assert_eq!(files.len(), MAX_FILES_TOUCHED);
        assert_eq!(
            total,
            MAX_FILES_TOUCHED + 5,
            "the cap must not hide the count"
        );
        assert!(files.iter().all(|f| !f.starts_with("/tmp/")));
    }

    #[test]
    fn test_extract_files_touched_empty() {
        let messages = vec![ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Text("hello".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
            timestamp_utc: None,
            usage: None,
        }];
        let (files, total) = extract_files_touched(&messages);
        assert!(files.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn test_stats_custom_date_range_single_day() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats_range(
            &server,
            &yesterday.to_string(),
            Some(&yesterday.to_string()),
        );

        assert_eq!(json["period"], "custom");
        assert_eq!(json["total_sessions"], 1);
        assert_eq!(json["tokens"]["input"], 2000);
    }

    #[test]
    fn test_stats_custom_date_range_overrides_period() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let mut sa =
            make_session_with_tokens("~/projects/a", 1000, 500, "claude-sonnet-4-20250514");
        sa.file_path = std::path::PathBuf::from("/tmp/a.jsonl");
        let mut sb =
            make_session_with_tokens("~/projects/b", 2000, 1000, "claude-sonnet-4-20250514");
        sb.file_path = std::path::PathBuf::from("/tmp/b.jsonl");
        let groups = vec![
            make_daily_group(today, vec![sa]),
            make_daily_group(yesterday, vec![sb]),
        ];
        let server = CcsightServer::from_groups(groups);

        let params = StatsParams {
            period: Some("today".to_string()),
            date_from: Some(yesterday.to_string()),
            date_to: None,
            group_by: None,
            compact: None,
        };
        let json = extract_json(&server.stats(Parameters(params)).unwrap());

        assert_eq!(json["period"], "custom");
        assert_eq!(
            json["total_sessions"], 2,
            "date_from should override period"
        );
    }

    #[test]
    fn test_stats_custom_date_from_only() {
        let today = Local::now().date_naive();
        let old = today - chrono::Duration::days(30);
        let groups = vec![
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                old,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let json = call_stats_range(&server, &today.to_string(), None);

        assert_eq!(json["total_sessions"], 1, "only today should be included");
    }

    #[test]
    fn test_derive_summary_from_file() {
        let tmp = TempFile::new("ccsight_derive_summary.jsonl");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "user", "content": "Add MCP server support to the project"}
        })]);
        assert_eq!(
            derive_summary(tmp.path()).as_deref(),
            Some("Add MCP server support to the project")
        );
    }

    #[test]
    fn test_derive_summary_skips_system_tags() {
        let tmp = TempFile::new("ccsight_derive_skip_tags.jsonl");
        tmp.write_jsonl(&[
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:00:00Z",
                "message": {"role": "user", "content": "<local-command-caveat>system noise</local-command-caveat>"}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-03-20T10:01:00Z",
                "message": {"role": "user", "content": "Fix the login bug"}
            }),
        ]);
        assert_eq!(
            derive_summary(tmp.path()).as_deref(),
            Some("Fix the login bug")
        );
    }

    #[test]
    fn test_derive_summary_no_user_message() {
        let tmp = TempFile::new("ccsight_derive_no_user.jsonl");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "assistant", "content": "Hello"}
        })]);
        assert!(derive_summary(tmp.path()).is_none());
    }

    #[test]
    fn test_derive_summary_truncates_long() {
        let tmp = TempFile::new("ccsight_derive_long.jsonl");
        let long = "x".repeat(300);
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "user", "content": long}
        })]);
        let result = derive_summary(tmp.path()).unwrap();
        assert!(result.len() <= 124);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_sessions_date_from_to() {
        let today = Local::now().date_naive();
        let d1 = today - chrono::Duration::days(3);
        let d2 = today - chrono::Duration::days(2);
        let d3 = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                d1,
                vec![make_session_with_tokens(
                    "~/projects/a",
                    1000,
                    500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                d2,
                vec![make_session_with_tokens(
                    "~/projects/b",
                    2000,
                    1000,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                d3,
                vec![make_session_with_tokens(
                    "~/projects/c",
                    3000,
                    1500,
                    "claude-sonnet-4-20250514",
                )],
            ),
            make_daily_group(
                today,
                vec![make_session_with_tokens(
                    "~/projects/d",
                    4000,
                    2000,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let params = SessionsParams {
            session_id: None,
            query: None,
            project: None,
            date: None,
            date_from: Some(d2.to_string()),
            date_to: Some(d3.to_string()),
            sort: None,
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());
        assert_eq!(json["count"], 2);
    }

    #[test]
    fn test_sessions_sort_by_cost() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/cheap", 1000, 500, "claude-sonnet-4-20250514"),
                make_session_with_tokens(
                    "~/projects/expensive",
                    100_000,
                    50_000,
                    "claude-opus-4-5-20251101",
                ),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let params = SessionsParams {
            session_id: None,
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: Some("cost".to_string()),
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());
        let sessions = json["sessions"].as_array().unwrap();
        assert!(
            sessions[0]["cost_usd"].as_f64().unwrap() > sessions[1]["cost_usd"].as_f64().unwrap()
        );
    }

    #[test]
    fn test_sessions_sort_by_tokens() {
        let today = Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/small", 1000, 500, "claude-sonnet-4-20250514"),
                make_session_with_tokens(
                    "~/projects/large",
                    100_000,
                    50_000,
                    "claude-sonnet-4-20250514",
                ),
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let params = SessionsParams {
            session_id: None,
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: Some("tokens".to_string()),
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());
        let sessions = json["sessions"].as_array().unwrap();
        let t0 = sessions[0]["tokens"]["input"].as_u64().unwrap()
            + sessions[0]["tokens"]["output"].as_u64().unwrap();
        let t1 = sessions[1]["tokens"]["input"].as_u64().unwrap()
            + sessions[1]["tokens"]["output"].as_u64().unwrap();
        assert!(t0 > t1);
    }

    #[test]
    fn test_session_detail_conversation_truncated() {
        let today = Local::now().date_naive();
        let mut session = make_session("~/projects/app", Some("Many messages"), None);

        let tmp = TempFile::new("ccsight_truncation.jsonl");
        let entries: Vec<serde_json::Value> = (0..30)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "timestamp": format!("2026-03-20T10:{i:02}:00Z"),
                    "message": {"role": "user", "content": format!("request {i}")}
                })
            })
            .collect();
        tmp.write_jsonl(&entries);
        session.file_path = tmp.path().to_path_buf();

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);

        let params = SessionsParams {
            session_id: Some("ccsight_t".to_string()),
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: None,
            limit: None,
            conversation_limit: Some(15),
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());

        assert_eq!(json["conversation_total"], 30);
        let conv = json["conversation"].as_array().unwrap();
        assert!(conv.len() <= 16, "should be head + omission marker + tail");
        assert_eq!(conv[0]["text"].as_str().unwrap(), "request 0");
        assert!(
            conv.iter().any(|c| c["role"] == "..."),
            "should have omission marker"
        );
        let last = conv.last().unwrap();
        assert_eq!(last["text"].as_str().unwrap(), "request 29");
    }

    #[test]
    fn search_reports_the_match_count_separately_from_the_returned_page() {
        let today = Local::now().date_naive();
        // Distinct branches keep the fixture file paths distinct; search
        // dedupes by path, so same-path sessions would collapse into one.
        let sessions: Vec<_> = (0..7)
            .map(|i| {
                make_session(
                    "~/projects/app",
                    Some(&format!("widget note {i}")),
                    Some(&format!("b{i}")),
                )
            })
            .collect();
        let server = CcsightServer::from_groups(vec![make_daily_group(today, sessions)]);

        let json = extract_json(
            &server
                .search(Parameters(SearchParams {
                    query: "widget".to_string(),
                    project: None,
                    date_from: None,
                    date_to: None,
                    limit: Some(2),
                }))
                .unwrap(),
        );

        // A caller cannot tell a narrow page from an exhausted search unless
        // the two counts are reported apart.
        assert_eq!(json["total_results"], 2);
        assert_eq!(json["matched_total"], 7);
        // A server over fixed groups must not touch the on-disk index: a
        // refresh from here would rewrite the developer's own index to
        // these seven fixture sessions.
        assert_eq!(json["content_index"], "unavailable: metadata-only results");
    }

    fn detail_params(id: &str, query: Option<&str>, full_text: bool) -> SessionsParams {
        SessionsParams {
            session_id: Some(id.to_string()),
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: None,
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: query.map(str::to_string),
            full_text: Some(full_text),
            pinned: None,
        }
    }

    /// A server over one session whose single long message hides `NEEDLE`
    /// far past any preview cut.
    fn needle_server(tmp: &TempFile) -> (CcsightServer, usize) {
        let long = format!("{}NEEDLE{}", "x".repeat(5000), "y".repeat(5000));
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T12:00:00Z",
            "message": {"role": "user", "content": long}
        })]);
        let mut session = make_session("~/projects/app", Some("Long"), None);
        session.file_path = tmp.path().to_path_buf();
        let today = Local::now().date_naive();
        let server = CcsightServer::from_groups(vec![make_daily_group(today, vec![session])]);
        (server, long.chars().count())
    }

    #[test]
    fn conversation_query_finds_a_term_past_the_preview_and_returns_it() {
        let tmp = TempFile::new("ccsight_needle.jsonl");
        let (server, full_len) = needle_server(&tmp);
        let json = extract_json(
            &server
                .sessions(Parameters(detail_params(
                    "ccsight_n",
                    Some("needle"),
                    false,
                )))
                .unwrap(),
        );
        let conv = json["conversation"].as_array().unwrap();
        let hit = conv.iter().find(|c| c["match"] == true).expect("no match");
        let text = hit["text"].as_str().unwrap();
        // The window is what the caller reads, so it must hold the term.
        assert!(text.contains("NEEDLE"), "window missed the term");
        assert!(text.chars().count() < full_len);
        assert_eq!(hit["text_truncated"], true);
    }

    #[test]
    fn full_text_returns_the_whole_message() {
        let tmp = TempFile::new("ccsight_fulltext.jsonl");
        let (server, full_len) = needle_server(&tmp);
        let json = extract_json(
            &server
                .sessions(Parameters(detail_params("ccsight_f", Some("needle"), true)))
                .unwrap(),
        );
        let hit = &json["conversation"].as_array().unwrap()[0];
        assert_eq!(hit["text"].as_str().unwrap().chars().count(), full_len);
        assert!(hit.get("text_truncated").is_none());
    }

    #[test]
    fn conversation_times_carry_the_date() {
        let tmp = TempFile::new("ccsight_dated.jsonl");
        let (server, _) = needle_server(&tmp);
        let json = extract_json(
            &server
                .sessions(Parameters(detail_params("ccsight_d", None, false)))
                .unwrap(),
        );
        let time = json["conversation"][0]["time"].as_str().unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-03-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Local)
            .date_naive()
            .to_string();
        assert!(time.starts_with(&expected), "undated time {time:?}");
    }

    #[test]
    fn index_hit_positions_list_every_day_newest_first_without_subagents() {
        let newer = Local::now().date_naive();
        let older = newer - chrono::Duration::days(1);
        let with_path = |p: &str, sub: bool| {
            let mut s = make_session("proj", None, None);
            s.file_path = PathBuf::from(p);
            s.is_subagent = sub;
            s
        };
        let groups = vec![
            make_daily_group(
                newer,
                vec![with_path("/s/sub", true), with_path("/s/multi", false)],
            ),
            make_daily_group(
                older,
                vec![with_path("/s/old", false), with_path("/s/multi", false)],
            ),
        ];
        let pos = positions_by_path(&groups);
        // Subagents are not counted, so the user session is index 0 on the
        // newer day; newest day first, so a range filter can pick the newest
        // day it covers.
        assert_eq!(pos[std::path::Path::new("/s/multi")], vec![(0, 0), (1, 1)]);
        assert_eq!(pos[std::path::Path::new("/s/old")], vec![(1, 0)]);
        assert!(!pos.contains_key(std::path::Path::new("/s/sub")));
    }

    #[test]
    fn an_index_hit_lands_on_the_newest_day_inside_the_range() {
        let days = [(0, 2), (3, 1), (7, 0)];
        assert_eq!(newest_in_range(&days, |_| true), Some((0, 2)));
        // Day 0 is past `date_to`; the session was still active in range.
        assert_eq!(newest_in_range(&days, |d| d >= 3), Some((3, 1)));
        assert_eq!(newest_in_range(&days, |d| d > 7), None);
    }

    #[test]
    fn a_catch_up_waits_out_the_cool_down_after_the_last_one() {
        let done = 1_000_000;
        assert!(!cooled_down(done, done + CATCH_UP_COOL_DOWN_SECS - 1));
        assert!(cooled_down(done, done + CATCH_UP_COOL_DOWN_SECS));
        assert!(
            cooled_down(0, done),
            "a server that never refreshed may start"
        );
    }

    #[test]
    fn sessions_query_still_matches_the_first_prompt_of_a_titled_session() {
        let mut s = make_session("~/projects/app", Some("fix the flaky widget test"), None);
        s.ai_title = Some("Test stabilisation".to_string());
        let server =
            CcsightServer::from_groups(vec![make_daily_group(Local::now().date_naive(), vec![s])]);
        let json = call_sessions(&server, Some("widget"), None, None, None);
        assert_eq!(json["count"], 1, "the prompt's words must stay searchable");
        assert_eq!(json["sessions"][0]["summary"], "Test stabilisation");
    }

    #[test]
    fn a_multi_day_session_row_carries_the_whole_session_total() {
        let newer = Local::now().date_naive();
        let older = newer - chrono::Duration::days(3);
        let mk = |input| {
            let mut s = make_session_with_tokens("proj", input, 0, "claude-opus-5");
            s.file_path = PathBuf::from("/tmp/test/multi.jsonl");
            s
        };
        let groups = vec![
            make_daily_group(newer, vec![mk(1_000_000)]),
            make_daily_group(older, vec![mk(3_000_000)]),
        ];
        let totals = crate::aggregator::cumulative_by_path(&groups);
        let newest = &groups[0].sessions[0];
        let row = build_session_json(
            newer,
            newest,
            CostCalculator::global(),
            &HashMap::new(),
            totals.get(newest.file_path.as_path()),
        );
        // The row's own figure is the one day; the total spans both.
        assert_eq!(row["cost_usd"], 5.0);
        assert_eq!(row["session_total"]["cost_usd"], 20.0);
        assert_eq!(row["session_total"]["active_days"], 2);
        assert_eq!(row["session_total"]["tokens"]["input"], 4_000_000);

        let single = vec![make_daily_group(newer, vec![mk(1_000_000)])];
        let totals = crate::aggregator::cumulative_by_path(&single);
        let s = &single[0].sessions[0];
        let row = build_session_json(
            newer,
            s,
            CostCalculator::global(),
            &HashMap::new(),
            totals.get(s.file_path.as_path()),
        );
        assert!(
            row.get("session_total").is_none(),
            "a one-day session repeats its own row"
        );
    }

    #[test]
    fn session_detail_bounds_the_conversation_without_an_explicit_limit() {
        let today = Local::now().date_naive();
        let mut session = make_session("~/projects/app", Some("Many messages"), None);

        let tmp = TempFile::new("ccsight_default_window.jsonl");
        let entries: Vec<serde_json::Value> = (0..DEFAULT_CONVERSATION_ENTRIES * 3)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "timestamp": format!("2026-03-20T{:02}:{:02}:00Z", 10 + i / 60, i % 60),
                    "message": {"role": "user", "content": format!("request {i}")}
                })
            })
            .collect();
        tmp.write_jsonl(&entries);
        session.file_path = tmp.path().to_path_buf();

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);

        let params = SessionsParams {
            session_id: Some("ccsight_d".to_string()),
            query: None,
            project: None,
            date: None,
            date_from: None,
            date_to: None,
            sort: None,
            limit: None,
            conversation_limit: None,
            conversation_offset: None,
            conversation_query: None,
            full_text: None,
            pinned: None,
        };
        let json = extract_json(&server.sessions(Parameters(params)).unwrap());

        // A caller that names no limit still must not be handed the whole
        // transcript: the default window is what keeps one call affordable.
        assert_eq!(json["conversation_total"], DEFAULT_CONVERSATION_ENTRIES * 3);
        let conv = json["conversation"].as_array().unwrap();
        assert!(
            conv.len() <= DEFAULT_CONVERSATION_ENTRIES + 1,
            "default must bound the window, got {}",
            conv.len()
        );
        assert_eq!(conv[0]["text"].as_str().unwrap(), "request 0");
        assert!(
            conv.iter().any(|c| c["role"] == "..."),
            "should have omission marker"
        );
        let last = conv.last().unwrap();
        let total = DEFAULT_CONVERSATION_ENTRIES * 3;
        assert_eq!(
            last["text"].as_str().unwrap(),
            format!("request {}", total - 1)
        );
    }

    #[test]
    fn test_query_searches_derived_summary() {
        let today = Local::now().date_naive();
        let mut session = make_session("~/projects/app", None, None);
        session.summary = None;

        let tmp = TempFile::new("ccsight_query_summary.jsonl");
        tmp.write_jsonl(&[serde_json::json!({
            "type": "user",
            "timestamp": "2026-03-20T10:00:00Z",
            "message": {"role": "user", "content": "Fix the pagination bug in user list"}
        })]);
        session.file_path = tmp.path().to_path_buf();

        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);
        let json = call_sessions(&server, Some("pagination"), None, None, None);

        assert_eq!(json["count"], 1, "query should match derived summary");
    }

    #[test]
    fn test_clean_summary_extracts_plan_title() {
        let text = "Implement the following plan:\n\n# Fix user authentication flow\n\n## Context\nsome details";
        assert_eq!(clean_summary(text), "Fix user authentication flow");
    }

    #[test]
    fn test_clean_summary_plain_text() {
        assert_eq!(clean_summary("Fix the login bug"), "Fix the login bug");
    }

    #[test]
    fn test_clean_summary_truncates() {
        let long = "x".repeat(200);
        let result = clean_summary(&long);
        assert!(result.len() <= 124);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_stats_group_by_day() {
        let today = Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![
                    make_session_with_tokens("~/projects/a", 1000, 500, "claude-sonnet-4-20250514"),
                    make_session_with_tokens(
                        "~/projects/b",
                        2000,
                        1000,
                        "claude-sonnet-4-20250514",
                    ),
                ],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens(
                    "~/projects/c",
                    3000,
                    1500,
                    "claude-sonnet-4-20250514",
                )],
            ),
        ];
        let server = CcsightServer::from_groups(groups);
        let params = StatsParams {
            period: None,
            date_from: None,
            date_to: None,
            group_by: Some("day".to_string()),
            compact: None,
        };
        let json = extract_json(&server.stats(Parameters(params)).unwrap());

        let daily = json["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 2);
        assert_eq!(daily[0]["date"].as_str().unwrap(), yesterday.to_string());
        assert_eq!(daily[1]["date"].as_str().unwrap(), today.to_string());
        assert_eq!(daily[1]["sessions"], 2);
        assert!(daily[1]["cost_usd"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn test_stats_daily_and_projects_cost_include_subagents_and_reconcile() {
        // daily[].cost_usd and projects[].cost_usd must include subagent spend
        // so they sum to total_cost_usd (which always does) — matching --daily
        // and the TUI project_stats. Regression: the subagent skip used to sit
        // above these accumulators, undercounting both.
        let today = Local::now().date_naive();
        let mut sub =
            make_session_with_tokens("~/projects/a", 5000, 2000, "claude-sonnet-4-20250514");
        sub.is_subagent = true;
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/a", 1000, 500, "claude-sonnet-4-20250514"),
                sub,
            ],
        )];
        let server = CcsightServer::from_groups(groups);
        let json = extract_json(
            &server
                .stats(Parameters(StatsParams {
                    period: None,
                    date_from: None,
                    date_to: None,
                    group_by: Some("day".to_string()),
                    compact: Some(false),
                }))
                .unwrap(),
        );
        let total = json["total_cost_usd"].as_f64().unwrap();
        let sum_daily: f64 = json["daily"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["cost_usd"].as_f64().unwrap())
            .sum();
        let sum_proj: f64 = json["projects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["cost_usd"].as_f64().unwrap())
            .sum();
        assert!(total > 0.0);
        assert!(
            (total - sum_daily).abs() < 0.01,
            "daily cost must reconcile with total (subagent-inclusive): total={total} sum_daily={sum_daily}"
        );
        assert!(
            (total - sum_proj).abs() < 0.01,
            "projects cost must reconcile with total (subagent-inclusive): total={total} sum_proj={sum_proj}"
        );
    }

    #[test]
    fn test_stats_without_group_by_has_no_daily() {
        let server = CcsightServer::from_groups(make_test_groups());
        let json = call_stats(&server, None);
        assert!(json.get("daily").is_none());
    }

    #[test]
    fn test_sessions_query_matches_metadata_only() {
        let today = Local::now().date_naive();
        let session = make_session(
            "~/projects/app",
            Some("Setup database connection pool"),
            None,
        );
        let groups = vec![make_daily_group(today, vec![session])];
        let server = CcsightServer::from_groups(groups);

        let json = call_sessions(&server, Some("connection pool"), None, None, None);
        assert_eq!(json["count"], 1, "should find session by summary metadata");

        let json2 = call_sessions(&server, Some("nonexistent_xyz"), None, None, None);
        assert_eq!(
            json2["count"], 0,
            "should not match content that is only in conversation"
        );
    }
}
