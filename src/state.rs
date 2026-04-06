use std::path::PathBuf;
use std::sync::mpsc;

use chrono::NaiveDate;
use ratatui::text::Line;

use crate::aggregator::{CacheStats, CostCalculator, DailyGroup, Stats, TokenStats};
use crate::infrastructure::RetentionWarning;
use crate::{pins, search, ConversationMessage};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Daily,
    Insights,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConvListMode {
    Day,
    Pinned,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodFilter {
    All,
    Pinned,
    Today,
    Last7d,
    Last30d,
    ThisMonth,
    LastMonth,
    Last90d,
    Custom(NaiveDate, Option<NaiveDate>),
}

impl PeriodFilter {
    pub const ALL_VARIANTS: [PeriodFilter; 8] = [
        PeriodFilter::All,
        PeriodFilter::Pinned,
        PeriodFilter::Today,
        PeriodFilter::Last7d,
        PeriodFilter::Last30d,
        PeriodFilter::ThisMonth,
        PeriodFilter::LastMonth,
        PeriodFilter::Last90d,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PeriodFilter::All => "All",
            PeriodFilter::Pinned => "* Pinned",
            PeriodFilter::Today => "Today",
            PeriodFilter::Last7d => "7d",
            PeriodFilter::Last30d => "30d",
            PeriodFilter::ThisMonth => "This Month",
            PeriodFilter::LastMonth => "Last Month",
            PeriodFilter::Last90d => "90d",
            PeriodFilter::Custom(_, _) => "Custom",
        }
    }

    pub fn date_range(self) -> (Option<NaiveDate>, Option<NaiveDate>) {
        use chrono::Datelike;
        let today = chrono::Local::now().date_naive();
        match self {
            PeriodFilter::All | PeriodFilter::Pinned => (None, None),
            PeriodFilter::Today => (Some(today), None),
            PeriodFilter::Last7d => (Some(today - chrono::Duration::days(7)), None),
            PeriodFilter::Last30d => (Some(today - chrono::Duration::days(30)), None),
            PeriodFilter::ThisMonth => {
                let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
                (Some(first), None)
            }
            PeriodFilter::LastMonth => {
                let first_this = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();
                let last_prev = first_this - chrono::Duration::days(1);
                let first_prev =
                    NaiveDate::from_ymd_opt(last_prev.year(), last_prev.month(), 1).unwrap();
                (Some(first_prev), Some(last_prev))
            }
            PeriodFilter::Last90d => (Some(today - chrono::Duration::days(90)), None),
            PeriodFilter::Custom(start, end) => (Some(start), end),
        }
    }

    pub fn date_range_label(self) -> String {
        let (start, end) = self.date_range();
        let today = chrono::Local::now().date_naive();
        match (start, end) {
            (Some(s), None) if s == today => format!("({})", s.format("%m-%d")),
            (Some(s), None) => {
                format!("({} - {})", s.format("%m-%d"), today.format("%m-%d"))
            }
            (Some(s), Some(e)) => {
                format!("({} - {})", s.format("%m-%d"), e.format("%m-%d"))
            }
            _ => String::new(),
        }
    }

    pub fn parse_custom(input: &str) -> Option<PeriodFilter> {
        let input = input.trim();
        if let Some((left, right)) = input.split_once("..") {
            let start = NaiveDate::parse_from_str(left.trim(), "%Y-%m-%d").ok()?;
            let end = NaiveDate::parse_from_str(right.trim(), "%Y-%m-%d").ok()?;
            Some(PeriodFilter::Custom(start, Some(end)))
        } else if let Ok(date) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
            Some(PeriodFilter::Custom(date, Some(date)))
        } else {
            let parts: Vec<&str> = input.split('-').collect();
            if parts.len() == 2 {
                let year: i32 = parts[0].parse().ok()?;
                let month: u32 = parts[1].parse().ok()?;
                let first = NaiveDate::from_ymd_opt(year, month, 1)?;
                let next = if month == 12 {
                    NaiveDate::from_ymd_opt(year + 1, 1, 1)?
                } else {
                    NaiveDate::from_ymd_opt(year, month + 1, 1)?
                };
                let last = next - chrono::Duration::days(1);
                Some(PeriodFilter::Custom(first, Some(last)))
            } else {
                None
            }
        }
    }
}

#[derive(Clone)]
pub enum SummaryType {
    Session(Box<crate::aggregator::SessionInfo>),
    Day(crate::aggregator::DailyGroup),
}

pub const MAX_PANES: usize = 4;
pub const MIN_PANE_WIDTH: u16 = 40;
pub const SESSION_LIST_WIDTH: u16 = 28;
pub const SCROLL_LINES: usize = 5;

#[derive(Default)]
pub struct ConversationPane {
    pub messages: Vec<ConversationMessage>,
    pub scroll: usize,
    pub message_lines: Vec<(usize, usize)>,
    pub rendered: Option<(Vec<Line<'static>>, Vec<(usize, usize)>, Vec<bool>, Option<usize>)>,
    pub file_path: Option<PathBuf>,
    pub last_modified: Option<std::time::SystemTime>,
    pub reload_check: Option<std::time::Instant>,
    pub loading: bool,
    pub load_task: Option<mpsc::Receiver<Vec<ConversationMessage>>>,
    pub last_width: Option<u16>,
    pub selected_message: usize,
    pub focused_timestamp: Option<String>,
    pub search_mode: bool,
    pub search_query: String,
    pub search_matches: Vec<usize>,
    pub search_current: usize,
    pub search_saved_scroll: Option<(usize, usize)>,
}

impl ConversationPane {
    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.message_lines.clear();
        self.rendered = None;
        self.file_path = None;
        self.last_modified = None;
        self.reload_check = None;
        self.loading = false;
        self.load_task = None;
        self.last_width = None;
        self.selected_message = 0;
        self.focused_timestamp = None;
        self.search_mode = false;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_current = 0;
        self.search_saved_scroll = None;
    }
}

pub struct LoadedData {
    pub(crate) stats: Stats,
    pub(crate) cost: f64,
    pub(crate) cost_without_subagents: f64,
    pub(crate) model_costs: Vec<(String, f64)>,
    pub(crate) aggregated_model_tokens: std::collections::HashMap<String, TokenStats>,
    pub(crate) models_without_pricing: std::collections::HashSet<String>,
    pub(crate) daily_groups: Vec<DailyGroup>,
    pub(crate) daily_costs: Vec<(NaiveDate, f64)>,
    pub(crate) file_count: usize,
    pub(crate) cache_stats: CacheStats,
}

pub(crate) type LoadResult = Result<LoadedData, String>;

#[allow(clippy::type_complexity)]
pub struct AppState {
    pub needs_draw: bool,
    pub tab: Tab,
    pub pins: pins::Pins,
    pub conv_list_mode: ConvListMode,
    pub stats: Stats,
    pub total_cost: f64,
    pub cost_without_subagents: f64,
    pub model_costs: Vec<(String, f64)>,
    pub aggregated_model_tokens: std::collections::HashMap<String, TokenStats>,
    pub models_without_pricing: std::collections::HashSet<String>,
    pub daily_groups: Vec<DailyGroup>,
    pub daily_costs: Vec<(NaiveDate, f64)>,
    pub selected_day: usize,
    pub selected_session: usize,
    pub show_detail: bool,
    pub show_help: bool,
    pub help_scroll: u16,
    pub show_conversation: bool,
    pub show_summary: bool,
    pub summary_content: String,
    pub summary_scroll: usize,
    pub summary_type: Option<SummaryType>,
    pub daily_breakdown_focus: bool,
    pub daily_breakdown_scroll: usize,
    pub daily_breakdown_max_scroll: usize,
    pub generating_summary: bool,
    pub summary_task: Option<mpsc::Receiver<String>>,
    pub loading: bool,
    pub error: Option<String>,
    pub file_count: usize,
    pub cache_stats: Option<CacheStats>,
    pub dashboard_panel: usize,
    pub dashboard_scroll: [usize; 7],
    pub show_dashboard_detail: bool,
    pub search_mode: bool,
    pub search_query: String,
    pub search_results: Vec<search::SearchResult>,
    pub search_selected: usize,
    pub search_task: Option<(mpsc::Receiver<Vec<search::SearchResult>>, String)>,
    pub searching: bool,
    pub ctrl_c_pressed: bool,
    pub last_click_time: Option<std::time::Instant>,
    pub last_click_pos: (u16, u16),
    pub text_selection: Option<(u16, u16, u16, u16)>,
    pub selecting: bool,
    pub mouse_down_pos: Option<(u16, u16)>,
    pub screen_buffer: Option<ratatui::buffer::Buffer>,
    pub conversation_content_area: Option<ratatui::layout::Rect>,
    pub updating_session: Option<(usize, usize)>,
    pub updating_task: Option<(
        mpsc::Receiver<Result<String, String>>,
        PathBuf,
        usize,
        usize,
        usize,
    )>,
    pub last_data_update: Option<std::time::Instant>,
    pub data_reload_task: Option<mpsc::Receiver<LoadResult>>,
    pub data_limit: usize,
    pub animation_frame: usize,
    pub retention_warning: Option<RetentionWarning>,
    pub retention_warning_dismissed: bool,
    pub show_insights_detail: bool,
    pub insights_detail_scroll: usize,
    pub insights_panel: usize,
    pub toast_message: Option<String>,
    pub toast_time: Option<std::time::Instant>,
    pub panes: Vec<ConversationPane>,
    pub active_pane_index: Option<usize>,
    pub session_list_hidden: bool,
    pub show_conversation_detail: bool,
    pub tab_areas: Vec<(Tab, ratatui::layout::Rect)>,
    pub pane_areas: Vec<ratatui::layout::Rect>,
    pub dashboard_panel_areas: Vec<ratatui::layout::Rect>,
    pub insights_panel_areas: Vec<ratatui::layout::Rect>,
    pub session_list_area: Option<(ratatui::layout::Rect, usize, usize)>,
    pub breakdown_panel_area: Option<ratatui::layout::Rect>,
    pub summary_popup_area: Option<ratatui::layout::Rect>,
    pub daily_header_area: Option<ratatui::layout::Rect>,
    pub filter_popup_area_trigger: Option<ratatui::layout::Rect>,
    pub project_popup_area_trigger: Option<ratatui::layout::Rect>,
    pub pin_view_trigger: Option<ratatui::layout::Rect>,
    pub help_trigger: Option<ratatui::layout::Rect>,
    pub filter_popup_area: Option<ratatui::layout::Rect>,
    pub project_popup_area: Option<ratatui::layout::Rect>,
    pub search_results_area: Option<ratatui::layout::Rect>,
    pub period_filter: PeriodFilter,
    pub show_filter_popup: bool,
    pub filter_popup_selected: usize,
    pub filter_input_mode: bool,
    pub filter_input: String,
    pub filter_input_cursor: usize,
    pub filter_input_error: bool,
    pub project_filter: Option<String>,
    pub show_project_popup: bool,
    pub project_popup_selected: usize,
    pub project_popup_scroll: usize,
    pub project_list: Vec<(String, u64, NaiveDate)>,
    pub original_daily_groups: Vec<DailyGroup>,
    pub original_daily_costs: Vec<(NaiveDate, f64)>,
    pub original_stats: Stats,
    pub original_total_cost: f64,
    pub original_cost_without_subagents: f64,
    pub original_model_costs: Vec<(String, f64)>,
    pub original_aggregated_model_tokens: std::collections::HashMap<String, TokenStats>,
}

impl AppState {
    pub(crate) fn clear_summary(&mut self) {
        self.show_summary = false;
        self.generating_summary = false;
        self.summary_task = None;
        self.summary_content.clear();
        self.summary_scroll = 0;
        self.summary_type = None;
    }

    pub(crate) fn apply_loaded_data(&mut self, data: LoadedData) {
        self.original_stats = data.stats.clone();
        self.original_total_cost = data.cost;
        self.original_cost_without_subagents = data.cost_without_subagents;
        self.original_model_costs = data.model_costs.clone();
        self.original_aggregated_model_tokens = data.aggregated_model_tokens.clone();
        self.original_daily_groups = data.daily_groups.clone();
        self.original_daily_costs = data.daily_costs.clone();

        self.stats = data.stats;
        self.total_cost = data.cost;
        self.cost_without_subagents = data.cost_without_subagents;
        self.model_costs = data.model_costs;
        self.aggregated_model_tokens = data.aggregated_model_tokens;
        self.models_without_pricing = data.models_without_pricing;
        self.daily_groups = data.daily_groups;
        self.daily_costs = data.daily_costs;
        self.file_count = data.file_count;
        self.cache_stats = Some(data.cache_stats);
        self.last_data_update = Some(std::time::Instant::now());
        self.rebuild_project_list();

        if !matches!(self.period_filter, PeriodFilter::All) || self.project_filter.is_some() {
            self.apply_filter();
        }
    }

    pub fn apply_filter(&mut self) {
        let (start, end) = self.period_filter.date_range();
        let has_period = start.is_some() || end.is_some();
        let has_project = self.project_filter.is_some();
        let has_pinned = matches!(self.period_filter, PeriodFilter::Pinned);

        if !has_period && !has_project && !has_pinned {
            self.daily_groups = self.original_daily_groups.clone();
            self.daily_costs = self.original_daily_costs.clone();
            self.total_cost = self.original_total_cost;
            self.cost_without_subagents = self.original_cost_without_subagents;
            self.model_costs = self.original_model_costs.clone();
            self.aggregated_model_tokens = self.original_aggregated_model_tokens.clone();
            self.models_without_pricing =
                CostCalculator::global().models_without_pricing(&self.original_stats.model_tokens);
            self.stats = self.original_stats.clone();
        } else {
            let in_range = |date: &NaiveDate| -> bool {
                start.is_none_or(|s| *date >= s) && end.is_none_or(|e| *date <= e)
            };

            let mut groups: Vec<DailyGroup> = if has_pinned {
                self.original_daily_groups
                    .iter()
                    .filter_map(|g| {
                        let pinned: Vec<_> = g
                            .sessions
                            .iter()
                            .filter(|s| self.pins.is_pinned(&s.file_path))
                            .cloned()
                            .collect();
                        if pinned.is_empty() {
                            None
                        } else {
                            Some(DailyGroup {
                                date: g.date,
                                sessions: pinned,
                            })
                        }
                    })
                    .collect()
            } else {
                self.original_daily_groups
                    .iter()
                    .filter(|g| in_range(&g.date))
                    .cloned()
                    .collect()
            };

            if let Some(ref project) = self.project_filter {
                groups = groups
                    .into_iter()
                    .filter_map(|mut g| {
                        g.sessions.retain(|s| &s.project_name == project);
                        if g.sessions.is_empty() {
                            None
                        } else {
                            Some(g)
                        }
                    })
                    .collect();
            }
            self.daily_groups = groups;

            if has_project {
                let calculator = CostCalculator::global();
                let mut date_cost: std::collections::HashMap<NaiveDate, f64> =
                    std::collections::HashMap::new();
                for group in &self.daily_groups {
                    for session in &group.sessions {
                        if session.is_subagent {
                            continue;
                        }
                        for (model, tokens) in &session.day_tokens_by_model {
                            *date_cost.entry(group.date).or_insert(0.0) += calculator
                                .calculate_cost(tokens, Some(model))
                                .unwrap_or(0.0);
                        }
                    }
                }
                let mut costs: Vec<_> = date_cost.into_iter().collect();
                costs.sort_by(|a, b| b.0.cmp(&a.0));
                self.daily_costs = costs;
            } else {
                self.daily_costs = self
                    .original_daily_costs
                    .iter()
                    .filter(|(date, _)| in_range(date))
                    .copied()
                    .collect();
            }

            self.total_cost = self.daily_costs.iter().map(|(_, c)| c).sum();
            self.cost_without_subagents = self.total_cost;

            let calculator = CostCalculator::global();
            let mut all_model_tokens: std::collections::HashMap<String, TokenStats> =
                std::collections::HashMap::new();
            for group in &self.daily_groups {
                for session in &group.sessions {
                    if session.is_subagent {
                        continue;
                    }
                    for (model, tokens) in &session.day_tokens_by_model {
                        let entry = all_model_tokens.entry(model.clone()).or_default();
                        entry.input_tokens += tokens.input_tokens;
                        entry.output_tokens += tokens.output_tokens;
                        entry.cache_creation_tokens += tokens.cache_creation_tokens;
                        entry.cache_read_tokens += tokens.cache_read_tokens;
                    }
                }
            }
            self.model_costs = calculator.calculate_costs_by_model(&all_model_tokens);
            self.aggregated_model_tokens =
                CostCalculator::aggregate_tokens_by_model(&all_model_tokens);
            self.models_without_pricing = calculator.models_without_pricing(&all_model_tokens);

            self.rebuild_filtered_stats();
        }

        if self.selected_day >= self.daily_groups.len() {
            self.selected_day = self.daily_groups.len().saturating_sub(1);
        }
        if let Some(group) = self.daily_groups.get(self.selected_day) {
            let session_count = group.sessions.iter().filter(|s| !s.is_subagent).count();
            if self.selected_session >= session_count {
                self.selected_session = session_count.saturating_sub(1);
            }
        }
    }

    fn rebuild_filtered_stats(&mut self) {
        use chrono::Datelike;
        let mut stats = Stats::default();

        for group in &self.daily_groups {
            for session in &group.sessions {
                if session.is_subagent {
                    continue;
                }
                stats.total_sessions_count += 1;
                if session.summary.is_some() {
                    stats.sessions_with_summary += 1;
                }

                let work_tokens = session.day_input_tokens + session.day_output_tokens;
                stats.total_tokens.input_tokens += session.day_input_tokens;
                stats.total_tokens.output_tokens += session.day_output_tokens;

                for (model, tokens) in &session.day_tokens_by_model {
                    let entry = stats.model_tokens.entry(model.clone()).or_default();
                    entry.input_tokens += tokens.input_tokens;
                    entry.output_tokens += tokens.output_tokens;
                    entry.cache_creation_tokens += tokens.cache_creation_tokens;
                    entry.cache_read_tokens += tokens.cache_read_tokens;

                    stats.total_tokens.cache_creation_tokens += tokens.cache_creation_tokens;
                    stats.total_tokens.cache_read_tokens += tokens.cache_read_tokens;

                    *stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                }

                *stats.daily_activity.entry(group.date).or_insert(0) += session.day_input_tokens
                    + session.day_output_tokens
                    + session
                        .day_tokens_by_model
                        .values()
                        .map(|t| t.cache_creation_tokens + t.cache_read_tokens)
                        .sum::<u64>();
                *stats.daily_work_activity.entry(group.date).or_insert(0) += work_tokens;

                for (hour, tokens) in &session.day_hourly_activity {
                    *stats.hourly_activity.entry(*hour).or_insert(0) += tokens;
                }
                for (hour, tokens) in &session.day_hourly_work_tokens {
                    *stats.hourly_work_activity.entry(*hour).or_insert(0) += tokens;
                }

                let weekday = group.date.weekday();
                *stats.weekday_activity.entry(weekday).or_insert(0) += work_tokens;
                *stats.weekday_work_activity.entry(weekday).or_insert(0) += work_tokens;

                let project_stats = stats
                    .project_stats
                    .entry(session.project_name.clone())
                    .or_default();
                project_stats.sessions += 1;
                project_stats.work_tokens += work_tokens;
                project_stats.tokens += work_tokens;

                for (tool, count) in &session.day_tool_usage {
                    *stats.tool_usage.entry(tool.clone()).or_insert(0) += count;
                }
                for (lang, count) in &session.day_language_usage {
                    *stats.language_usage.entry(lang.clone()).or_insert(0) += count;
                }
                for (ext, count) in &session.day_extension_usage {
                    *stats.extension_usage.entry(ext.clone()).or_insert(0) += count;
                }
            }
        }

        // tool_error/success counts and branch_stats are file-level aggregates
        // not available per-session, so we use unfiltered values as approximation
        stats.tool_error_count = self.original_stats.tool_error_count;
        stats.tool_success_count = self.original_stats.tool_success_count;
        stats.branch_stats = self.original_stats.branch_stats.clone();

        self.stats = stats;
    }

    pub(crate) fn rebuild_project_list(&mut self) {
        let mut map: std::collections::HashMap<String, (u64, NaiveDate)> =
            std::collections::HashMap::new();
        for group in &self.original_daily_groups {
            for session in &group.sessions {
                if session.is_subagent {
                    continue;
                }
                let entry = map
                    .entry(session.project_name.clone())
                    .or_insert((0, group.date));
                entry.0 += session.day_input_tokens + session.day_output_tokens;
                if group.date > entry.1 {
                    entry.1 = group.date;
                }
            }
        }
        let mut list: Vec<_> = map
            .into_iter()
            .map(|(name, (tokens, date))| (name, tokens, date))
            .collect();
        list.sort_by(|a, b| b.1.cmp(&a.1));
        self.project_list = list;
    }
}
