use std::collections::HashSet;
use std::path::Path;

use chrono::NaiveDate;

use crate::aggregator::DailyGroup;
use crate::domain::{EntryType, Role};
use crate::parser::JsonlParser;

/// Inline filter tokens parsed out of the search query. Live state /
/// period filters are enum-valued (only one of each can be active); the
/// property filters (project / model / branch) are substring needles.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SearchFilters {
    pub state: Option<SessionStateFilter>,
    pub period: Option<PeriodFilter>,
    pub project: Option<String>,
    pub model: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStateFilter {
    Live,
    Paused,
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodFilter {
    Today,
    Week,
    Month,
    /// Exact-date pinpoint, used by `filter:date:YYYY-MM-DD`.
    On(NaiveDate),
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.period.is_none()
            && self.project.is_none()
            && self.model.is_none()
            && self.branch.is_none()
    }
}

/// Context the filter step needs that lives outside `daily_groups` — the
/// in-memory live/paused session lists (joined by `file_path`) and today's
/// local calendar date for the period filters.
pub struct SearchFiltersContext<'a> {
    pub today: NaiveDate,
    pub live_paths: &'a HashSet<std::path::PathBuf>,
    pub busy_paths: &'a HashSet<std::path::PathBuf>,
    pub paused_paths: &'a HashSet<std::path::PathBuf>,
}

/// Strip `filter:KEY` / `project:X` / `model:X` / `branch:X` tokens out of
/// the raw query and return the remaining free text. Unknown tokens fall
/// through as free text so users typing a colon mid-word aren't punished.
pub fn parse_search_query(input: &str) -> (SearchFilters, String) {
    let mut filters = SearchFilters::default();
    let mut free_tokens: Vec<&str> = Vec::new();
    for token in input.split_whitespace() {
        if let Some((key, value)) = token.split_once(':') {
            let key_lower = key.to_ascii_lowercase();
            let value_lower = value.to_ascii_lowercase();
            match key_lower.as_str() {
                "filter" => match value_lower.as_str() {
                    "live" => filters.state = Some(SessionStateFilter::Live),
                    "paused" => filters.state = Some(SessionStateFilter::Paused),
                    "busy" => filters.state = Some(SessionStateFilter::Busy),
                    "today" => filters.period = Some(PeriodFilter::Today),
                    "week" => filters.period = Some(PeriodFilter::Week),
                    "month" => filters.period = Some(PeriodFilter::Month),
                    // `filter:date:YYYY-MM-DD` — value carries the
                    // sub-prefix `date:` then the parseable date string.
                    v if v.starts_with("date:") => {
                        let raw = &v["date:".len()..];
                        if let Ok(d) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
                            filters.period = Some(PeriodFilter::On(d));
                        } else {
                            free_tokens.push(token);
                        }
                    }
                    _ => free_tokens.push(token),
                },
                "project" if !value.is_empty() => filters.project = Some(value.to_string()),
                "model" if !value.is_empty() => filters.model = Some(value.to_string()),
                "branch" if !value.is_empty() => filters.branch = Some(value.to_string()),
                _ => free_tokens.push(token),
            }
        } else {
            free_tokens.push(token);
        }
    }
    (filters, free_tokens.join(" "))
}

/// Public re-export of `period_matches` for callers that already have the
/// remapped `(day_idx, session_idx)` and just want to apply the filter
/// step (e.g. the tantivy-result poll in `main.rs`).
pub fn period_matches_for_filter(
    day: NaiveDate,
    period: Option<PeriodFilter>,
    today: NaiveDate,
) -> bool {
    period_matches(day, period, today)
}

/// Public re-export of the per-session filter predicate. Same caveat as
/// `period_matches_for_filter`.
pub fn session_passes_filters(
    session: &crate::aggregator::SessionInfo,
    filters: &SearchFilters,
    ctx: &SearchFiltersContext,
) -> bool {
    session_matches_filters(session, filters, ctx)
}

/// True when the day satisfies the period filter. `None` = pass.
fn period_matches(day: NaiveDate, period: Option<PeriodFilter>, today: NaiveDate) -> bool {
    match period {
        None => true,
        Some(PeriodFilter::Today) => day == today,
        Some(PeriodFilter::Week) => {
            let diff = (today - day).num_days();
            (0..=6).contains(&diff)
        }
        Some(PeriodFilter::Month) => {
            let diff = (today - day).num_days();
            (0..=29).contains(&diff)
        }
        Some(PeriodFilter::On(target)) => day == target,
    }
}

fn session_matches_filters(
    session: &crate::aggregator::SessionInfo,
    filters: &SearchFilters,
    ctx: &SearchFiltersContext,
) -> bool {
    if let Some(state) = filters.state {
        let path = &session.file_path;
        let ok = match state {
            SessionStateFilter::Live => ctx.live_paths.contains(path),
            SessionStateFilter::Busy => ctx.busy_paths.contains(path),
            SessionStateFilter::Paused => ctx.paused_paths.contains(path),
        };
        if !ok {
            return false;
        }
    }
    if let Some(p) = &filters.project {
        let needle = p.to_lowercase();
        if !session.project_name.to_lowercase().contains(&needle) {
            return false;
        }
    }
    if let Some(m) = &filters.model {
        let needle = m.to_lowercase();
        let hit = session
            .model
            .as_ref()
            .is_some_and(|s| s.to_lowercase().contains(&needle));
        if !hit {
            return false;
        }
    }
    if let Some(b) = &filters.branch {
        let needle = b.to_lowercase();
        let hit = session
            .git_branch
            .as_ref()
            .is_some_and(|s| s.to_lowercase().contains(&needle));
        if !hit {
            return false;
        }
    }
    true
}

/// Index hits fetched when filters run after the index answers — a narrow
/// filter would otherwise starve on a small fetch, and one busy session's
/// hits collapse into a single result.
pub(crate) const FILTERED_INDEX_FETCH: usize = 2000;

/// Inclusive date bounds of a search period — the same days `period_matches`
/// accepts, so the index query and the post-filter agree.
pub(crate) fn period_bounds(
    period: Option<PeriodFilter>,
    today: NaiveDate,
) -> (Option<NaiveDate>, Option<NaiveDate>) {
    match period {
        None => (None, None),
        Some(PeriodFilter::Today) => (Some(today), Some(today)),
        Some(PeriodFilter::Week) => (Some(today - chrono::Duration::days(6)), Some(today)),
        Some(PeriodFilter::Month) => (Some(today - chrono::Duration::days(29)), Some(today)),
        Some(PeriodFilter::On(d)) => (Some(d), Some(d)),
    }
}

/// Narrowest of two inclusive ranges; either side `None` = unbounded.
pub(crate) fn intersect_bounds(
    a: (Option<NaiveDate>, Option<NaiveDate>),
    b: (Option<NaiveDate>, Option<NaiveDate>),
) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let pick =
        |x: Option<NaiveDate>, y: Option<NaiveDate>, f: fn(NaiveDate, NaiveDate) -> NaiveDate| {
            match (x, y) {
                (Some(x), Some(y)) => Some(f(x, y)),
                (x, y) => x.or(y),
            }
        };
    (pick(a.0, b.0, std::cmp::max), pick(a.1, b.1, std::cmp::min))
}

/// The slice a content hit opens: the day its message was written, else the
/// newest in-range day (a hit that carries no message date).
pub(crate) fn hit_position(
    days: &[(usize, usize)],
    in_range: impl Fn(usize) -> bool,
    is_message_day: impl Fn(usize) -> bool,
) -> Option<(usize, usize)> {
    days.iter()
        .copied()
        .find(|(d, _)| in_range(*d) && is_message_day(*d))
        .or_else(|| newest_in_range(days, in_range))
}

/// First `(day_idx, session_idx)` whose day passes `in_range`; `days` runs
/// newest first. An index hit names a session, not a day, so a multi-day
/// session must land on a day the active period covers — its newest one —
/// or a period filter drops a session it should keep.
pub(crate) fn newest_in_range(
    days: &[(usize, usize)],
    in_range: impl Fn(usize) -> bool,
) -> Option<(usize, usize)> {
    days.iter().copied().find(|(d, _)| in_range(*d))
}

#[derive(Clone)]
pub struct SearchResult {
    /// Position in `daily_groups` that the result currently refers to. The
    /// tantivy index uses positions captured at build time; when a project /
    /// period filter shrinks `daily_groups`, callers must remap these indices
    /// (using `session_path` as the stable lookup key) before pushing into
    /// `state.search_results`. Stale indices crash the renderer.
    pub day_idx: usize,
    pub session_idx: usize,
    pub snippet: Option<String>,
    pub match_type: SearchMatchType,
    /// Absolute session-file path stored in the search index. Used as the
    /// stable identifier for remapping `day_idx` / `session_idx` after
    /// filter changes. `None` for non-index searches (where indices already
    /// reflect the current filtered set).
    pub session_path: Option<String>,
    /// Local date the matched message was written; `None` for matches that
    /// aren't a single message (metadata, file scan).
    pub message_date: Option<NaiveDate>,
}

#[derive(Clone)]
pub enum SearchMatchType {
    ProjectName,
    Title,
    Summary,
    GitBranch,
    SessionId,
    Date,
    Content,
}

impl SearchMatchType {
    /// Where a hit ranks: one naming the session first, then a message's
    /// content, then a hit on an attribute alone (`project:` filters those).
    pub fn tier(&self) -> u8 {
        match self {
            Self::Title | Self::Summary | Self::SessionId => 0,
            Self::Content => 1,
            Self::ProjectName | Self::GitBranch | Self::Date => 2,
        }
    }
}

/// Merges content hits into a tier-ordered metadata list: the hits go after
/// the tier-0 rows, and a session listed only on an attribute moves up to
/// its content hit. `path_of` names each row's session file.
pub fn merge_content_hits(
    results: &mut Vec<SearchResult>,
    hits: Vec<SearchResult>,
    path_of: impl Fn(&SearchResult) -> Option<std::path::PathBuf>,
) {
    let named: HashSet<std::path::PathBuf> = results
        .iter()
        .filter(|r| r.match_type.tier() == 0)
        .filter_map(&path_of)
        .collect();
    let mut taken = HashSet::new();
    let hits: Vec<SearchResult> = hits
        .into_iter()
        .filter(|h| path_of(h).is_some_and(|p| !named.contains(&p) && taken.insert(p)))
        .collect();
    results.retain(|r| r.match_type.tier() < 2 || path_of(r).is_none_or(|p| !taken.contains(&p)));
    let at = results
        .iter()
        .position(|r| r.match_type.tier() == 2)
        .unwrap_or(results.len());
    results.splice(at..at, hits);
}

pub fn perform_search(
    daily_groups: &[DailyGroup],
    query: &str,
    ctx: &SearchFiltersContext,
) -> Vec<SearchResult> {
    let (filters, free_text) = parse_search_query(query);
    if filters.is_empty() && free_text.is_empty() {
        return Vec::new();
    }

    let query_lower = free_text.to_lowercase();
    let no_text = free_text.is_empty();
    let mut results = Vec::new();
    // Dedupe by file_path; daily_groups is newest-first so first hit =
    // most-recent day, matching user intent ("the live session").
    let mut seen: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();

    for (day_idx, group) in daily_groups.iter().enumerate() {
        if !period_matches(group.date, filters.period, ctx.today) {
            continue;
        }
        for (session_idx, session) in group.sessions.iter().filter(|s| !s.is_subagent).enumerate() {
            if !session_matches_filters(session, &filters, ctx) {
                continue;
            }
            if !seen.insert(session.file_path.clone()) {
                continue;
            }
            // Filter-only query (no free text): include every session that
            // passed the filter step, newest-first via the existing
            // daily_groups order.
            if no_text {
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::ProjectName,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }
            // Line 1 already shows the title, so its hit needs no snippet.
            if session
                .custom_title
                .as_deref()
                .or(session.ai_title.as_deref())
                .is_some_and(|t| t.to_lowercase().contains(&query_lower))
            {
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::Title,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }

            if let Some(ref summary) = session.summary
                && summary.to_lowercase().contains(&query_lower)
            {
                let snippet = extract_snippet(summary, &query_lower, 60);
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: Some(snippet),
                    match_type: SearchMatchType::Summary,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }

            if let Some(session_id) = session.file_path.file_stem().and_then(|n| n.to_str())
                && session_id.to_lowercase().contains(&query_lower)
            {
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: Some(session_id.to_string()),
                    match_type: SearchMatchType::SessionId,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }

            if session.project_name.to_lowercase().contains(&query_lower) {
                // The project column is already on line 1, so a snippet that
                // re-prints the project name would just duplicate it. Set
                // None and let the renderer collapse the row to a single line.
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::ProjectName,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }

            if let Some(ref branch) = session.git_branch
                && branch.to_lowercase().contains(&query_lower)
            {
                // Branch column is already on line 1; same rationale as
                // ProjectName above.
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::GitBranch,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }

            if group
                .date
                .format("%Y-%m-%d")
                .to_string()
                .contains(&query_lower)
            {
                results.push(SearchResult {
                    day_idx,
                    session_idx,
                    snippet: None,
                    match_type: SearchMatchType::Date,
                    session_path: None,
                    message_date: None,
                });
                continue;
            }
        }
    }

    // Stable: each tier keeps the newest-first order the walk produced.
    results.sort_by_key(|r| r.match_type.tier());
    results
}

pub fn extract_snippet(text: &str, query: &str, max_len: usize) -> String {
    if query.is_empty() {
        return text.chars().take(max_len).collect();
    }

    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();

    if let Some(byte_pos) = text_lower.find(&query_lower) {
        let chars: Vec<char> = text.chars().collect();
        let char_pos = text[..byte_pos].chars().count();
        let query_len = query.chars().count();

        let start = char_pos.saturating_sub(max_len / 2);
        let end = (char_pos + query_len + max_len / 2).min(chars.len());

        let mut snippet = String::new();
        if start > 0 {
            snippet.push_str("...");
        }
        snippet.extend(&chars[start..end]);
        if end < chars.len() {
            snippet.push_str("...");
        }
        snippet.replace('\n', " ")
    } else {
        text.chars().take(max_len).collect()
    }
}

pub fn search_session_content(file_path: &Path, query: &str) -> Option<String> {
    let entries = JsonlParser::parse_file(file_path).ok()?;
    let query_lower = query.to_lowercase();

    for entry in &entries {
        if (entry.entry_type == EntryType::User || entry.entry_type == EntryType::Assistant)
            && let Some(ref message) = entry.message
        {
            let text = message.content.extract_text();
            let text_lower = text.to_lowercase();
            if text_lower.contains(&query_lower) {
                let role = match message.role {
                    Role::User => "User",
                    Role::Assistant => "AI",
                    _ => "?",
                };
                let snippet = extract_snippet(&text, query, 50);
                return Some(format!("[{role}] {snippet}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_snippet_empty_query() {
        let result = extract_snippet("hello world", "", 20);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_extract_snippet_empty_text() {
        let result = extract_snippet("", "query", 20);
        assert_eq!(result, "");
    }

    #[test]
    fn test_extract_snippet_query_longer_than_text() {
        let result = extract_snippet("hi", "hello world", 20);
        assert_eq!(result, "hi");
    }

    #[test]
    fn test_extract_snippet_normal_match() {
        let result = extract_snippet("hello world test", "world", 30);
        assert!(result.contains("world"));
    }

    #[test]
    fn test_extract_snippet_case_insensitive() {
        let result = extract_snippet("Hello World", "world", 30);
        assert!(result.contains("World"));
    }

    #[test]
    fn test_extract_snippet_no_match() {
        let result = extract_snippet("hello world", "xyz", 20);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_extract_snippet_unicode() {
        let result = extract_snippet("こんにちは世界", "世界", 20);
        assert!(result.contains("世界"));
    }

    #[test]
    fn test_extract_snippet_unicode_long_text() {
        let text = "これは長い日本語のテキストです。検索クエリはここにあります。そして続きます。";
        let result = extract_snippet(text, "検索クエリ", 30);
        assert!(result.contains("検索クエリ"));
        assert!(result.contains("..."));
    }

    #[test]
    fn parse_filter_live() {
        let (f, free) = parse_search_query("filter:live");
        assert_eq!(f.state, Some(SessionStateFilter::Live));
        assert_eq!(free, "");
    }

    #[test]
    fn parse_filter_with_free_text() {
        let (f, free) = parse_search_query("filter:today mcp setup");
        assert_eq!(f.period, Some(PeriodFilter::Today));
        assert_eq!(free, "mcp setup");
    }

    #[test]
    fn parse_property_filters() {
        let (f, free) = parse_search_query("project:ccsight branch:main model:opus");
        assert_eq!(f.project.as_deref(), Some("ccsight"));
        assert_eq!(f.branch.as_deref(), Some("main"));
        assert_eq!(f.model.as_deref(), Some("opus"));
        assert_eq!(free, "");
    }

    #[test]
    fn parse_unknown_filter_value_falls_through() {
        let (f, free) = parse_search_query("filter:nonsense actual");
        assert!(f.is_empty());
        assert_eq!(free, "filter:nonsense actual");
    }

    #[test]
    fn parse_url_with_colon_kept_as_free_text() {
        let (f, free) = parse_search_query("https://example.com/path");
        assert!(f.is_empty());
        assert_eq!(free, "https://example.com/path");
    }

    #[test]
    fn test_extract_snippet_with_newlines() {
        let text = "line1\nline2\nline3 target line4\nline5";
        let result = extract_snippet(text, "target", 30);
        assert!(result.contains("target"));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn test_extract_snippet_mixed_unicode() {
        let text = "Hello こんにちは World 世界 Test";
        let result = extract_snippet(text, "世界", 20);
        assert!(result.contains("世界"));
    }

    #[test]
    fn test_extract_snippet_at_start() {
        let result = extract_snippet("target is at start of text", "target", 15);
        assert!(result.starts_with("target"));
    }

    #[test]
    fn test_extract_snippet_at_end() {
        let result = extract_snippet("text ends with target", "target", 15);
        assert!(result.ends_with("target"));
    }

    use crate::aggregator::DailyGroup;
    use crate::test_helpers::helpers::{make_daily_group, make_session};
    use chrono::NaiveDate;

    /// Default-empty filter context for legacy tests that don't exercise
    /// the new `filter:` / `project:` / etc. syntax. Today is fixed at the
    /// epoch so period filters never accidentally match a real date.
    fn empty_ctx() -> (
        std::collections::HashSet<std::path::PathBuf>,
        std::collections::HashSet<std::path::PathBuf>,
        std::collections::HashSet<std::path::PathBuf>,
    ) {
        (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        )
    }
    fn ctx<'a>(
        sets: &'a (
            std::collections::HashSet<std::path::PathBuf>,
            std::collections::HashSet<std::path::PathBuf>,
            std::collections::HashSet<std::path::PathBuf>,
        ),
    ) -> SearchFiltersContext<'a> {
        SearchFiltersContext {
            today: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(), // lint-ok: date-literal
            live_paths: &sets.0,
            busy_paths: &sets.1,
            paused_paths: &sets.2,
        }
    }

    fn make_groups() -> Vec<DailyGroup> {
        vec![
            make_daily_group(
                NaiveDate::from_ymd_opt(2026, 2, 24).unwrap(), // lint-ok: date-literal
                vec![
                    make_session(
                        "~/projects/app-a",
                        Some("Add project filter feature"),
                        Some("feature/project-filter"),
                    ),
                    make_session(
                        "~/projects/other-app",
                        Some("Fix login bug"),
                        Some("fix/login"),
                    ),
                ],
            ),
            make_daily_group(
                NaiveDate::from_ymd_opt(2026, 2, 25).unwrap(), // lint-ok: date-literal
                vec![make_session(
                    "~/projects/app-a",
                    Some("Refactor search module"),
                    None,
                )],
            ),
        ]
    }

    // A hit naming the session outranks one on an attribute alone, however
    // much newer the attribute hit is.
    #[test]
    fn a_title_hit_ranks_before_newer_project_name_hits() {
        let mut groups = make_groups();
        groups[1].sessions[0].custom_title = Some("app-a migration notes".to_string());
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "app-a", &__ctx);
        let kinds: Vec<u8> = results.iter().map(|r| r.match_type.tier()).collect();
        assert_eq!(kinds, [0, 2]);
        assert!(matches!(results[0].match_type, SearchMatchType::Title));
        assert_eq!(
            results[0].day_idx, 1,
            "the older day's titled session leads"
        );
    }

    // Content hits slot in after the named rows; a session listed only on an
    // attribute moves up to its content hit instead of appearing twice.
    #[test]
    fn content_hits_take_the_middle_tier_and_lift_attribute_rows() {
        let row = |day_idx: usize, match_type: SearchMatchType| SearchResult {
            day_idx,
            session_idx: 0,
            snippet: None,
            match_type,
            session_path: None,
            message_date: None,
        };
        let mut results = vec![
            row(0, SearchMatchType::Title),
            row(1, SearchMatchType::ProjectName),
            row(2, SearchMatchType::ProjectName),
        ];
        let hits = vec![
            row(2, SearchMatchType::Content),
            row(0, SearchMatchType::Content),
            row(3, SearchMatchType::Content),
            row(3, SearchMatchType::Content),
        ];
        let path_of = |r: &SearchResult| Some(std::path::PathBuf::from(r.day_idx.to_string()));
        merge_content_hits(&mut results, hits, path_of);
        let order: Vec<(usize, u8)> = results
            .iter()
            .map(|r| (r.day_idx, r.match_type.tier()))
            .collect();
        assert_eq!(order, [(0, 0), (2, 1), (3, 1), (1, 2)]);
    }

    #[test]
    fn test_search_empty_query_returns_empty() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "", &__ctx);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_by_project_name() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "app-a", &__ctx);
        assert_eq!(results.len(), 2);
        assert!(matches!(
            results[0].match_type,
            SearchMatchType::ProjectName
        ));
        assert_eq!(results[0].day_idx, 0);
        assert_eq!(results[0].session_idx, 0);
        assert_eq!(results[1].day_idx, 1);
        assert_eq!(results[1].session_idx, 0);
    }

    #[test]
    fn test_search_by_project_name_case_insensitive() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "APP-A", &__ctx);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_by_summary() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "login bug", &__ctx);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].match_type, SearchMatchType::Summary));
        assert_eq!(results[0].day_idx, 0);
        assert_eq!(results[0].session_idx, 1);
        assert!(results[0].snippet.as_ref().unwrap().contains("login bug"));
    }

    #[test]
    fn test_search_by_git_branch() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "feature/project", &__ctx);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].match_type, SearchMatchType::GitBranch));
        assert_eq!(results[0].session_idx, 0);
    }

    #[test]
    fn test_search_by_date() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "2026-02-25", &__ctx); // lint-ok: date-literal
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0].match_type, SearchMatchType::Date));
        assert_eq!(results[0].day_idx, 1);
    }

    #[test]
    fn test_search_by_partial_date() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "2026-02", &__ctx);
        assert_eq!(results.len(), 3);
        assert!(
            results
                .iter()
                .all(|r| matches!(r.match_type, SearchMatchType::Date))
        );
    }

    #[test]
    fn a_summary_hit_outranks_a_project_name_hit_on_the_same_session() {
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![make_session(
                "~/projects/myapp",
                Some("Working on myapp features"),
                None,
            )],
        )];
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "myapp", &__ctx);
        assert_eq!(results.len(), 1, "one row per session");
        assert!(
            matches!(results[0].match_type, SearchMatchType::Summary),
            "the stronger, session-naming hit is the one recorded"
        );
    }

    #[test]
    fn test_search_no_match() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "nonexistent-xyz", &__ctx);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_indices_correct_with_multiple_sessions() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "other-app", &__ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].day_idx, 0);
        assert_eq!(results[0].session_idx, 1);
    }

    #[test]
    fn test_search_content_match_does_not_duplicate_metadata_match() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let meta_results = perform_search(&groups, "app-a", &__ctx);
        assert_eq!(meta_results.len(), 2);
        let already_matched = meta_results
            .iter()
            .any(|r| r.day_idx == 0 && r.session_idx == 0);
        assert!(
            already_matched,
            "metadata search should find app-a at day 0 session 0"
        );
    }

    #[test]
    fn test_search_date_matches_all_sessions_in_day() {
        let groups = make_groups();
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "2026-02-24", &__ctx); // lint-ok: date-literal
        assert_eq!(
            results.len(),
            2,
            "all sessions in the matching day should be returned"
        );
        assert!(results.iter().all(|r| r.day_idx == 0));
        assert!(
            results
                .iter()
                .all(|r| matches!(r.match_type, SearchMatchType::Date))
        );
    }

    #[test]
    fn test_search_summary_snippet_is_reasonable_length() {
        let long_summary = "a".repeat(200) + " target " + &"b".repeat(200);
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![make_session("~/projects/x", Some(&long_summary), None)],
        )];
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "target", &__ctx);
        assert_eq!(results.len(), 1);
        let snippet = results[0].snippet.as_ref().unwrap();
        assert!(snippet.contains("target"));
        assert!(
            snippet.len() < long_summary.len(),
            "snippet should be shorter than full summary"
        );
    }

    #[test]
    fn test_search_session_idx_skips_subagents() {
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![
                {
                    let mut s = make_session("~/projects/sub", None, None);
                    s.is_subagent = true;
                    s
                },
                make_session("~/projects/main-project", None, None),
            ],
        )];
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "main-project", &__ctx);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].session_idx, 0,
            "session_idx should be filtered index, not raw index"
        );
    }

    #[test]
    fn test_search_subagent_sessions_excluded() {
        let groups = vec![make_daily_group(
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), // lint-ok: date-literal
            vec![{
                let mut s = make_session("~/projects/agent-task", None, None);
                s.is_subagent = true;
                s
            }],
        )];
        let __c = empty_ctx();
        let __ctx = ctx(&__c);
        let results = perform_search(&groups, "agent-task", &__ctx);
        assert_eq!(results.len(), 0, "subagent sessions should be excluded");
    }

    #[test]
    fn period_bounds_admit_exactly_the_days_period_matches_admits() {
        let today = chrono::Local::now().date_naive();
        let periods = [
            None,
            Some(PeriodFilter::Today),
            Some(PeriodFilter::Week),
            Some(PeriodFilter::Month),
            Some(PeriodFilter::On(today - chrono::Duration::days(3))),
        ];
        for period in periods {
            let (from, to) = period_bounds(period, today);
            for back in -2..40 {
                let day = today - chrono::Duration::days(back);
                let inside = from.is_none_or(|f| day >= f) && to.is_none_or(|t| day <= t);
                assert_eq!(
                    inside,
                    period_matches(day, period, today),
                    "{period:?} {day}"
                );
            }
        }
    }

    #[test]
    fn intersecting_bounds_keeps_the_narrower_side_of_each_end() {
        let d = |n| chrono::NaiveDate::from_ymd_opt(2026, 1, n).unwrap(); // lint-ok: date-literal
        assert_eq!(
            intersect_bounds((Some(d(1)), None), (Some(d(5)), Some(d(9)))),
            (Some(d(5)), Some(d(9)))
        );
        assert_eq!(
            intersect_bounds((Some(d(6)), Some(d(8))), (Some(d(5)), Some(d(9)))),
            (Some(d(6)), Some(d(8)))
        );
        assert_eq!(intersect_bounds((None, None), (None, None)), (None, None));
    }

    #[test]
    fn a_hit_opens_the_day_its_message_was_written() {
        // Newest first, as a path's positions are listed.
        let days = [(0, 4), (3, 1), (7, 2)];
        assert_eq!(hit_position(&days, |_| true, |d| d == 3), Some((3, 1)));
        // A message day outside the range falls back to the newest in range.
        assert_eq!(hit_position(&days, |d| d >= 7, |d| d == 3), Some((7, 2)));
        assert_eq!(hit_position(&days, |_| true, |_| false), Some((0, 4)));
    }
}
