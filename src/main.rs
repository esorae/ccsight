#![allow(clippy::let_underscore_must_use)]

mod aggregator;
mod cli;
mod conversation;
mod domain;
mod infrastructure;
mod mcp;
mod parser;
mod pins;
mod search;
mod state;
mod summary;
#[cfg(test)]
mod test_helpers;
mod text;
mod ui;

pub use state::*;

pub use conversation::{ConversationBlock, ConversationMessage};

pub(crate) const SUMMARY_MODEL: &str = "claude-haiku-4-5-20251001";

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use chrono::{Local, NaiveDate};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::DefaultTerminal;

use crate::aggregator::{CostCalculator, DailyGroup, DailyGrouper, Stats, StatsAggregator};
use crate::infrastructure::{check_cleanup_period, FileDiscovery};

#[derive(Parser, Debug)]
#[command(name = "ccsight")]
#[command(author, version, about = "Claude Code log viewer with statistics", long_about = None)]
struct Args {
    /// Maximum number of session files to load (0 = all)
    #[arg(short, long, default_value = "0")]
    limit: usize,

    /// Clear the cache before loading
    #[arg(long)]
    clear_cache: bool,

    /// Show daily cost summary and exit
    #[arg(long)]
    daily: bool,

    /// Run as MCP server (stdio transport)
    #[arg(long)]
    mcp: bool,
}

pub fn cli_help_lines() -> Vec<(String, String)> {
    use clap::CommandFactory;
    let cmd = Args::command();
    let mut lines = Vec::new();
    for arg in cmd.get_arguments() {
        if arg.get_id() == "help" || arg.get_id() == "version" {
            continue;
        }
        let flag = if let Some(short) = arg.get_short() {
            if let Some(long) = arg.get_long() {
                format!("-{short}, --{long}")
            } else {
                format!("-{short}")
            }
        } else if let Some(long) = arg.get_long() {
            format!("    --{long}")
        } else {
            continue;
        };
        let help = arg
            .get_help()
            .map_or_else(String::new, ToString::to_string);
        lines.push((flag, help));
    }
    lines
}

impl ConversationPane {
    fn load_from(file_path: &std::path::Path) -> Self {
        let mut pane = Self::default();
        pane.load_task = Some(spawn_load_conversation(file_path));
        pane.loading = true;
        pane.scroll = usize::MAX;
        pane.file_path = Some(file_path.to_path_buf());
        pane.last_modified = std::fs::metadata(file_path)
            .and_then(|m| m.modified())
            .ok();
        pane
    }
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.clear_cache {
        if let Ok(home) = std::env::var("HOME") {
            let cache_path = PathBuf::from(home).join(".cache/ccsight/cache.json");
            if cache_path.exists() {
                std::fs::remove_file(&cache_path).ok();
                println!("Cache cleared: {}", cache_path.display());
            } else {
                println!("No cache file found");
            }
        }
        if infrastructure::SearchIndex::clear_index().is_ok() {
            println!("Search index cleared");
        }
        return Ok(());
    }

    if args.daily {
        cli::show_daily_costs(args.limit);
        return Ok(());
    }

    if args.mcp {
        let rt = tokio::runtime::Runtime::new()
            .map_err(io::Error::other)?;
        rt.block_on(mcp::run_mcp_server(args.limit))
            .map_err(io::Error::other)?;
        return Ok(());
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste);
        original_hook(panic_info);
    }));

    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;

    let result = run(terminal, args.limit);

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste)?;
    result
}

fn run(mut terminal: DefaultTerminal, limit: usize) -> io::Result<()> {
    thread::spawn(ui::warmup_syntax_highlighting);

    let mut state = AppState {
        needs_draw: true,
        tab: Tab::Dashboard,
        pins: pins::Pins::load().unwrap_or_else(|_| pins::Pins::empty()),
        conv_list_mode: ConvListMode::Day,
        stats: Stats::default(),
        total_cost: 0.0,
        cost_without_subagents: 0.0,
        model_costs: Vec::new(),
        aggregated_model_tokens: std::collections::HashMap::new(),
        models_without_pricing: std::collections::HashSet::new(),
        daily_groups: Vec::new(),
        daily_costs: Vec::new(),
        selected_day: 0,
        selected_session: 0,
        show_detail: false,
        show_help: false,
        help_scroll: 0,
        show_conversation: false,
        show_summary: false,
        summary_content: String::new(),
        summary_scroll: 0,
        summary_type: None,
        daily_breakdown_focus: false,
        daily_breakdown_scroll: 0,
        daily_breakdown_max_scroll: 0,
        generating_summary: false,
        summary_task: None,
        loading: true,
        error: None,
        file_count: 0,
        cache_stats: None,
        dashboard_panel: 0,
        dashboard_scroll: [0; 7],
        show_dashboard_detail: false,
        search_mode: false,
        search_input: TextInput::default(),
        search_results: Vec::new(),
        search_selected: 0,
        search_task: None,
        searching: false,
        search_preview_mode: false,
        search_saved_state: None,
        search_index: None,
        index_build_task: None,
        ctrl_c_pressed: false,
        last_click_time: None,
        last_click_pos: (0, 0),
        text_selection: None,
        selecting: false,
        mouse_down_pos: None,
        screen_buffer: None,
        conversation_content_area: None,
        updating_session: None,
        updating_task: None,
        last_data_update: None,
        data_reload_task: None,
        data_limit: limit,
        animation_frame: 0,
        retention_warning: check_cleanup_period(),
        retention_warning_dismissed: false,
        show_insights_detail: false,
        insights_detail_scroll: 0,
        insights_panel: 0,
        toast_message: None,
        toast_time: None,
        panes: Vec::new(),
        active_pane_index: None,
        session_list_hidden: false,
        show_conversation_detail: false,
        tab_areas: Vec::new(),
        pane_areas: Vec::new(),
        dashboard_panel_areas: Vec::new(),
        insights_panel_areas: Vec::new(),
        session_list_area: None,
        breakdown_panel_area: None,
        summary_popup_area: None,
        daily_header_area: None,
        filter_popup_area_trigger: None,
        project_popup_area_trigger: None,
        pin_view_trigger: None,
        help_trigger: None,
        filter_popup_area: None,
        project_popup_area: None,
        search_results_area: None,
        period_filter: PeriodFilter::All,
        show_filter_popup: false,
        filter_popup_selected: 0,
        filter_input_mode: false,
        filter_input: TextInput::default(),
        filter_input_error: false,
        project_filter: None,
        show_project_popup: false,
        project_popup_selected: 0,
        project_popup_scroll: 0,
        project_list: Vec::new(),
        original_daily_groups: Vec::new(),
        original_daily_costs: Vec::new(),
        original_stats: Stats::default(),
        original_total_cost: 0.0,
        original_cost_without_subagents: 0.0,
        original_model_costs: Vec::new(),
        original_aggregated_model_tokens: std::collections::HashMap::new(),
    };

    execute!(io::stdout(), BeginSynchronizedUpdate)?;
    let completed = terminal.draw(|f| ui::draw(f, &mut state))?;
    state.screen_buffer = Some(completed.buffer.clone());
    execute!(io::stdout(), EndSynchronizedUpdate)?;

    let (tx, rx) = mpsc::channel::<LoadResult>();

    thread::spawn(move || {
        let result = load_data(limit).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });

    loop {
        if state.tab == Tab::Dashboard && !state.show_dashboard_detail {
            let today = Local::now().date_naive();
            if let Some(oldest) = state.daily_groups.iter().map(|g| g.date).min() {
                let days_from_oldest = (today - oldest).num_days().max(0) as usize;
                let max_scroll = days_from_oldest / 7;
                state.dashboard_scroll[5] = state.dashboard_scroll[5].min(max_scroll);
            }
        }

        if state.needs_draw {
            execute!(io::stdout(), BeginSynchronizedUpdate)?;
            let completed = terminal.draw(|f| ui::draw(f, &mut state))?;
            state.screen_buffer = Some(completed.buffer.clone());
            execute!(io::stdout(), EndSynchronizedUpdate)?;
            state.needs_draw = false;
        }

        let has_pending = state.loading || state.generating_summary
            || state.toast_time.is_some()
            || state.data_reload_task.is_some()
            || state.summary_task.is_some()
            || state.search_task.is_some()
            || state.index_build_task.is_some()
            || state.panes.iter().any(|p| p.loading);
        if has_pending && state.index_build_task.is_none() {
            state.needs_draw = true;
        }

        if state.loading || state.generating_summary || state.panes.iter().any(|p| p.loading) {
            state.animation_frame = state.animation_frame.wrapping_add(1);
        }

        if let Some(toast_time) = state.toast_time
            && toast_time.elapsed() > std::time::Duration::from_secs(2) {
                state.toast_message = None;
                state.toast_time = None;
                state.needs_draw = true;
            }

        if state.loading
            && let Ok(result) = rx.try_recv() {
                match result {
                    Ok(data) => {
                        state.apply_loaded_data(data);
                        state.loading = false;
                        start_index_build(&mut state);
                    }
                    Err(e) => {
                        state.error = Some(e);
                        state.loading = false;
                    }
                }
            }

        if let Some(ref reload_rx) = state.data_reload_task
            && let Ok(result) = reload_rx.try_recv() {
                state.data_reload_task = None;
                if let Ok(data) = result {
                    state.apply_loaded_data(data);

                    if state.selected_day >= state.daily_groups.len() {
                        state.selected_day = state.daily_groups.len().saturating_sub(1);
                    }
                    if let Some(group) = state.daily_groups.get(state.selected_day) {
                        let session_count = group.sessions.iter().filter(|s| !s.is_subagent).count();
                        if state.selected_session >= session_count {
                            state.selected_session = session_count.saturating_sub(1);
                        }
                    }
                    state.search_results.clear();
                    state.search_selected = 0;
                    start_index_build(&mut state);
                    if state.search_mode && !state.search_input.text.is_empty() {
                        state.search_results = search::perform_search(
                            &state.daily_groups,
                            &state.search_input.text,
                        );
                        start_content_search(&mut state);
                    }
                }
            }

        if !state.loading && state.data_reload_task.is_none() {
            let should_reload = state
                .last_data_update
                .is_some_and(|last| last.elapsed() > std::time::Duration::from_secs(30));

            if should_reload {
                let limit = state.data_limit;
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let result = load_data(limit).map_err(|e| e.to_string());
                    let _ = tx.send(result);
                });
                state.data_reload_task = Some(rx);
            }
        }

        if let Some((ref rx, ref file_path, day_idx, _session_idx, actual_idx)) =
            state.updating_task
            && let Ok(result) = rx.try_recv() {
                let file_path = file_path.clone();
                state.updating_task = None;
                state.updating_session = None;

                match result {
                    Ok(new_summary) => {
                        if update_jsonl_summary(&file_path, &new_summary).is_ok() {
                            if let Some(group) = state.daily_groups.get_mut(day_idx)
                                && let Some(session) = group.sessions.get_mut(actual_idx) {
                                    session.summary = Some(new_summary);
                                }
                        } else if !state.show_detail {
                            state.show_summary = true;
                            state.summary_content = "❌ Failed to write JSONL file".to_string();
                            state.summary_scroll = 0;
                        }
                    }
                    Err(e) => {
                        if !state.show_detail {
                            state.show_summary = true;
                            state.summary_content = format!("❌ Error: {e}");
                            state.summary_scroll = 0;
                        }
                    }
                }
            }

        if state.show_conversation {
            for pane in &mut state.panes {
                let should_check = pane
                    .reload_check
                    .is_none_or(|last| last.elapsed() > std::time::Duration::from_millis(500));

                if should_check {
                    pane.reload_check = Some(std::time::Instant::now());
                    if let Some(ref file_path) = pane.file_path.clone()
                        && let Ok(metadata) = std::fs::metadata(file_path)
                            && let Ok(modified) = metadata.modified() {
                                let needs_reload = pane
                                    .last_modified
                                    .is_some_and(|last| modified > last);
                                if needs_reload && pane.load_task.is_none() {
                                    if let Some(&(_, msg_idx)) =
                                        pane.message_lines.get(pane.selected_message)
                                        && let Some(msg) = pane.messages.get(msg_idx) {
                                            pane.focused_timestamp = msg.timestamp.clone();
                                        }
                                    pane.load_task = Some(spawn_load_conversation(file_path));
                                    pane.loading = true;
                                    pane.last_modified = Some(modified);
                                }
                            }
                }
            }
        }

        for pane in &mut state.panes {
            if let Some(ref rx) = pane.load_task
                && let Ok(messages) = rx.try_recv() {
                    let is_reload = !pane.messages.is_empty();
                    let old_count = pane.message_lines.len();
                    let was_at_last = old_count > 0 && pane.selected_message >= old_count - 1;

                    pane.messages = messages;
                    pane.loading = false;
                    pane.load_task = None;
                    pane.rendered = None;

                    if is_reload {
                        if was_at_last {
                            if let Some(msg) = pane
                                .messages
                                .iter()
                                .rev()
                                .find(|m| !ui::is_thinking_only_message(m))
                            {
                                pane.focused_timestamp = msg.timestamp.clone();
                            }
                            pane.scroll = usize::MAX;
                            pane.selected_message = usize::MAX;
                        }
                    } else {
                        pane.search_matches.clear();
                        pane.search_current = 0;
                        pane.search_saved_scroll = None;
                        pane.scroll = usize::MAX;
                        pane.selected_message = usize::MAX;
                    }
                }
        }

        if let Some(ref rx) = state.summary_task
            && let Ok(content) = rx.try_recv() {
                state.summary_content = content;
                state.generating_summary = false;
                state.summary_scroll = 0;
                state.summary_task = None;
            }

        if let Some(ref index_rx) = state.index_build_task
            && let Ok(index) = index_rx.try_recv() {
                state.search_index = Some(index);
                state.index_build_task = None;
            }

        if let Some((ref rx, ref query)) = state.search_task
            && let Ok(content_results) = rx.try_recv() {
                if *query == state.search_input.text {
                    for result in content_results {
                        if !state.search_results.iter().any(|r| {
                            r.day_idx == result.day_idx && r.session_idx == result.session_idx
                        }) {
                            state.search_results.push(result);
                        }
                    }
                }
                state.search_task = None;
                state.searching = false;
            }

        if event::poll(Duration::from_millis(50))? {
            let ev = match std::panic::catch_unwind(event::read) {
                Ok(result) => result?,
                Err(_) => continue,
            };
            match ev {
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        state.text_selection = None;
                        state.selecting = false;
                        state.mouse_down_pos = Some((mouse.column, mouse.row));

                        let now = std::time::Instant::now();
                        let is_double_click = state.last_click_time.is_some_and(|t| {
                            now.duration_since(t) < Duration::from_millis(400)
                                && state.last_click_pos == (mouse.column, mouse.row)
                        });
                        if is_double_click {
                            state.last_click_time = None;
                            handle_double_click(&mut state, mouse.column, mouse.row);
                        } else {
                            state.last_click_time = Some(now);
                            state.last_click_pos = (mouse.column, mouse.row);
                            handle_mouse_click(&mut state, mouse.column, mouse.row);
                        }
                    }
                    MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                        if let Some((sc, sr)) = state.mouse_down_pos {
                            state.selecting = true;
                            state.text_selection = Some((sc, sr, mouse.column, mouse.row));

                            if state.show_conversation
                                && let Some(ca) = state.conversation_content_area {
                                    let mut scrolled = false;
                                    let scroll_amount = 2;

                                    if mouse.row < ca.y {
                                        let idx = state.active_pane_index.unwrap_or(0);
                                        if let Some(pane) = state.panes.get_mut(idx)
                                            && pane.scroll > 0 {
                                                pane.scroll = pane.scroll.saturating_sub(scroll_amount);
                                                scrolled = true;
                                            }
                                    } else if mouse.row >= ca.y + ca.height {
                                        let idx = state.active_pane_index.unwrap_or(0);
                                        if let Some(pane) = state.panes.get_mut(idx)
                                            && let Some(cached) = pane.rendered.as_ref() {
                                                let max_scroll = cached.0.len().saturating_sub(ca.height as usize);
                                                if pane.scroll < max_scroll {
                                                    pane.scroll = (pane.scroll + scroll_amount).min(max_scroll);
                                                    scrolled = true;
                                                }
                                            }
                                    }

                                    if scrolled {
                                        continue;
                                    }
                                }
                        }
                    }
                    MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                        if state.selecting {
                            state.selecting = false;
                            state.mouse_down_pos = None;
                            if let (Some(sel), Some(buf)) =
                                (&state.text_selection, &state.screen_buffer)
                            {
                                let conv_area = if state.show_conversation {
                                    state.conversation_content_area
                                } else {
                                    None
                                };
                                let (wrap_flags, conv_scroll) = if state.show_conversation {
                                    let idx = state.active_pane_index.unwrap_or(0);
                                    state.panes.get(idx)
                                        .and_then(|p| p.rendered.as_ref().map(|(_, _, flags, _)| (flags.as_slice(), p.scroll)))
                                        .map_or((None, 0), |(flags, scroll)| (Some(flags), scroll))
                                } else {
                                    (None, 0)
                                };
                                let text =
                                    extract_selected_text_from_buffer(sel, buf, conv_area, wrap_flags, conv_scroll);
                                if !text.is_empty() {
                                    match arboard::Clipboard::new() {
                                        Ok(mut clipboard) => {
                                            if clipboard.set_text(&text).is_ok() {
                                                let len = text.chars().count();
                                                state.toast_message =
                                                    Some(format!("Copied ({len} chars)"));
                                                state.toast_time =
                                                    Some(std::time::Instant::now());
                                            }
                                        }
                                        Err(_) => {
                                            state.toast_message =
                                                Some("Clipboard unavailable".to_string());
                                            state.toast_time = Some(std::time::Instant::now());
                                        }
                                    }
                                }
                            }
                        } else {
                            state.mouse_down_pos = None;
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        state.text_selection = None;
                        handle_mouse_scroll(&mut state, mouse.column, mouse.row, true);
                    }
                    MouseEventKind::ScrollDown => {
                        state.text_selection = None;
                        handle_mouse_scroll(&mut state, mouse.column, mouse.row, false);
                    }
                    _ => {}
                },
                Event::Paste(text) => {
                    if state.search_mode {
                        for c in text.chars() {
                            state.search_input.insert_char(c);
                        }
                        state.search_results = search::perform_search(
                            &state.daily_groups,
                            &state.search_input.text,
                        );
                        state.search_selected = 0;
                        start_content_search(&mut state);
                    } else if state.filter_input_mode {
                        for c in text.chars() {
                            state.filter_input.insert_char(c);
                        }
                        state.filter_input_error = false;
                    } else if let Some(idx) = state.active_pane_index
                        && let Some(pane) = state.panes.get_mut(idx)
                        && pane.search_mode
                    {
                        for c in text.chars() {
                            pane.search_input.insert_char(c);
                        }
                        ui::update_pane_search_matches(pane);
                    }
                }
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        use crossterm::event::KeyModifiers;
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            if state.ctrl_c_pressed {
                                break;
                            }
                            state.ctrl_c_pressed = true;
                            state.toast_message = Some("Press again to quit".to_string());
                            state.toast_time = Some(std::time::Instant::now());
                            state.needs_draw = true;
                            continue;
                        }
                        if key.code != KeyCode::Char('q') {
                            state.ctrl_c_pressed = false;
                        }

                        if state.show_help {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                                    state.show_help = false;
                                    state.help_scroll = 0;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    state.help_scroll = state.help_scroll.saturating_add(1);
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    state.help_scroll = state.help_scroll.saturating_sub(1);
                                }
                                _ => {}
                            }
                        } else if state.show_filter_popup {
                            let total_items = PeriodFilter::ALL_VARIANTS.len() + 1;
                            if state.filter_input_mode {
                                match key.code {
                                    KeyCode::Esc => {
                                        state.filter_input_mode = false;
                                        state.filter_input.clear();
                                        state.filter_input_error = false;
                                    }
                                    KeyCode::Enter => {
                                        if let Some(filter) =
                                            PeriodFilter::parse_custom(&state.filter_input.text)
                                        {
                                            state.period_filter = filter;
                                            state.apply_filter();
                                            state.show_filter_popup = false;
                                            state.filter_input_mode = false;
                                            state.filter_input.clear();
                                            state.filter_input_error = false;
                                        } else {
                                            state.filter_input_error = true;
                                        }
                                    }
                                    KeyCode::Backspace => {
                                        state.filter_input.delete_back();
                                        state.filter_input_error = false;
                                    }
                                    KeyCode::Left => { state.filter_input.move_left(); }
                                    KeyCode::Right => { state.filter_input.move_right(); }
                                    KeyCode::Home => { state.filter_input.move_home(); }
                                    KeyCode::End => { state.filter_input.move_end(); }
                                    KeyCode::Char(c) => {
                                        state.filter_input.insert_char(c);
                                        state.filter_input_error = false;
                                    }
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => {
                                        state.show_filter_popup = false;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        if state.filter_popup_selected > 0 {
                                            state.filter_popup_selected -= 1;
                                        }
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        if state.filter_popup_selected < total_items - 1 {
                                            state.filter_popup_selected += 1;
                                        }
                                    }
                                    KeyCode::Enter => {
                                        if state.filter_popup_selected
                                            < PeriodFilter::ALL_VARIANTS.len()
                                        {
                                            state.period_filter = PeriodFilter::ALL_VARIANTS
                                                [state.filter_popup_selected];
                                            state.apply_filter();
                                            state.show_filter_popup = false;
                                        } else {
                                            state.filter_input_mode = true;
                                            let text = match state.period_filter {
                                                PeriodFilter::Custom(s, Some(e)) if s == e => {
                                                    s.format("%Y-%m-%d").to_string()
                                                }
                                                PeriodFilter::Custom(s, Some(e)) => {
                                                    format!(
                                                        "{}..{}",
                                                        s.format("%Y-%m-%d"),
                                                        e.format("%Y-%m-%d")
                                                    )
                                                }
                                                PeriodFilter::Custom(s, None) => {
                                                    s.format("%Y-%m-%d").to_string()
                                                }
                                                _ => String::new(),
                                            };
                                            state.filter_input.set(text);
                                            state.filter_input_error = false;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        } else if state.show_project_popup {
                            let total = state.project_list.len() + 1;
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => {
                                    state.show_project_popup = false;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if state.project_popup_selected > 0 {
                                        state.project_popup_selected -= 1;
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if state.project_popup_selected < total - 1 {
                                        state.project_popup_selected += 1;
                                    }
                                }
                                KeyCode::Enter => {
                                    if state.project_popup_selected == 0 {
                                        state.project_filter = None;
                                    } else if let Some((name, _, _)) =
                                        state.project_list.get(state.project_popup_selected - 1)
                                    {
                                        state.project_filter = Some(name.clone());
                                    }
                                    state.apply_filter();
                                    state.show_project_popup = false;
                                }
                                _ => {}
                            }
                        } else if state.show_summary {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') => {
                                    state.clear_summary();
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    state.summary_scroll = state.summary_scroll.saturating_add(1);
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    state.summary_scroll = state.summary_scroll.saturating_sub(1);
                                }
                                KeyCode::Char('r') => {
                                    if state.summary_task.is_none()
                                        && let Some(ref summary_type) = state.summary_type.clone() {
                                            state.generating_summary = true;
                                            let (tx, rx) = mpsc::channel();
                                            state.summary_task = Some(rx);
                                            match summary_type {
                                                SummaryType::Session(session) => {
                                                    let session = session.clone();
                                                    thread::spawn(move || {
                                                        let summary =
                                                            summary::regenerate_session_summary(
                                                                &session,
                                                            );
                                                        let _ = tx.send(summary);
                                                    });
                                                }
                                                SummaryType::Day(group) => {
                                                    let group = group.clone();
                                                    thread::spawn(move || {
                                                        let summary =
                                                            summary::regenerate_day_summary(&group);
                                                        let _ = tx.send(summary);
                                                    });
                                                }
                                            }
                                        }
                                }
                                _ => {}
                            }
                        } else if state.show_conversation {
                            {
                                let pane_search_mode = state
                                    .active_pane_index
                                    .and_then(|i| state.panes.get(i))
                                    .is_some_and(|p| p.search_mode);

                                if pane_search_mode {
                                    if let Some(idx) = state.active_pane_index
                                        && let Some(pane) = state.panes.get_mut(idx) {
                                            match key.code {
                                                KeyCode::Esc => {
                                                    pane.search_mode = false;
                                                    if pane.search_matches.is_empty() {
                                                        pane.search_input.clear();
                                                        if let Some((saved_scroll, saved_msg)) = pane.search_saved_scroll.take() {
                                                            pane.scroll = saved_scroll;
                                                            pane.selected_message = saved_msg;
                                                        }
                                                    } else {
                                                        pane.search_saved_scroll = None;
                                                    }
                                                }
                                                KeyCode::Enter => {
                                                    if !pane.search_matches.is_empty() {
                                                        if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
                                                            pane.search_current = pane
                                                                .search_current
                                                                .checked_sub(1)
                                                                .unwrap_or(pane.search_matches.len() - 1);
                                                        } else {
                                                            pane.search_current = (pane.search_current + 1)
                                                                % pane.search_matches.len();
                                                        }
                                                        pane.scroll = pane.search_matches[pane.search_current];
                                                        if let Some(msg_idx) = pane.message_lines.iter().rposition(|&(start, _)| start <= pane.scroll) {
                                                            pane.selected_message = msg_idx;
                                                        }
                                                    }
                                                }
                                                KeyCode::Backspace => {
                                                    pane.search_input.delete_back();
                                                    ui::update_pane_search_matches(pane);
                                                }
                                                KeyCode::Left => { pane.search_input.move_left(); }
                                                KeyCode::Right => { pane.search_input.move_right(); }
                                                KeyCode::Home => { pane.search_input.move_home(); }
                                                KeyCode::End => { pane.search_input.move_end(); }
                                                KeyCode::Char(c) => {
                                                    pane.search_input.insert_char(c);
                                                    ui::update_pane_search_matches(pane);
                                                }
                                                _ => {}
                                            }
                                        }
                                } else {
                                    match key.code {
                                        KeyCode::Char('0') => {
                                            state.active_pane_index = None;
                                        }
                                        KeyCode::Char('1')
                                        | KeyCode::Char('2')
                                        | KeyCode::Char('3')
                                        | KeyCode::Char('4') => {
                                            let target_idx = match key.code {
                                                KeyCode::Char('1') => 0,
                                                KeyCode::Char('2') => 1,
                                                KeyCode::Char('3') => 2,
                                                KeyCode::Char('4') => 3,
                                                _ => unreachable!(),
                                            };
                                            if state.active_pane_index.is_none() {
                                                if let Some(group) =
                                                    state.daily_groups.get(state.selected_day)
                                                {
                                                    let sessions: Vec<_> = group
                                                        .sessions
                                                        .iter()
                                                        .filter(|s| !s.is_subagent)
                                                        .collect();
                                                    if let Some(session) =
                                                        sessions.get(state.selected_session)
                                                    {
                                                        let new_pane = ConversationPane::load_from(&session.file_path);
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
                                        KeyCode::Char('Q') => {
                                            state.show_conversation = false;
                                            state.panes.clear();
                                            state.active_pane_index = None;
                                            state.conv_list_mode = ConvListMode::Day;
                                        }
                                        KeyCode::Char('T') => {
                                            state.session_list_hidden = !state.session_list_hidden;
                                        }
                                        KeyCode::Esc | KeyCode::Char('q') => {
                                            let has_search = state.active_pane_index
                                                .and_then(|i| state.panes.get(i))
                                                .is_some_and(|p| !p.search_input.text.is_empty());
                                            if has_search {
                                                if let Some(idx) = state.active_pane_index
                                                    && let Some(pane) = state.panes.get_mut(idx) {
                                                        pane.search_input.text.clear();
                                                        pane.search_input.cursor = 0;
                                                        pane.search_matches.clear();
                                                        pane.search_current = 0;
                                                        if let Some((saved_scroll, saved_msg)) = pane.search_saved_scroll.take() {
                                                            pane.scroll = saved_scroll;
                                                            pane.selected_message = saved_msg;
                                                        }
                                                    }
                                            } else if state.search_preview_mode {
                                                state.show_conversation = false;
                                                for pane in &mut state.panes { pane.clear(); }
                                                state.active_pane_index = None;
                                                if let Some((tab, day, session, _)) = &state.search_saved_state {
                                                    state.tab = *tab;
                                                    state.selected_day = *day;
                                                    state.selected_session = *session;
                                                }
                                                state.search_mode = true;
                                                state.search_preview_mode = false;
                                            } else if state.active_pane_index.is_none() {
                                                if !state.panes.is_empty() {
                                                    state.active_pane_index = Some(0);
                                                } else {
                                                    state.show_conversation = false;
                                                    state.conv_list_mode = ConvListMode::Day;
                                                }
                                            } else if let Some(idx) = state.active_pane_index {
                                                state.panes.remove(idx);
                                                if state.panes.is_empty() {
                                                    state.show_conversation = false;
                                                    state.active_pane_index = None;
                                                    state.conv_list_mode = ConvListMode::Day;
                                                } else {
                                                    let new_idx = idx.min(state.panes.len() - 1);
                                                    state.active_pane_index = Some(new_idx);
                                                }
                                            }
                                        }
                                        KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
                                            let pane_count = state.panes.len();
                                            if pane_count > 0 {
                                                state.active_pane_index =
                                                    match state.active_pane_index {
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
                                                state.active_pane_index =
                                                    match state.active_pane_index {
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
                                        KeyCode::Char('/') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    pane.search_saved_scroll = Some((pane.scroll, pane.selected_message));
                                                    pane.search_mode = true;
                                                    pane.search_input.clear();
                                                    pane.search_matches.clear();
                                                    pane.search_current = 0;
                                                }
                                        }
                                        KeyCode::Char('j') => {
                                            if state.active_pane_index.is_none() {
                                                let max = get_conv_session_count(&state)
                                                    .saturating_sub(1);
                                                if state.selected_session < max {
                                                    state.selected_session += 1;
                                                } else if state.conv_list_mode == ConvListMode::Day
                                                    && state.selected_day
                                                        < state
                                                            .daily_groups
                                                            .len()
                                                            .saturating_sub(1)
                                                {
                                                    state.selected_day += 1;
                                                    state.selected_session = 0;
                                                }
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            }
                                        }
                                        KeyCode::Char('k') => {
                                            if state.active_pane_index.is_none() {
                                                if state.selected_session > 0 {
                                                    state.selected_session -= 1;
                                                } else if state.conv_list_mode == ConvListMode::Day
                                                    && state.selected_day > 0
                                                {
                                                    state.selected_day -= 1;
                                                    state.selected_session =
                                                        get_conv_session_count(&state)
                                                            .saturating_sub(1);
                                                }
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            }
                                        }
                                        KeyCode::Char('H') => {
                                            if state.conv_list_mode == ConvListMode::Day
                                                && state.selected_day
                                                    < state.daily_groups.len().saturating_sub(1)
                                            {
                                                state.selected_day += 1;
                                                state.selected_session = 0;
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            }
                                        }
                                        KeyCode::Char('L') => {
                                            if state.conv_list_mode == ConvListMode::Day
                                                && state.selected_day > 0
                                            {
                                                state.selected_day -= 1;
                                                state.selected_session = 0;
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            }
                                        }
                                        KeyCode::Char(' ') => {
                                            if state.show_conversation_detail {
                                                let fp = state
                                                    .active_pane_index
                                                    .and_then(|i| state.panes.get(i))
                                                    .and_then(|p| p.file_path.clone())
                                                    .or_else(|| {
                                                        get_conv_session_file(
                                                            &state,
                                                            state.selected_session,
                                                        )
                                                    });
                                                if let Some(fp) = fp {
                                                    state.pins.toggle(&fp);
                                                    if let Err(e) = state.pins.save() { state.toast_message = Some(format!("Pin save failed: {e}")); state.toast_time = Some(std::time::Instant::now()); }
                                                    state.needs_draw = true;
                                                }
                                            } else if state.active_pane_index.is_none()
                                                && let Some(fp) = get_conv_session_file(
                                                    &state,
                                                    state.selected_session,
                                                ) {
                                                    state.pins.toggle(&fp);
                                                    if let Err(e) = state.pins.save() { state.toast_message = Some(format!("Pin save failed: {e}")); state.toast_time = Some(std::time::Instant::now()); }
                                                    state.needs_draw = true;
                                                }
                                        }
                                        KeyCode::Char('m') => {
                                            if !state.pins.entries().is_empty() {
                                                state.conv_list_mode = ConvListMode::Pinned;
                                                state.selected_session = 0;
                                                state.active_pane_index = None;
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            }
                                        }
                                        KeyCode::BackTab => {
                                            if state.active_pane_index.is_none() {
                                                state.conv_list_mode = match state.conv_list_mode {
                                                    ConvListMode::Day => {
                                                        if state.pins.entries().is_empty() {
                                                            ConvListMode::All
                                                        } else {
                                                            ConvListMode::Pinned
                                                        }
                                                    }
                                                    ConvListMode::Pinned => ConvListMode::All,
                                                    ConvListMode::All => ConvListMode::Day,
                                                };
                                                state.selected_session = 0;
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            }
                                        }
                                        KeyCode::Down => {
                                            if state.active_pane_index.is_none() {
                                                let max = get_conv_session_count(&state)
                                                    .saturating_sub(1);
                                                if state.selected_session < max {
                                                    state.selected_session += 1;
                                                } else if state.conv_list_mode == ConvListMode::Day
                                                    && state.selected_day
                                                        < state
                                                            .daily_groups
                                                            .len()
                                                            .saturating_sub(1)
                                                {
                                                    state.selected_day += 1;
                                                    state.selected_session = 0;
                                                }
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            } else if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    let msg_count = pane.message_lines.len();
                                                    if msg_count > 0 {
                                                        pane.selected_message =
                                                            pane.selected_message
                                                                .saturating_add(1)
                                                                .min(msg_count - 1);
                                                    }
                                                }
                                        }
                                        KeyCode::Up => {
                                            if state.active_pane_index.is_none() {
                                                if state.selected_session > 0 {
                                                    state.selected_session -= 1;
                                                } else if state.conv_list_mode == ConvListMode::Day
                                                    && state.selected_day > 0
                                                {
                                                    state.selected_day -= 1;
                                                    state.selected_session =
                                                        get_conv_session_count(&state)
                                                            .saturating_sub(1);
                                                }
                                                if state.panes.len() == 1 {
                                                    preview_conversation_in_pane(&mut state);
                                                }
                                            } else if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx)
                                                    && pane.selected_message > 0 {
                                                        pane.selected_message -= 1;
                                                    }
                                        }
                                        KeyCode::PageDown | KeyCode::Char('d') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    pane.scroll = pane.scroll.saturating_add(20);
                                                    if !pane.message_lines.is_empty() {
                                                        let msg_idx = pane
                                                            .message_lines
                                                            .iter()
                                                            .position(|&(start, _)| {
                                                                start >= pane.scroll
                                                            })
                                                            .unwrap_or(
                                                                pane.message_lines.len() - 1,
                                                            );
                                                        pane.selected_message = msg_idx;
                                                    }
                                                }
                                        }
                                        KeyCode::PageUp | KeyCode::Char('u') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    pane.scroll = pane.scroll.saturating_sub(20);
                                                    if !pane.message_lines.is_empty() {
                                                        let msg_idx = pane
                                                            .message_lines
                                                            .iter()
                                                            .rposition(|&(start, _)| {
                                                                start <= pane.scroll
                                                            })
                                                            .unwrap_or(0);
                                                        pane.selected_message = msg_idx;
                                                    }
                                                }
                                        }
                                        KeyCode::Home | KeyCode::Char('g') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    pane.scroll = 0;
                                                    pane.selected_message = 0;
                                                }
                                        }
                                        KeyCode::End | KeyCode::Char('G') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    pane.scroll = usize::MAX;
                                                    let msg_count = pane.message_lines.len();
                                                    if msg_count > 0 {
                                                        pane.selected_message = msg_count - 1;
                                                    }
                                                }
                                        }
                                        KeyCode::Char('n') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    if !pane.search_matches.is_empty() {
                                                        pane.search_current = (pane.search_current
                                                            + 1)
                                                            % pane.search_matches.len();
                                                        pane.scroll = pane.search_matches
                                                            [pane.search_current];
                                                        if let Some(msg_idx) = pane.message_lines.iter().rposition(|&(start, _)| start <= pane.scroll) {
                                                            pane.selected_message = msg_idx;
                                                        }
                                                    } else if let Some(&(next_pos, _)) = pane
                                                        .message_lines
                                                        .iter()
                                                        .find(|&&(pos, _)| pos > pane.scroll + 2)
                                                    {
                                                        pane.scroll = next_pos;
                                                    }
                                                }
                                        }
                                        KeyCode::Char('N') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    if !pane.search_matches.is_empty() {
                                                        pane.search_current = pane
                                                            .search_current
                                                            .checked_sub(1)
                                                            .unwrap_or(
                                                                pane.search_matches.len() - 1,
                                                            );
                                                        pane.scroll = pane.search_matches
                                                            [pane.search_current];
                                                        if let Some(msg_idx) = pane.message_lines.iter().rposition(|&(start, _)| start <= pane.scroll) {
                                                            pane.selected_message = msg_idx;
                                                        }
                                                    } else if let Some(&(prev_pos, _)) =
                                                        pane.message_lines.iter().rev().find(
                                                            |&&(pos, _)| pos + 2 < pane.scroll,
                                                        )
                                                    {
                                                        pane.scroll = prev_pos;
                                                    } else {
                                                        pane.scroll = 0;
                                                    }
                                                }
                                        }
                                        KeyCode::Char('J') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx)
                                                    && let Some(&(next_pos, _)) = pane
                                                        .message_lines
                                                        .iter()
                                                        .find(|&&(pos, _)| pos > pane.scroll + 2)
                                                    {
                                                        pane.scroll = next_pos;
                                                    }
                                        }
                                        KeyCode::Char('K') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    if let Some(&(prev_pos, _)) =
                                                        pane.message_lines.iter().rev().find(
                                                            |&&(pos, _)| pos + 2 < pane.scroll,
                                                        )
                                                    {
                                                        pane.scroll = prev_pos;
                                                    } else {
                                                        pane.scroll = 0;
                                                    }
                                                }
                                        }
                                        KeyCode::Char('y') => {
                                            if let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx)
                                                    && let Some(&(_, msg_idx)) = pane
                                                        .message_lines
                                                        .get(pane.selected_message)
                                                        && let Some(msg) =
                                                            pane.messages.get(msg_idx)
                                                        {
                                                            let content =
                                                                ui::extract_message_text(msg);
                                                            match arboard::Clipboard::new() {
                                                                Ok(mut clipboard) => {
                                                                    if clipboard
                                                                        .set_text(&content)
                                                                        .is_ok()
                                                                    {
                                                                        let len =
                                                                            content.chars().count();
                                                                        state.toast_message =
                                                                            Some(format!(
                                                                                "Copied ({len} chars)"
                                                                            ));
                                                                        state.toast_time = Some(
                                                                            std::time::Instant::now(
                                                                            ),
                                                                        );
                                                                    }
                                                                }
                                                                Err(_) => {
                                                                    state.toast_message = Some(
                                                                        "Clipboard unavailable"
                                                                            .to_string(),
                                                                    );
                                                                    state.toast_time = Some(
                                                                        std::time::Instant::now(),
                                                                    );
                                                                }
                                                            }
                                                        }
                                        }
                                        KeyCode::Char('i') => {
                                            state.show_conversation_detail = !state.show_conversation_detail;
                                        }
                                        KeyCode::Enter => {
                                            if state.active_pane_index.is_none() {
                                                open_conversation_in_pane(&mut state);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        } else if state.show_detail {
                            match key.code {
                                KeyCode::Esc
                                | KeyCode::Char('q')
                                | KeyCode::Enter
                                | KeyCode::Char('i') => {
                                    state.show_detail = false;
                                }
                                KeyCode::Char(' ') => {
                                    if let Some(group) =
                                        state.daily_groups.get(state.selected_day)
                                    {
                                        let sessions: Vec<_> = group
                                            .sessions
                                            .iter()
                                            .filter(|s| !s.is_subagent)
                                            .collect();
                                        if let Some(session) =
                                            sessions.get(state.selected_session)
                                        {
                                            state.pins.toggle(&session.file_path);
                                            if let Err(e) = state.pins.save() { state.toast_message = Some(format!("Pin save failed: {e}")); state.toast_time = Some(std::time::Instant::now()); }
                                            state.needs_draw = true;
                                        }
                                    }
                                }
                                KeyCode::Char('C') => {
                                    let no_loading_panes = state.panes.iter().all(|p| !p.loading);
                                    if no_loading_panes && state.panes.len() < MAX_PANES
                                        && let Some(group) =
                                            state.daily_groups.get(state.selected_day)
                                        {
                                            let sessions: Vec<_> = group
                                                .sessions
                                                .iter()
                                                .filter(|s| !s.is_subagent)
                                                .collect();
                                            if let Some(session) =
                                                sessions.get(state.selected_session)
                                            {
                                                state.panes.push(ConversationPane::load_from(&session.file_path));
                                                state.active_pane_index =
                                                    Some(state.panes.len() - 1);
                                                state.conv_list_mode = ConvListMode::Day;
                                                state.show_conversation = true;
                                                state.show_detail = false;
                                            }
                                        }
                                }
                                KeyCode::Char('S') => {
                                    if state.summary_task.is_none() {
                                        let session_clone = {
                                            state.daily_groups.get(state.selected_day).and_then(
                                                |group| {
                                                    let sessions: Vec<_> = group
                                                        .sessions
                                                        .iter()
                                                        .filter(|s| !s.is_subagent)
                                                        .collect();
                                                    sessions
                                                        .get(state.selected_session)
                                                        .map(|s| (*s).clone())
                                                },
                                            )
                                        };
                                        if let Some(session) = session_clone {
                                            state.generating_summary = true;
                                            state.show_summary = true;
                                            state.show_detail = false;
                                            state.summary_type = Some(SummaryType::Session(
                                                Box::new(session.clone()),
                                            ));
                                            let (tx, rx) = mpsc::channel();
                                            state.summary_task = Some(rx);
                                            thread::spawn(move || {
                                                let summary =
                                                    summary::generate_session_summary(&session);
                                                let _ = tx.send(summary);
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('r') => {
                                    if state.summary_task.is_none() {
                                        let session_clone = {
                                            state.daily_groups.get(state.selected_day).and_then(
                                                |group| {
                                                    let sessions: Vec<_> = group
                                                        .sessions
                                                        .iter()
                                                        .filter(|s| !s.is_subagent)
                                                        .collect();
                                                    sessions
                                                        .get(state.selected_session)
                                                        .map(|s| (*s).clone())
                                                },
                                            )
                                        };
                                        if let Some(session) = session_clone {
                                            state.generating_summary = true;
                                            state.show_summary = true;
                                            state.show_detail = false;
                                            state.summary_type = Some(SummaryType::Session(
                                                Box::new(session.clone()),
                                            ));
                                            let (tx, rx) = mpsc::channel();
                                            state.summary_task = Some(rx);
                                            thread::spawn(move || {
                                                let summary =
                                                    summary::generate_session_summary(&session);
                                                let _ = tx.send(summary);
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('R') => {
                                    let selected_day = state.selected_day;
                                    let selected_session = state.selected_session;

                                    let session_data = {
                                        state.daily_groups.get(selected_day).and_then(|group| {
                                            let session_indices: Vec<usize> = group
                                                .sessions
                                                .iter()
                                                .enumerate()
                                                .filter(|(_, s)| !s.is_subagent)
                                                .map(|(i, _)| i)
                                                .collect();
                                            session_indices
                                                .get(selected_session)
                                                .map(|&idx| (idx, group.sessions[idx].clone()))
                                        })
                                    };

                                    if let Some((actual_idx, session)) = session_data
                                        && state.updating_task.is_none() {
                                            let file_path = session.file_path.clone();
                                            let (tx, rx) = mpsc::channel();

                                            state.updating_session =
                                                Some((selected_day, selected_session));
                                            state.updating_task = Some((
                                                rx,
                                                file_path,
                                                selected_day,
                                                selected_session,
                                                actual_idx,
                                            ));

                                            thread::spawn(move || {
                                                let result = regenerate_jsonl_summary(&session);
                                                let _ = tx.send(result);
                                            });
                                        }
                                }
                                _ => {}
                            }
                        } else if state.search_mode {
                            match key.code {
                                KeyCode::Esc => {
                                    state.search_mode = false;
                                    state.search_input.clear();
                                    state.search_results.clear();
                                    state.search_selected = 0;
                                    state.search_task = None;
                                    state.searching = false;
                                    if let Some((tab, day, session, show_conv)) = state.search_saved_state.take() {
                                        state.tab = tab;
                                        state.selected_day = day;
                                        state.selected_session = session;
                                        state.show_conversation = show_conv;
                                    }
                                    state.search_preview_mode = false;
                                }
                                KeyCode::Enter => {
                                    if !state.search_results.is_empty() {
                                        let result = state.search_results[state.search_selected].clone();
                                        let query = state.search_input.text.clone();
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
                                        state.search_task = None;
                                        state.searching = false;
                                        open_conversation_in_pane(&mut state);
                                        if is_content
                                            && let Some(idx) = state.active_pane_index
                                                && let Some(pane) = state.panes.get_mut(idx) {
                                                    pane.search_input.set(query);
                                                    pane.search_mode = true;
                                                }
                                    }
                                }
                                KeyCode::Down => {
                                    if !state.search_results.is_empty() {
                                        state.search_selected = (state.search_selected + 1)
                                            % state.search_results.len();
                                    }
                                }
                                KeyCode::Up => {
                                    if !state.search_results.is_empty() {
                                        state.search_selected = state
                                            .search_selected
                                            .checked_sub(1)
                                            .unwrap_or(state.search_results.len() - 1);
                                    }
                                }
                                KeyCode::Backspace => {
                                    state.search_input.delete_back();
                                    state.search_results = search::perform_search(
                                        &state.daily_groups,
                                        &state.search_input.text,
                                    );
                                    state.search_selected = 0;
                                    start_content_search(&mut state);
                                }
                                KeyCode::Left => { state.search_input.move_left(); }
                                KeyCode::Right => { state.search_input.move_right(); }
                                KeyCode::Home => { state.search_input.move_home(); }
                                KeyCode::End => { state.search_input.move_end(); }
                                KeyCode::Char(c) => {
                                    state.search_input.insert_char(c);
                                    state.search_results = search::perform_search(
                                        &state.daily_groups,
                                        &state.search_input.text,
                                    );
                                    state.search_selected = 0;
                                    start_content_search(&mut state);
                                }
                                _ => {}
                            }
                        } else if state.show_dashboard_detail {
                            match key.code {
                                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                                    state.show_dashboard_detail = false;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if state.dashboard_scroll[state.dashboard_panel] > 0 {
                                        state.dashboard_scroll[state.dashboard_panel] -= 1;
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    let max_items = dashboard_max_items(&state);
                                    let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
                                    if *scroll + 1 < max_items {
                                        *scroll += 1;
                                    }
                                }
                                KeyCode::Left | KeyCode::Char('h') => {
                                    state.dashboard_panel = if state.dashboard_panel == 0 {
                                        6
                                    } else {
                                        state.dashboard_panel - 1
                                    };
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    state.dashboard_panel = if state.dashboard_panel >= 6 {
                                        0
                                    } else {
                                        state.dashboard_panel + 1
                                    };
                                }
                                _ => {}
                            }
                        } else if state.show_insights_detail {
                            match key.code {
                                KeyCode::Esc
                                | KeyCode::Char('q')
                                | KeyCode::Enter
                                | KeyCode::Char('i') => {
                                    state.show_insights_detail = false;
                                }
                                KeyCode::Left | KeyCode::Char('h') => {
                                    state.insights_panel = if state.insights_panel == 0 {
                                        3
                                    } else {
                                        state.insights_panel - 1
                                    };
                                    state.insights_detail_scroll = 0;
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    state.insights_panel = if state.insights_panel >= 3 {
                                        0
                                    } else {
                                        state.insights_panel + 1
                                    };
                                    state.insights_detail_scroll = 0;
                                }
                                KeyCode::Char('j') | KeyCode::Down => {
                                    state.insights_detail_scroll = state.insights_detail_scroll.saturating_add(1);
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    state.insights_detail_scroll =
                                        state.insights_detail_scroll.saturating_sub(1);
                                }
                                _ => {}
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('q') => {
                                    if state.ctrl_c_pressed {
                                        break;
                                    }
                                    state.ctrl_c_pressed = true;
                                    state.toast_message = Some("Press q again to quit".to_string());
                                    state.toast_time = Some(std::time::Instant::now());
                                    state.needs_draw = true;
                                }
                                KeyCode::Esc => {
                                    if state.daily_breakdown_focus {
                                        state.daily_breakdown_focus = false;
                                        state.daily_breakdown_scroll = 0;
                                    }
                                }
                                KeyCode::Char('x') => {
                                    if state.retention_warning.is_some()
                                        && !state.retention_warning_dismissed
                                    {
                                        state.retention_warning_dismissed = true;
                                    }
                                }
                                KeyCode::Char('?') => {
                                    state.show_help = true;
                                }
                                KeyCode::Char('f') => {
                                    state.show_filter_popup = true;
                                    state.filter_popup_selected = if matches!(
                                        state.period_filter,
                                        PeriodFilter::Custom(_, _)
                                    ) {
                                        PeriodFilter::ALL_VARIANTS.len()
                                    } else {
                                        PeriodFilter::ALL_VARIANTS
                                            .iter()
                                            .position(|&v| v == state.period_filter)
                                            .unwrap_or(0)
                                    };
                                }
                                KeyCode::Char('p') => {
                                    state.show_project_popup = true;
                                    state.project_popup_selected = match &state.project_filter {
                                        Some(name) => state
                                            .project_list
                                            .iter()
                                            .position(|(n, _, _)| n == name)
                                            .map_or(0, |i| i + 1),
                                        None => 0,
                                    };
                                    state.project_popup_scroll = 0;
                                }
                                KeyCode::Char(' ') => {
                                    if state.tab == Tab::Daily
                                        && !state.show_conversation
                                        && let Some(group) =
                                            state.daily_groups.get(state.selected_day)
                                        {
                                            let sessions: Vec<_> = group
                                                .sessions
                                                .iter()
                                                .filter(|s| !s.is_subagent)
                                                .collect();
                                            if let Some(session) =
                                                sessions.get(state.selected_session)
                                            {
                                                state.pins.toggle(&session.file_path);
                                                if let Err(e) = state.pins.save() { state.toast_message = Some(format!("Pin save failed: {e}")); state.toast_time = Some(std::time::Instant::now()); }
                                                state.needs_draw = true;
                                            }
                                        }
                                }
                                KeyCode::Char('m') => {
                                    if !state.pins.entries().is_empty() {
                                        state.conv_list_mode = ConvListMode::Pinned;
                                        state.selected_session = 0;
                                        state.tab = Tab::Daily;
                                        if !state.show_conversation {
                                            state.panes.clear();
                                            state.panes
                                                .push(ConversationPane::default());
                                            state.show_conversation = true;
                                        }
                                        state.active_pane_index = None;
                                        if state.panes.len() == 1 {
                                            preview_conversation_in_pane(&mut state);
                                        }
                                    }
                                }
                                KeyCode::Char('/') => {
                                    state.search_mode = true;
                                    state.search_input.move_end();
                                }
                                KeyCode::Tab
                                | KeyCode::Char('1')
                                | KeyCode::Char('2')
                                | KeyCode::Char('3') => {
                                    if state.show_summary || state.generating_summary {
                                        state.clear_summary();
                                    }
                                    state.show_dashboard_detail = false;
                                    state.show_insights_detail = false;
                                    state.show_detail = false;
                                    state.daily_breakdown_focus = false;
                                    state.daily_breakdown_scroll = 0;
                                    state.tab = match key.code {
                                        KeyCode::Char('1') => Tab::Dashboard,
                                        KeyCode::Char('2') => Tab::Daily,
                                        KeyCode::Char('3') => Tab::Insights,
                                        KeyCode::Tab => match state.tab {
                                            Tab::Dashboard => Tab::Daily,
                                            Tab::Daily => Tab::Insights,
                                            Tab::Insights => Tab::Dashboard,
                                        },
                                        _ => state.tab,
                                    };
                                }
                                KeyCode::Left | KeyCode::Char('h') => {
                                    if state.tab == Tab::Dashboard {
                                        state.dashboard_panel = if state.dashboard_panel == 0 {
                                            6
                                        } else {
                                            state.dashboard_panel - 1
                                        };
                                    } else if state.tab == Tab::Daily
                                        && state.selected_day
                                            < state.daily_groups.len().saturating_sub(1)
                                    {
                                        state.selected_day += 1;
                                        state.selected_session = 0;
                                        state.daily_breakdown_scroll = 0;
                                    } else if state.tab == Tab::Insights {
                                        state.insights_panel = if state.insights_panel == 0 {
                                            3
                                        } else {
                                            state.insights_panel - 1
                                        };
                                        state.insights_detail_scroll = 0;
                                    }
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    if state.tab == Tab::Dashboard {
                                        state.dashboard_panel = if state.dashboard_panel >= 6 {
                                            0
                                        } else {
                                            state.dashboard_panel + 1
                                        };
                                    } else if state.tab == Tab::Daily && state.selected_day > 0 {
                                        state.selected_day -= 1;
                                        state.selected_session = 0;
                                        state.daily_breakdown_scroll = 0;
                                    } else if state.tab == Tab::Insights {
                                        state.insights_panel = if state.insights_panel >= 3 {
                                            0
                                        } else {
                                            state.insights_panel + 1
                                        };
                                        state.insights_detail_scroll = 0;
                                    }
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    if state.tab == Tab::Dashboard {
                                        // Panel 5 (heatmap) uses inverted scroll: older dates are at top
                                        if state.dashboard_panel == 5 {
                                            let scroll = &mut state.dashboard_scroll[5];
                                            *scroll = scroll.saturating_add(1);
                                        } else {
                                            let scroll =
                                                &mut state.dashboard_scroll[state.dashboard_panel];
                                            if *scroll > 0 {
                                                *scroll -= 1;
                                            }
                                        }
                                    } else if state.tab == Tab::Daily {
                                        if state.daily_breakdown_focus {
                                            if state.daily_breakdown_scroll > 0 {
                                                state.daily_breakdown_scroll -= 1;
                                            }
                                        } else if state.selected_session > 0 {
                                            state.selected_session -= 1;
                                        }
                                    }
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    if state.tab == Tab::Dashboard {
                                        if state.dashboard_panel == 5 {
                                            let scroll = &mut state.dashboard_scroll[5];
                                            *scroll = scroll.saturating_sub(1);
                                        } else {
                                            let max_items = dashboard_max_items(&state);
                                            let scroll =
                                                &mut state.dashboard_scroll[state.dashboard_panel];
                                            if *scroll + 1 < max_items {
                                                *scroll += 1;
                                            }
                                        }
                                    } else if state.tab == Tab::Daily {
                                        if state.daily_breakdown_focus {
                                            if state.daily_breakdown_scroll
                                                < state.daily_breakdown_max_scroll
                                            {
                                                state.daily_breakdown_scroll += 1;
                                            }
                                        } else {
                                            let max = state
                                                .daily_groups
                                                .get(state.selected_day)
                                                .map_or(0, |g| {
                                                    g.sessions
                                                        .iter()
                                                        .filter(|s| !s.is_subagent)
                                                        .count()
                                                        .saturating_sub(1)
                                                });
                                            if state.selected_session < max {
                                                state.selected_session += 1;
                                            }
                                        }
                                    }
                                }
                                KeyCode::Enter
                                    if key.modifiers.contains(KeyModifiers::SHIFT)
                                        && state.tab == Tab::Daily
                                        && state.panes.iter().all(|p| !p.loading)
                                        && state.panes.len() < MAX_PANES =>
                                {
                                    if let Some(group) = state.daily_groups.get(state.selected_day)
                                    {
                                        let sessions: Vec<_> = group
                                            .sessions
                                            .iter()
                                            .filter(|s| !s.is_subagent)
                                            .collect();
                                        if let Some(session) = sessions.get(state.selected_session)
                                        {
                                            state.panes.push(ConversationPane::load_from(&session.file_path));
                                            state.active_pane_index = Some(state.panes.len() - 1);
                                            state.conv_list_mode = ConvListMode::Day;
                                            state.show_conversation = true;
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if state.tab == Tab::Daily {
                                        if state.daily_breakdown_focus {
                                            state.daily_breakdown_focus = false;
                                            state.daily_breakdown_scroll = 0;
                                        }
                                        open_conversation_in_pane(&mut state);
                                    } else if state.tab == Tab::Dashboard {
                                        state.show_dashboard_detail = true;
                                    } else if state.tab == Tab::Insights {
                                        state.show_insights_detail = true;
                                        state.insights_detail_scroll = 0;
                                    }
                                }
                                KeyCode::Char('C') => {
                                    let no_loading = state.panes.iter().all(|p| !p.loading);
                                    if state.tab == Tab::Daily
                                        && no_loading
                                        && state.panes.len() < MAX_PANES
                                        && let Some(group) =
                                            state.daily_groups.get(state.selected_day)
                                        {
                                            let sessions: Vec<_> = group
                                                .sessions
                                                .iter()
                                                .filter(|s| !s.is_subagent)
                                                .collect();
                                            if let Some(session) =
                                                sessions.get(state.selected_session)
                                            {
                                                state.panes.push(ConversationPane::load_from(&session.file_path));
                                                state.active_pane_index =
                                                    Some(state.panes.len() - 1);
                                                state.conv_list_mode = ConvListMode::Day;
                                                state.show_conversation = true;
                                            }
                                        }
                                }
                                KeyCode::Char('S') => {
                                    if state.tab == Tab::Daily && state.summary_task.is_none() {
                                        let group_clone =
                                            state.daily_groups.get(state.selected_day).cloned();
                                        if let Some(group) = group_clone {
                                            state.generating_summary = true;
                                            state.show_summary = true;
                                            state.show_detail = false;
                                            state.summary_type =
                                                Some(SummaryType::Day(group.clone()));
                                            let (tx, rx) = mpsc::channel();
                                            state.summary_task = Some(rx);
                                            thread::spawn(move || {
                                                let summary = summary::generate_day_summary(&group);
                                                let _ = tx.send(summary);
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('s') => {
                                    if state.tab == Tab::Daily && state.summary_task.is_none() {
                                        let session_clone = {
                                            state.daily_groups.get(state.selected_day).and_then(
                                                |group| {
                                                    let sessions: Vec<_> = group
                                                        .sessions
                                                        .iter()
                                                        .filter(|s| !s.is_subagent)
                                                        .collect();
                                                    sessions
                                                        .get(state.selected_session)
                                                        .map(|s| (*s).clone())
                                                },
                                            )
                                        };
                                        if let Some(session) = session_clone {
                                            state.generating_summary = true;
                                            state.show_summary = true;
                                            state.show_detail = false;
                                            state.summary_type = Some(SummaryType::Session(
                                                Box::new(session.clone()),
                                            ));
                                            let (tx, rx) = mpsc::channel();
                                            state.summary_task = Some(rx);
                                            thread::spawn(move || {
                                                let summary =
                                                    summary::generate_session_summary(&session);
                                                let _ = tx.send(summary);
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('r') => {
                                    if state.tab == Tab::Daily && state.summary_task.is_none() {
                                        let session_clone = {
                                            state.daily_groups.get(state.selected_day).and_then(
                                                |group| {
                                                    let sessions: Vec<_> = group
                                                        .sessions
                                                        .iter()
                                                        .filter(|s| !s.is_subagent)
                                                        .collect();
                                                    sessions
                                                        .get(state.selected_session)
                                                        .map(|s| (*s).clone())
                                                },
                                            )
                                        };
                                        if let Some(session) = session_clone {
                                            state.generating_summary = true;
                                            state.show_summary = true;
                                            state.show_detail = false;
                                            state.summary_type = Some(SummaryType::Session(
                                                Box::new(session.clone()),
                                            ));
                                            let (tx, rx) = mpsc::channel();
                                            state.summary_task = Some(rx);
                                            thread::spawn(move || {
                                                let summary =
                                                    summary::regenerate_session_summary(&session);
                                                let _ = tx.send(summary);
                                            });
                                        }
                                    }
                                }
                                KeyCode::Char('R') => {
                                    if state.tab == Tab::Daily {
                                        let selected_day = state.selected_day;
                                        let selected_session = state.selected_session;

                                        let session_data = {
                                            state.daily_groups.get(selected_day).and_then(|group| {
                                                let session_indices: Vec<usize> = group
                                                    .sessions
                                                    .iter()
                                                    .enumerate()
                                                    .filter(|(_, s)| !s.is_subagent)
                                                    .map(|(i, _)| i)
                                                    .collect();
                                                session_indices
                                                    .get(selected_session)
                                                    .map(|&idx| (idx, group.sessions[idx].clone()))
                                            })
                                        };

                                        if let Some((actual_idx, session)) = session_data
                                            && state.updating_task.is_none() {
                                                let file_path = session.file_path.clone();
                                                let (tx, rx) = mpsc::channel();

                                                state.updating_session =
                                                    Some((selected_day, selected_session));
                                                state.updating_task = Some((
                                                    rx,
                                                    file_path,
                                                    selected_day,
                                                    selected_session,
                                                    actual_idx,
                                                ));

                                                thread::spawn(move || {
                                                    let result = regenerate_jsonl_summary(&session);
                                                    let _ = tx.send(result);
                                                });
                                            }
                                    }
                                }
                                KeyCode::Char('b') => {
                                    if state.tab == Tab::Daily {
                                        state.daily_breakdown_focus = !state.daily_breakdown_focus;
                                        if state.daily_breakdown_focus {
                                            state.daily_breakdown_scroll = 0;
                                        }
                                    }
                                }
                                KeyCode::Char('t') => {
                                    if state.tab == Tab::Daily && !state.daily_groups.is_empty() {
                                        state.selected_day = 0;
                                        state.selected_session = 0;
                                    }
                                }
                                KeyCode::Char('i') => {
                                    if state.tab == Tab::Daily {
                                        state.show_detail = true;
                                    } else if state.tab == Tab::Insights {
                                        state.show_insights_detail = true;
                                        state.insights_detail_scroll = 0;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
            state.needs_draw = true;
        }
    }
    Ok(())
}

fn dismiss_overlay(state: &mut AppState) {
    if state.show_help {
        state.show_help = false;
        return;
    }
    if state.show_filter_popup {
        if state.filter_input_mode {
            state.filter_input_mode = false;
            state.filter_input.clear();
            state.filter_input_error = false;
        } else {
            state.show_filter_popup = false;
        }
        return;
    }
    if state.show_project_popup {
        state.show_project_popup = false;
        return;
    }
    if state.show_summary {
        state.clear_summary();
        return;
    }
    if state.search_mode {
        state.search_mode = false;
        state.search_input.clear();
        state.search_results.clear();
        state.search_selected = 0;
        state.search_task = None;
        state.searching = false;
        if let Some((tab, day, session, show_conv)) = state.search_saved_state.take() {
            state.tab = tab;
            state.selected_day = day;
            state.selected_session = session;
            state.show_conversation = show_conv;
        }
        state.search_preview_mode = false;
        return;
    }
    if state.show_detail {
        state.show_detail = false;
        return;
    }
    if state.show_dashboard_detail {
        state.show_dashboard_detail = false;
        return;
    }
    if state.show_insights_detail {
        state.show_insights_detail = false;
        return;
    }
    if state.show_conversation_detail {
        state.show_conversation_detail = false;
        return;
    }
    if state.show_conversation {
        if state.panes.len() > 1 {
            if let Some(idx) = state.active_pane_index {
                if state.panes.get(idx).is_some_and(|p| p.search_mode) {
                    state.panes[idx].search_mode = false;
                    return;
                }
                state.panes.remove(idx);
                if state.panes.is_empty() {
                    state.show_conversation = false;
                    state.active_pane_index = None;
                    state.conv_list_mode = ConvListMode::Day;
                } else {
                    state.active_pane_index = Some(idx.min(state.panes.len() - 1));
                }
            } else if !state.panes.is_empty() {
                state.active_pane_index = Some(0);
            }
        } else {
            let has_search = state.panes.first().is_some_and(|p| p.search_mode);
            if has_search {
                if let Some(pane) = state.panes.first_mut() {
                    pane.search_mode = false;
                }
                return;
            }
            state.show_conversation = false;
            state.panes.clear();
            state.active_pane_index = None;
            state.conv_list_mode = ConvListMode::Day;
        }
        if !state.show_conversation {
            state.conv_list_mode = ConvListMode::Day;
        }
        return;
    }
    if state.daily_breakdown_focus {
        state.daily_breakdown_focus = false;
        state.daily_breakdown_scroll = 0;
    }
}

fn handle_double_click(state: &mut AppState, column: u16, row: u16) {
    if state.show_summary {
        if let Some(popup_area) = state.summary_popup_area
            && !in_area(column, row, &popup_area) {
                state.clear_summary();
            }
        return;
    }

    if state.show_filter_popup && !state.filter_input_mode {
        if state.filter_popup_selected < PeriodFilter::ALL_VARIANTS.len() {
            state.period_filter = PeriodFilter::ALL_VARIANTS[state.filter_popup_selected];
            state.apply_filter();
            state.show_filter_popup = false;
        } else {
            state.filter_input_mode = true;
            let text = match state.period_filter {
                PeriodFilter::Custom(s, Some(e)) if s == e => s.format("%Y-%m-%d").to_string(),
                PeriodFilter::Custom(s, Some(e)) => {
                    format!("{}..{}", s.format("%Y-%m-%d"), e.format("%Y-%m-%d"))
                }
                PeriodFilter::Custom(s, None) => s.format("%Y-%m-%d").to_string(),
                _ => String::new(),
            };
            state.filter_input.set(text);
            state.filter_input_error = false;
        }
        return;
    }

    if state.show_project_popup {
        if state.project_popup_selected == 0 {
            state.project_filter = None;
        } else if let Some((name, _, _)) = state.project_list.get(state.project_popup_selected - 1)
        {
            state.project_filter = Some(name.clone());
        }
        state.apply_filter();
        state.show_project_popup = false;
        return;
    }

    if state.search_mode && !state.search_results.is_empty() {
        let result = &state.search_results[state.search_selected];
        state.selected_day = result.day_idx;
        state.selected_session = result.session_idx;
        state.tab = Tab::Daily;
        state.search_mode = false;
        state.search_task = None;
        state.searching = false;
        return;
    }

    if has_blocking_popup(state) {
        dismiss_overlay(state);
        return;
    }

    if state.show_conversation && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.session_list_area
            && in_area(column, row, &area) {
                let relative_y = (row - area.y) as usize;
                let clicked_idx = scroll + relative_y / item_height;
                let session_count = get_conv_session_count(state);
                if clicked_idx < session_count {
                    state.selected_session = clicked_idx;
                    open_conversation_in_pane(state);
                }
                return;
            }

    if !state.show_conversation && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.session_list_area
            && in_area(column, row, &area) {
                let relative_y = (row - area.y) as usize;
                let clicked_idx = scroll + relative_y / item_height;
                if let Some(group) = state.daily_groups.get(state.selected_day) {
                    let session_count = group.sessions.iter().filter(|s| !s.is_subagent).count();
                    if clicked_idx < session_count {
                        state.selected_session = clicked_idx;
                        open_conversation_in_pane(state);
                    }
                }
                return;
            }

    if !state.show_conversation && state.tab == Tab::Daily
        && let Some(area) = state.breakdown_panel_area
            && in_area(column, row, &area) {
                state.daily_breakdown_focus = true;
                state.daily_breakdown_scroll = 0;
                return;
            }

    if !state.show_conversation && state.tab == Tab::Dashboard {
        for (idx, area) in state.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.dashboard_panel = idx;
                state.show_dashboard_detail = true;
                return;
            }
        }
    }

    if !state.show_conversation && state.tab == Tab::Insights {
        for (idx, area) in state.insights_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.insights_panel = idx;
                state.show_insights_detail = true;
                state.insights_detail_scroll = 0;
                return;
            }
        }
    }
}

fn spawn_load_conversation(
    file_path: &std::path::Path,
) -> mpsc::Receiver<Vec<ConversationMessage>> {
    let fp = file_path.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let messages = ui::load_conversation(&fp).unwrap_or_default();
        let _ = tx.send(messages);
    });
    rx
}

fn get_conv_session_file(state: &AppState, idx: usize) -> Option<std::path::PathBuf> {
    match state.conv_list_mode {
        ConvListMode::Day => state
            .daily_groups
            .get(state.selected_day)
            .and_then(|g| g.sessions.iter().filter(|s| !s.is_subagent).nth(idx))
            .map(|s| s.file_path.clone()),
        ConvListMode::Pinned => state.pins.entries().get(idx).map(|e| e.path.clone()),
        ConvListMode::All => state
            .original_daily_groups
            .iter()
            .flat_map(|g| g.sessions.iter().filter(|s| !s.is_subagent))
            .nth(idx)
            .map(|s| s.file_path.clone()),
    }
}

fn get_conv_session_count(state: &AppState) -> usize {
    match state.conv_list_mode {
        ConvListMode::Day => state
            .daily_groups
            .get(state.selected_day)
            .map_or(0, |g| g.sessions.iter().filter(|s| !s.is_subagent).count()),
        ConvListMode::Pinned => state.pins.entries().len(),
        ConvListMode::All => state
            .original_daily_groups
            .iter()
            .flat_map(|g| g.sessions.iter().filter(|s| !s.is_subagent))
            .count(),
    }
}

fn preview_conversation_in_pane(state: &mut AppState) {
    let saved = state.active_pane_index;
    open_conversation_in_pane(state);
    state.active_pane_index = saved;
}

fn open_conversation_in_pane(state: &mut AppState) {
    let no_loading = state.panes.iter().all(|p| !p.loading);
    if !no_loading || state.panes.len() >= MAX_PANES {
        return;
    }

    let Some(file_path) = get_conv_session_file(state, state.selected_session) else {
        return;
    };

    let target_idx = state
        .panes
        .iter()
        .position(|p| p.file_path.is_none())
        .unwrap_or_else(|| state.active_pane_index.unwrap_or(0));

    let new_pane = ConversationPane::load_from(&file_path);
    if target_idx < state.panes.len() {
        state.panes[target_idx] = new_pane;
    } else {
        state.panes.push(new_pane);
    }

    state.active_pane_index = Some(target_idx);
    state.show_conversation = true;
}

fn extract_selected_text_from_buffer(
    sel: &(u16, u16, u16, u16),
    buffer: &ratatui::buffer::Buffer,
    conv_area: Option<ratatui::layout::Rect>,
    wrap_flags: Option<&[bool]>,
    conv_scroll: usize,
) -> String {
    let (sc, sr, ec, er) = *sel;
    let buf_area = buffer.area;

    let (start_col, start_row, end_col, end_row) = if (sr, sc) <= (er, ec) {
        (sc, sr, ec, er)
    } else {
        (ec, er, sc, sr)
    };

    let clamp = conv_area.filter(|ca| {
        start_row >= ca.y
            && start_row < ca.y + ca.height
            && start_col >= ca.x
            && start_col < ca.x + ca.width
    });

    let mut lines: Vec<String> = Vec::new();
    let mut line_rows: Vec<u16> = Vec::new();
    for row in start_row..=end_row {
        if row < buf_area.y || row >= buf_area.y + buf_area.height {
            continue;
        }
        if let Some(ca) = clamp
            && (row < ca.y || row >= ca.y + ca.height) {
                continue;
            }
        let col_start = if row == start_row {
            start_col
        } else {
            clamp.map_or(buf_area.x, |ca| ca.x)
        };
        let col_end = if row == end_row {
            end_col
        } else {
            clamp.map_or(buf_area.x + buf_area.width - 1, |ca| ca.x + ca.width - 1)
        };

        let mut line = String::new();
        let mut col = col_start;
        let mut skip_next = false;
        while col <= col_end && col < buf_area.x + buf_area.width {
            if skip_next {
                skip_next = false;
                col += 1;
                continue;
            }
            let cell = &buffer[(col, row)];
            let sym = cell.symbol();
            line.push_str(sym);
            if sym
                .chars()
                .next()
                .is_some_and(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) > 1)
            {
                skip_next = true;
            }
            col += 1;
        }
        lines.push(line.trim_end().to_string());
        line_rows.push(row);
    }

    while lines.last().is_some_and(std::string::String::is_empty) {
        lines.pop();
        line_rows.pop();
    }

    if let (Some(ca), Some(flags)) = (clamp, wrap_flags) {
        if !lines.is_empty() {
            let continuation_flags: Vec<bool> = line_rows
                .iter()
                .map(|&row| {
                    let flag_idx = conv_scroll + (row - ca.y) as usize;
                    flags.get(flag_idx).copied().unwrap_or(false)
                })
                .collect();
            return join_conversation_lines(&lines, &continuation_flags);
        }
    } else if clamp.is_some() && !lines.is_empty() {
        let no_flags = vec![false; lines.len()];
        return join_conversation_lines(&lines, &no_flags);
    }

    lines.join("\n")
}

fn join_conversation_lines(lines: &[String], wrap_continuation: &[bool]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    let strip_prefix = |s: &str| -> String {
        if let Some(stripped) = s.strip_prefix("▶ ") {
            stripped.to_string()
        } else if let Some(stripped) = s.strip_prefix("  ") {
            stripped.to_string()
        } else {
            s.to_string()
        }
    };

    let mut result = String::new();
    let mut i = 0;

    while i < lines.len() {
        let stripped = strip_prefix(&lines[i]);
        result.push_str(&stripped);

        if i + 1 < lines.len() {
            let next_is_continuation = wrap_continuation.get(i + 1).copied().unwrap_or(false);
            if next_is_continuation {
                let next_stripped = strip_prefix(&lines[i + 1]);
                if !next_stripped.is_empty() {
                    result.push(' ');
                } else {
                    result.push('\n');
                }
            } else {
                result.push('\n');
            }
        }

        i += 1;
    }

    result
}

fn handle_mouse_click(state: &mut AppState, column: u16, row: u16) {
    // Popups are topmost - check them first
    if state.show_filter_popup && !state.filter_input_mode {
        if let Some(area) = state.filter_popup_area
            && in_area(column, row, &area) {
                let relative_row = (row - area.y).saturating_sub(1) as usize;
                let total_items = PeriodFilter::ALL_VARIANTS.len() + 1;
                if relative_row < total_items {
                    state.filter_popup_selected = relative_row;
                }
                return;
            }
        dismiss_overlay(state);
        return;
    }

    if state.show_project_popup {
        if let Some(area) = state.project_popup_area
            && in_area(column, row, &area) {
                let relative_row = (row - area.y).saturating_sub(1) as usize;
                let clicked_idx = state.project_popup_scroll + relative_row;
                let total = state.project_list.len() + 1;
                if clicked_idx < total {
                    state.project_popup_selected = clicked_idx;
                }
                return;
            }
        dismiss_overlay(state);
        return;
    }

    if state.search_mode && !state.search_results.is_empty()
        && let Some(area) = state.search_results_area
            && in_area(column, row, &area) {
                let item_height = 2usize;
                let visible_items = area.height.saturating_sub(2) as usize / item_height;
                let scroll_start = if visible_items > 0 && state.search_selected >= visible_items
                {
                    state.search_selected - visible_items + 1
                } else {
                    0
                };
                let relative_row = (row - area.y).saturating_sub(1) as usize;
                let clicked_idx = scroll_start + relative_row / item_height;
                if clicked_idx < state.search_results.len() {
                    state.search_selected = clicked_idx;
                }
                return;
            }

    // All other popups: single click does nothing, close via keyboard
    if has_blocking_popup(state) {
        return;
    }

    // Check if click is on help trigger button
    if let Some(area) = state.help_trigger
        && in_area(column, row, &area) {
            state.show_help = true;
            state.help_scroll = 0;
            return;
        }

    // Check if click is on filter/project trigger buttons in tab bar
    if !state.show_conversation {
        if let Some(area) = state.filter_popup_area_trigger
            && in_area(column, row, &area) {
                state.show_filter_popup = true;
                state.filter_popup_selected = 0;
                return;
            }
        if let Some(area) = state.project_popup_area_trigger
            && in_area(column, row, &area) {
                state.rebuild_project_list();
                state.show_project_popup = true;
                state.project_popup_selected = 0;
                state.project_popup_scroll = 0;
                return;
            }
        if let Some(area) = state.pin_view_trigger
            && in_area(column, row, &area) && !state.pins.entries().is_empty() {
                state.conv_list_mode = ConvListMode::Pinned;
                state.selected_session = 0;
                state.tab = Tab::Daily;
                state.panes.clear();
                state.panes.push(ConversationPane::default());
                state.show_conversation = true;
                state.active_pane_index = None;
                open_conversation_in_pane(state);
                state.active_pane_index = None;
                return;
            }
    }

    // Check if click is on a tab (only when not showing conversation)
    if !state.show_conversation {
        let clicked_tab = state.tab_areas.iter().find_map(|(tab, area)| {
            if in_area(column, row, area) {
                Some(*tab)
            } else {
                None
            }
        });
        if let Some(tab) = clicked_tab {
            state.show_dashboard_detail = false;
            state.show_insights_detail = false;
            state.show_detail = false;
            state.daily_breakdown_focus = false;
            state.daily_breakdown_scroll = 0;
            if state.show_summary || state.generating_summary {
                state.clear_summary();
            }
            state.tab = tab;
            return;
        }
    }

    // Check if click is on session list title (mode toggle) in conversation view
    if state.show_conversation && state.tab == Tab::Daily
        && let Some((area, _, _)) = state.session_list_area
            && column >= area.x
                && column < area.x + area.width
                && area.y > 0
                && row == area.y - 1
            {
                state.conv_list_mode = match state.conv_list_mode {
                    ConvListMode::Day => {
                        if state.pins.entries().is_empty() {
                            ConvListMode::All
                        } else {
                            ConvListMode::Pinned
                        }
                    }
                    ConvListMode::Pinned => ConvListMode::All,
                    ConvListMode::All => ConvListMode::Day,
                };
                state.selected_session = 0;
                state.active_pane_index = None;
                return;
            }

    // Check if click is on session list (in conversation view)
    if state.show_conversation && state.tab == Tab::Daily
        && let Some((area, scroll, item_height)) = state.session_list_area
            && column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                let relative_y = (row - area.y) as usize;
                let clicked_idx = scroll + relative_y / item_height;
                let session_count = get_conv_session_count(state);
                if clicked_idx < session_count {
                    state.selected_session = clicked_idx;
                }
                state.active_pane_index = None;
                return;
            }

    // Check if click is on a pane (in conversation view)
    if state.show_conversation {
        for (idx, area) in state.pane_areas.iter().enumerate() {
            if in_area(column, row, area) {
                state.active_pane_index = Some(idx);
                let content_y = area.y + 1;
                if row >= content_y
                    && let Some(pane) = state.panes.get_mut(idx)
                        && !pane.message_lines.is_empty() {
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


    // Check if click is on a dashboard panel
    if state.tab == Tab::Dashboard && !state.show_conversation {
        for (idx, area) in state.dashboard_panel_areas.iter().enumerate() {
            if column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                state.dashboard_panel = idx;
                return;
            }
        }
    }

    // Check if click is on an insights panel
    if state.tab == Tab::Insights && !state.show_conversation {
        for (idx, area) in state.insights_panel_areas.iter().enumerate() {
            if column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                state.insights_panel = idx;
                return;
            }
        }
    }

    // Check if click is on Daily header (left/right navigation)
    if state.tab == Tab::Daily && !state.show_conversation
        && let Some(area) = state.daily_header_area
            && in_area(column, row, &area) {
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
    if state.tab == Tab::Daily && !state.show_conversation
        && let Some((area, scroll, item_height)) = state.session_list_area
            && column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                let relative_y = (row - area.y) as usize;
                let clicked_idx = scroll + relative_y / item_height;
                if let Some(group) = state.daily_groups.get(state.selected_day) {
                    let session_count = group.sessions.iter().filter(|s| !s.is_subagent).count();
                    if clicked_idx < session_count {
                        state.selected_session = clicked_idx;
                    }
                }
                return;
            }

    // Click on empty area dismisses topmost overlay
    dismiss_overlay(state);
}

fn in_area(column: u16, row: u16, area: &ratatui::layout::Rect) -> bool {
    column >= area.x && column < area.x + area.width && row >= area.y && row < area.y + area.height
}

fn has_blocking_popup(state: &AppState) -> bool {
    state.show_help
        || state.show_summary
        || state.show_detail
        || state.show_dashboard_detail
        || state.show_insights_detail
        || state.show_conversation_detail
}

fn dashboard_max_items(state: &AppState) -> usize {
    match state.dashboard_panel {
        0 => state.daily_costs.len(),
        1 => state.stats.project_stats.len(),
        2 => state.model_costs.len(),
        3 => state.stats.tool_usage.len(),
        4 => {
            let known = state.stats.language_usage.iter().filter(|(l, _)| l.as_str() != "Other").count();
            let other = state.stats.extension_usage.iter().filter(|(ext, _)| {
                crate::aggregator::StatsAggregator::language_for_extension(ext) == "Other"
            }).count();
            known + other
        }
        5 => state.daily_groups.len(),
        6 => 24,
        _ => 0,
    }
}

fn handle_mouse_scroll(state: &mut AppState, column: u16, row: u16, up: bool) {
    if state.show_filter_popup {
        let max = PeriodFilter::ALL_VARIANTS.len();
        if up {
            state.filter_popup_selected = state.filter_popup_selected.saturating_sub(1);
        } else if state.filter_popup_selected < max {
            state.filter_popup_selected += 1;
        }
        return;
    }

    if state.show_project_popup {
        let max = state.project_list.len().saturating_sub(1);
        if up {
            state.project_popup_selected = state.project_popup_selected.saturating_sub(1);
        } else if state.project_popup_selected < max {
            state.project_popup_selected += 1;
        }
        return;
    }

    if state.show_summary {
        if up {
            state.summary_scroll = state.summary_scroll.saturating_sub(SCROLL_LINES);
        } else {
            state.summary_scroll += SCROLL_LINES;
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

    if state.show_help {
        if up {
            state.help_scroll = state.help_scroll.saturating_sub(1);
        } else {
            state.help_scroll = state.help_scroll.saturating_add(1);
        }
        return;
    }

    if state.show_detail {
        return;
    }

    if state.show_insights_detail {
        if up {
            state.insights_detail_scroll =
                state.insights_detail_scroll.saturating_sub(SCROLL_LINES);
        } else {
            state.insights_detail_scroll = state.insights_detail_scroll.saturating_add(SCROLL_LINES);
        }
        return;
    }

    if state.show_dashboard_detail {
        if up {
            state.dashboard_scroll[state.dashboard_panel] =
                state.dashboard_scroll[state.dashboard_panel].saturating_sub(1);
        } else {
            let max_items = dashboard_max_items(state);
            let scroll = &mut state.dashboard_scroll[state.dashboard_panel];
            if *scroll + 1 < max_items {
                *scroll += 1;
            }
        }
        return;
    }

    if state.show_conversation {
        if let Some((area, _, _)) = state.session_list_area
            && in_area(column, row, &area) {
                let max = get_conv_session_count(state).saturating_sub(1);
                if up {
                    state.selected_session = state.selected_session.saturating_sub(1);
                } else if state.selected_session < max {
                    state.selected_session += 1;
                }
                return;
            }

        for (idx, area) in state.pane_areas.iter().enumerate() {
            if in_area(column, row, area) {
                if let Some(pane) = state.panes.get_mut(idx) {
                    if up {
                        pane.scroll = pane.scroll.saturating_sub(SCROLL_LINES);
                    } else {
                        pane.scroll += SCROLL_LINES;
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
        for (idx, area) in state.dashboard_panel_areas.iter().enumerate() {
            if in_area(column, row, area) {
                let (scroll_up, scroll_down) = if idx == 5 { (!up, up) } else { (up, !up) };
                if scroll_up {
                    state.dashboard_scroll[idx] = state.dashboard_scroll[idx].saturating_sub(1);
                } else if scroll_down {
                    let saved = state.dashboard_panel;
                    state.dashboard_panel = idx;
                    let max_items = dashboard_max_items(state);
                    state.dashboard_panel = saved;
                    if state.dashboard_scroll[idx] + 1 < max_items {
                        state.dashboard_scroll[idx] += 1;
                    }
                }
                return;
            }
        }
    }

    if state.tab == Tab::Daily {
        if state.daily_breakdown_focus {
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
                .map_or(0, |g| {
                    g.sessions
                        .iter()
                        .filter(|s| !s.is_subagent)
                        .count()
                        .saturating_sub(1)
                });
            if up {
                state.selected_session = state.selected_session.saturating_sub(1);
            } else if state.selected_session < max {
                state.selected_session += 1;
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn load_data(limit: usize) -> anyhow::Result<LoadedData> {
    let files = FileDiscovery::find_jsonl_files_with_limit(limit)?;
    let file_count = files.len();
    let cache = crate::infrastructure::Cache::load().unwrap_or_else(|_| crate::infrastructure::Cache::new_empty());
    let cache_for_grouper = Some(cache.clone());
    let (stats, cache_stats) = StatsAggregator::aggregate_with_shared_cache(&files, cache);
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &cache_for_grouper);

    let calculator = CostCalculator::global();
    let model_costs = calculator.calculate_costs_by_model(&stats.model_tokens);
    let aggregated_model_tokens = CostCalculator::aggregate_tokens_by_model(&stats.model_tokens);
    let models_without_pricing = calculator.models_without_pricing(&stats.model_tokens);
    let cost: f64 = model_costs.iter().map(|(_, c)| c).sum();

    let daily_costs: Vec<(NaiveDate, f64)> = daily_groups
        .iter()
        .map(|group| {
            let day_cost: f64 = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .flat_map(|s| {
                    s.day_tokens_by_model.iter().map(|(model, tokens)| {
                        calculator
                            .calculate_cost(tokens, Some(model.as_str()))
                            .unwrap_or(0.0)
                    })
                })
                .sum();
            (group.date, day_cost)
        })
        .collect();

    let cost_without_subagents: f64 = daily_costs.iter().map(|(_, c)| c).sum();

    Ok(LoadedData {
        stats,
        cost,
        cost_without_subagents,
        model_costs,
        aggregated_model_tokens,
        models_without_pricing,
        daily_groups,
        daily_costs,
        file_count,
        cache_stats,
    })
}

fn start_content_search(state: &mut AppState) {
    state.search_task = None;
    state.searching = false;

    if state.search_input.text.len() < 2 {
        return;
    }

    let query = state.search_input.text.clone();

    if let Some(ref index) = state.search_index {
        let results = index.search(&query, 200, 50);
        for result in results {
            if !state
                .search_results
                .iter()
                .any(|r| r.day_idx == result.day_idx && r.session_idx == result.session_idx)
            {
                state.search_results.push(result);
            }
        }
        return;
    }

    let groups_data: Vec<(usize, Vec<(usize, PathBuf)>)> = state
        .daily_groups
        .iter()
        .enumerate()
        .map(|(day_idx, group)| {
            let sessions: Vec<_> = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .enumerate()
                .map(|(session_idx, s)| (session_idx, s.file_path.clone()))
                .collect();
            (day_idx, sessions)
        })
        .collect();

    let (tx, rx) = mpsc::channel();
    state.searching = true;
    state.search_task = Some((rx, query.clone()));

    std::thread::spawn(move || {
        let mut results = Vec::new();
        let mut searched = 0;
        let max_files = 100;

        for (day_idx, sessions) in &groups_data {
            for (session_idx, file_path) in sessions {
                if searched >= max_files {
                    break;
                }
                if let Some(snippet) = search::search_session_content(file_path, &query) {
                    results.push(search::SearchResult {
                        day_idx: *day_idx,
                        session_idx: *session_idx,
                        snippet: Some(snippet),
                        match_type: search::SearchMatchType::Content,
                    });
                }
                searched += 1;
            }
            if searched >= max_files {
                break;
            }
        }

        let _ = tx.send(results);
    });
}

fn start_index_build(state: &mut AppState) {
    use std::sync::Arc;

    let groups: Vec<DailyGroup> = state.daily_groups.clone();
    let (tx, rx) = mpsc::channel();
    state.index_build_task = Some(rx);

    std::thread::spawn(move || {
        if let Ok(index) = infrastructure::SearchIndex::update_or_build(&groups) {
            let _ = tx.send(Arc::new(index));
        }
    });
}

pub(crate) use text::format_number;

fn regenerate_jsonl_summary(session: &crate::aggregator::SessionInfo) -> Result<String, String> {
    use std::process::Command;

    let (user_requests, files_modified, _) = summary::extract_session_details(&session.file_path);

    if user_requests.is_empty() {
        return Err("No conversation to summarize".to_string());
    }

    let mut context = String::new();
    context.push_str(&format!("Project: {}\n", session.project_name));
    context.push_str("\nUser requests:\n");
    for req in user_requests.iter().take(5) {
        let truncated: String = req.chars().take(100).collect();
        context.push_str(&format!("- {truncated}\n"));
    }
    if !files_modified.is_empty() {
        context.push_str("\nFiles modified:\n");
        for file in files_modified.iter().take(10) {
            context.push_str(&format!("- {file}\n"));
        }
    }

    let prompt = format!(
        "Based on this Claude Code session, generate a VERY SHORT summary (max 60 chars).\n\
        Format: Brief description of what was done (e.g. \"Fix login bug and add tests\")\n\
        Use emoji if appropriate. Reply with ONLY the summary, nothing else.\n\n\
        ---\n{context}\n---"
    );

    use std::io::Read;
    use std::process::Stdio;
    use std::thread;
    use std::time::{Duration, Instant};

    let mut child = Command::new("claude")
        .args(["-p", &prompt, "--model", SUMMARY_MODEL])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn claude: {e}"))?;

    let timeout = Duration::from_secs(60);
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Timeout: claude command took too long".to_string());
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Error waiting for claude: {e}")),
        }
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }

    let summary = stdout.trim().to_string();
    if summary.is_empty() && !stderr.is_empty() {
        return Err(format!("claude error: {}", stderr.trim()));
    }

    let summary = if unicode_width::UnicodeWidthStr::width(summary.as_str()) > 80 {
        let truncated = crate::ui::truncate_to_display_width(&summary, 77);
        format!("{truncated}...")
    } else {
        summary
    };
    Ok(summary)
}

fn update_jsonl_summary(file_path: &std::path::Path, new_summary: &str) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    let file = std::fs::File::open(file_path).map_err(|e| format!("Failed to open file: {e}"))?;
    let reader = BufReader::new(file);

    let mut last_leaf_uuid: Option<String> = None;

    for line_result in reader.lines() {
        let line = line_result.map_err(|e| format!("Read error: {e}"))?;
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
            && value.get("uuid").is_some() {
                last_leaf_uuid = value
                    .get("uuid")
                    .and_then(|v| v.as_str())
                    .map(std::string::ToString::to_string);
            }
    }

    let leaf_uuid = last_leaf_uuid.unwrap_or_default();
    let new_entry = serde_json::json!({
        "type": "summary",
        "summary": new_summary,
        "leafUuid": leaf_uuid
    });

    let mut file = OpenOptions::new()
        .append(true)
        .open(file_path)
        .map_err(|e| format!("Failed to open file for append: {e}"))?;

    let json_str = serde_json::to_string(&new_entry)
        .map_err(|e| format!("JSON serialization error: {e}"))?;
    writeln!(file, "{json_str}").map_err(|e| format!("Write error: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{
        parse_text_with_code_blocks, render_text_with_highlighting,
        render_tool_result_with_highlighting, TextSegment,
    };
    use chrono::Datelike;
    use std::time::Instant;

    #[test]
    fn test_format_number() {
        // Under 1K
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(500), "500");
        assert_eq!(format_number(999), "999");

        // 1K - 10K (2 decimal places)
        assert_eq!(format_number(1000), "1.00K");
        assert_eq!(format_number(1500), "1.50K");
        assert_eq!(format_number(6800), "6.80K");
        assert_eq!(format_number(9500), "9.50K");

        // 10K - 100K (1 decimal place)
        assert_eq!(format_number(10000), "10.0K");
        assert_eq!(format_number(10900), "10.9K");
        assert_eq!(format_number(99500), "99.5K");

        // 100K - 1M (no decimal)
        assert_eq!(format_number(100000), "100K");
        assert_eq!(format_number(500000), "500K");
        assert_eq!(format_number(961400), "961K");
        assert_eq!(format_number(999999), "1000K");

        // 1M - 10M (2 decimal places)
        assert_eq!(format_number(1000000), "1.00M");
        assert_eq!(format_number(1500000), "1.50M");
        assert_eq!(format_number(6800000), "6.80M");
        assert_eq!(format_number(9500000), "9.50M");

        // 10M - 100M (1 decimal place)
        assert_eq!(format_number(10000000), "10.0M");
        assert_eq!(format_number(10900000), "10.9M");

        // 100M - 1B (no decimal)
        assert_eq!(format_number(100000000), "100M");
        assert_eq!(format_number(500000000), "500M");

        // 1B - 10B (2 decimal places)
        assert_eq!(format_number(1000000000), "1.00B");
        assert_eq!(format_number(1500000000), "1.50B");
        assert_eq!(format_number(9500000000), "9.50B");

        // 10B - 100B (1 decimal place)
        assert_eq!(format_number(10000000000), "10.0B");

        // 100B+ (no decimal)
        assert_eq!(format_number(100000000000), "100B");
    }

    #[test]
    fn test_load_data_performance() {
        let result = load_data(20).unwrap();
        if result.file_count == 0 {
            return; // No data available (CI environment)
        }
        assert!(result.file_count <= 20);
    }

    #[test]
    #[ignore] // Deletes cache file — run manually with `cargo test -- --ignored`
    fn test_cache_speedup() {
        std::fs::remove_file(
            std::path::PathBuf::from(
                std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| "/tmp".to_string()),
            )
            .join(".cache/ccsight/cache.json"),
        )
        .ok();

        let start1 = Instant::now();
        let result1 = load_data(20);
        let duration1 = start1.elapsed();
        let cache1 = result1.unwrap().cache_stats;

        let start2 = Instant::now();
        let result2 = load_data(20);
        let duration2 = start2.elapsed();
        let cache2 = result2.unwrap().cache_stats;

        assert!(cache2.cached_files > 0, "Second load should use cache");
        assert!(
            duration2 < duration1 || duration2.as_millis() < 500,
            "Cached load ({:?}) should be faster than uncached ({:?})",
            duration2,
            duration1
        );
    }

    #[test]
    fn test_load_data_integration() {
        let result = load_data(5).unwrap();
        if result.file_count == 0 {
            return; // No data available (CI environment)
        }

        assert!(!result.stats.daily_activity.is_empty());
        assert!(!result.stats.project_stats.is_empty());
        assert!(
            result.models_without_pricing.is_empty(),
            "Models without pricing: {:?}",
            result.models_without_pricing
        );

        let groups = &result.daily_groups;
        assert!(!groups.is_empty());
        for i in 1..groups.len() {
            assert!(groups[i - 1].date >= groups[i].date);
        }
    }

    #[test]
    #[ignore]
    fn bench_search_index() {
        let _ = infrastructure::SearchIndex::clear_index();
        let result = load_data(0).unwrap();
        if result.daily_groups.is_empty() {
            return;
        }
        let groups = &result.daily_groups;
        let session_count: usize = groups
            .iter()
            .map(|g| g.sessions.iter().filter(|s| !s.is_subagent).count())
            .sum();
        println!("Sessions: {session_count}");

        let start = Instant::now();
        let index = infrastructure::SearchIndex::update_or_build(groups).unwrap();
        let build_time = start.elapsed();
        println!("Index build: {build_time:?}");

        let queries = ["cargo", "hello", "commit", "error", "ratatui", "日本語テスト"];
        println!("\n--- Search breakdown ---");
        for q in &queries {
            let start = Instant::now();
            let results = index.search(q, 200, 50);
            let total = start.elapsed();
            println!("'{q}': {total:?} total, {} results", results.len());
        }

        println!("\n--- Warm search (2nd call) ---");
        for q in &queries {
            let start = Instant::now();
            let results = index.search(q, 200, 50);
            let total = start.elapsed();
            println!("'{q}': {total:?} total, {} results", results.len());
        }

        println!("\n--- Fallback regex path ---");
        let special_queries = ["cargo build", "hello world", "fn main()", "state.search_"];
        for q in &special_queries {
            let start = Instant::now();
            let results = index.search(q, 200, 50);
            let total = start.elapsed();
            println!("'{q}': {total:?} total, {} results", results.len());
        }

        println!("\n--- Linear scan comparison ---");
        let start = Instant::now();
        let mut linear_count = 0;
        let mut searched = 0;
        for group in groups {
            for session in group.sessions.iter().filter(|s| !s.is_subagent) {
                if searched >= 100 { break; }
                if search::search_session_content(&session.file_path, "cargo").is_some() {
                    linear_count += 1;
                }
                searched += 1;
            }
        }
        let linear_time = start.elapsed();
        println!("Linear 'cargo' ({searched} files): {linear_time:?} ({linear_count} matches)");

        println!("\n--- 2nd update_or_build (no changes) ---");
        let start = Instant::now();
        let _index2 = infrastructure::SearchIndex::update_or_build(groups).unwrap();
        println!("Open existing: {:?}", start.elapsed());
    }

    #[test]
    fn test_parse_text_with_code_blocks_simple() {
        let text = "Hello\n```rust\nfn main() {}\n```\nWorld";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(segments.len(), 3);

        match &segments[0] {
            TextSegment::Plain(s) => assert_eq!(s, "Hello"),
            _ => panic!("Expected Plain segment"),
        }

        match &segments[1] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(content, "fn main() {}");
            }
            _ => panic!("Expected Code segment"),
        }

        match &segments[2] {
            TextSegment::Plain(s) => assert_eq!(s, "World"),
            _ => panic!("Expected Plain segment"),
        }
    }

    #[test]
    fn test_parse_text_with_code_blocks_multiple() {
        let text = "```rust\nlet x = 1;\n```\nText\n```python\nprint('hi')\n```";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(
            segments.len(),
            3,
            "segments: {:?}",
            segments
                .iter()
                .map(|s| match s {
                    TextSegment::Plain(p) => format!("Plain({:?})", p),
                    TextSegment::Code { lang, content } =>
                        format!("Code({:?}, {:?})", lang, content),
                })
                .collect::<Vec<_>>()
        );

        match &segments[0] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(content, "let x = 1;");
            }
            _ => panic!("Expected Code segment"),
        }

        match &segments[1] {
            TextSegment::Plain(s) => assert_eq!(s, "Text"),
            _ => panic!("Expected Plain segment"),
        }

        match &segments[2] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("python"));
                assert_eq!(content, "print('hi')");
            }
            _ => panic!("Expected Code segment"),
        }
    }

    #[test]
    fn test_parse_text_with_code_blocks_no_lang() {
        let text = "```\nsome code\n```";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(segments.len(), 1);

        match &segments[0] {
            TextSegment::Code { lang, content } => {
                assert!(lang.is_none());
                assert_eq!(content, "some code");
            }
            _ => panic!("Expected Code segment"),
        }
    }

    #[test]
    fn test_parse_text_with_code_blocks_real_jsonl() {
        let text = "コードブロックの例:\n```rust\nfn create_dedup_hash(entry: &LogEntry) -> Option<String> {\n    let request_id = entry.request_id.as_ref()?;\n    Some(format!(\"{}:{}\", message_id, request_id))\n}\n```\n改善点を説明します。";
        let segments = parse_text_with_code_blocks(text);

        assert_eq!(segments.len(), 3);

        match &segments[0] {
            TextSegment::Plain(s) => assert!(s.contains("コードブロックの例")),
            _ => panic!("Expected Plain segment"),
        }

        match &segments[1] {
            TextSegment::Code { lang, content } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(content.contains("create_dedup_hash"));
                assert!(content.contains("Option<String>"));
            }
            _ => panic!("Expected Code segment"),
        }

        match &segments[2] {
            TextSegment::Plain(s) => assert!(s.contains("改善点")),
            _ => panic!("Expected Plain segment"),
        }
    }

    #[test]
    fn test_render_text_with_highlighting() {
        let text = "Hello\n```rust\nfn main() {\n    println!(\"test\");\n}\n```\nWorld";
        let (lines, flags) = render_text_with_highlighting(text, 80);

        println!("Rendered {} lines:", lines.len());
        for (i, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            println!("Line {}: {}", i, text);
        }

        assert_eq!(lines.len(), flags.len());
        assert!(lines.len() >= 7, "Should have at least 7 lines (plain + code header + 3 code lines + code footer + plain)");

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all_text.contains("Hello"));
        assert!(all_text.contains("rust"));
        assert!(all_text.contains("fn main()"));
        assert!(all_text.contains("World"));
    }

    #[test]
    fn test_highlighting_with_actual_jsonl_text() {
        let text = "全コードを確認しました。レビュー結果をまとめます。\n\n---\n\n## コードレビュー\n\n### 1. 重複エントリ除外\n\n**良い点:**\n- `HashSet`を使った効率的な重複検出\n\n**改善点:**\n```rust\nfn create_dedup_hash(entry: &LogEntry) -> Option<String> {\n    let request_id = entry.request_id.as_ref()?;\n    let message_id = entry.message.as_ref()?.id.as_ref()?;\n    Some(format!(\"{}:{}\", message_id, request_id))\n}\n```\n- 毎回`format!`で文字列を生成";

        let segments = parse_text_with_code_blocks(text);
        println!("Segments: {}", segments.len());

        let mut found_code = false;
        for (i, seg) in segments.iter().enumerate() {
            match seg {
                TextSegment::Plain(p) => println!("Segment {}: Plain({} chars)", i, p.len()),
                TextSegment::Code { lang, content } => {
                    println!(
                        "Segment {}: Code(lang={:?}, {} chars)",
                        i,
                        lang,
                        content.len()
                    );
                    found_code = true;
                    assert_eq!(lang.as_deref(), Some("rust"));
                    assert!(content.contains("create_dedup_hash"));
                }
            }
        }

        assert!(found_code, "Should find at least one code block");

        let (lines, _flags) = render_text_with_highlighting(text, 80);
        println!("\nRendered {} lines", lines.len());

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();

        assert!(
            all_text.contains("コードレビュー"),
            "Should contain Japanese text"
        );
        assert!(all_text.contains("rust"), "Should contain language label");
        assert!(
            all_text.contains("create_dedup_hash"),
            "Should contain function name"
        );
    }

    #[test]
    fn test_render_tool_result_with_highlighting() {
        let content = "The file /home/user/project/src/main.rs has been updated. Here's the result of running `cat -n` on a snippet of the edited file:\n     1→use std::io;\n     2→\n     3→fn main() {\n     4→    println!(\"Hello\");\n     5→}\n";

        let (lines, _flags) = render_tool_result_with_highlighting(content, 80);

        println!("Rendered {} lines:", lines.len());
        for (i, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            println!("Line {}: {}", i, text);
        }

        assert!(lines.len() >= 5, "Should have at least 5 lines");

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();

        assert!(all_text.contains("use std::io"), "Should contain code");
        assert!(all_text.contains("fn main()"), "Should contain function");
        assert!(
            all_text.contains("1→") || all_text.contains("→"),
            "Should contain line numbers"
        );
    }

    #[test]
    fn test_parse_custom_single_date() {
        let f = PeriodFilter::parse_custom("2026-02-15").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 15).unwrap());
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 2, 15).unwrap());
            }
            _ => panic!("expected Custom with same start/end"),
        }
    }

    #[test]
    fn test_parse_custom_year_month() {
        let f = PeriodFilter::parse_custom("2026-02").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
            }
            _ => panic!("expected Custom with month range"),
        }
    }

    #[test]
    fn test_parse_custom_year_month_december() {
        let f = PeriodFilter::parse_custom("2025-12").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2025, 12, 1).unwrap());
                assert_eq!(end, NaiveDate::from_ymd_opt(2025, 12, 31).unwrap());
            }
            _ => panic!("expected Custom with december range"),
        }
    }

    #[test]
    fn test_parse_custom_leap_year_february() {
        let f = PeriodFilter::parse_custom("2024-02").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2024, 2, 1).unwrap());
                assert_eq!(end, NaiveDate::from_ymd_opt(2024, 2, 29).unwrap());
            }
            _ => panic!("expected leap year feb range"),
        }
    }

    #[test]
    fn test_parse_custom_range() {
        let f = PeriodFilter::parse_custom("2026-01-01..2026-01-31").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 1, 31).unwrap());
            }
            _ => panic!("expected Custom range"),
        }
    }

    #[test]
    fn test_parse_custom_range_with_spaces() {
        let f = PeriodFilter::parse_custom("  2026-01-01 .. 2026-01-31  ").unwrap();
        match f {
            PeriodFilter::Custom(start, Some(end)) => {
                assert_eq!(start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
                assert_eq!(end, NaiveDate::from_ymd_opt(2026, 1, 31).unwrap());
            }
            _ => panic!("expected Custom range with trimmed spaces"),
        }
    }

    #[test]
    fn test_parse_custom_invalid_garbage() {
        assert!(PeriodFilter::parse_custom("abc").is_none());
    }

    #[test]
    fn test_parse_custom_invalid_month() {
        assert!(PeriodFilter::parse_custom("2026-13").is_none());
    }

    #[test]
    fn test_parse_custom_empty_string() {
        assert!(PeriodFilter::parse_custom("").is_none());
    }

    #[test]
    fn test_parse_custom_invalid_date() {
        assert!(PeriodFilter::parse_custom("2026-02-30").is_none());
    }

    #[test]
    fn test_date_range_label_all() {
        assert_eq!(PeriodFilter::All.date_range_label(), "");
    }

    #[test]
    fn test_date_range_label_today() {
        let label = PeriodFilter::Today.date_range_label();
        assert!(label.starts_with('('));
        assert!(label.ends_with(')'));
    }

    #[test]
    fn test_date_range_label_custom_range() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        let label = PeriodFilter::Custom(start, Some(end)).date_range_label();
        assert!(label.contains("01-01"));
        assert!(label.contains("01-31"));
    }

    #[test]
    fn test_period_filter_date_range_all() {
        let (start, end) = PeriodFilter::All.date_range();
        assert!(start.is_none());
        assert!(end.is_none());
    }

    #[test]
    fn test_period_filter_date_range_last_month() {
        let (start, end) = PeriodFilter::LastMonth.date_range();
        assert!(start.is_some());
        assert!(end.is_some());
        let s = start.unwrap();
        let e = end.unwrap();
        assert_eq!(s.day(), 1);
        assert!(e >= s);
    }

    #[test]
    fn test_apply_filter_no_filter_restores_original() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let groups = vec![
            make_daily_group(today, vec![make_session("~/projects/a", None, None)]),
            make_daily_group(
                today - chrono::Duration::days(5),
                vec![make_session("~/projects/b", None, None)],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.period_filter = PeriodFilter::All;
        state.project_filter = None;
        state.apply_filter();
        assert_eq!(state.daily_groups.len(), 2);
    }

    #[test]
    fn test_apply_filter_project_only() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let groups = vec![make_daily_group(
            today,
            vec![
                make_session_with_tokens("~/projects/alpha", 1000, 500, "claude-sonnet-4-20250514"),
                make_session_with_tokens("~/projects/beta", 2000, 800, "claude-sonnet-4-20250514"),
            ],
        )];
        let mut state = make_test_app_state(groups);
        state.project_filter = Some("~/projects/alpha".to_string());
        state.apply_filter();

        assert_eq!(state.daily_groups.len(), 1);
        assert_eq!(state.daily_groups[0].sessions.len(), 1);
        assert_eq!(
            state.daily_groups[0].sessions[0].project_name,
            "~/projects/alpha"
        );
    }

    #[test]
    fn test_apply_filter_project_removes_empty_groups() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(today, vec![make_session("~/projects/alpha", None, None)]),
            make_daily_group(yesterday, vec![make_session("~/projects/beta", None, None)]),
        ];
        let mut state = make_test_app_state(groups);
        state.project_filter = Some("~/projects/alpha".to_string());
        state.apply_filter();

        assert_eq!(state.daily_groups.len(), 1);
        assert_eq!(state.daily_groups[0].date, today);
    }

    #[test]
    fn test_apply_filter_period_and_project_combined() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let old = today - chrono::Duration::days(60);
        let groups = vec![
            make_daily_group(
                today,
                vec![
                    make_session("~/projects/alpha", None, None),
                    make_session("~/projects/beta", None, None),
                ],
            ),
            make_daily_group(old, vec![make_session("~/projects/alpha", None, None)]),
        ];
        let mut state = make_test_app_state(groups);
        state.period_filter = PeriodFilter::Last30d;
        state.project_filter = Some("~/projects/alpha".to_string());
        state.apply_filter();

        assert_eq!(state.daily_groups.len(), 1);
        assert_eq!(state.daily_groups[0].sessions.len(), 1);
        assert_eq!(
            state.daily_groups[0].sessions[0].project_name,
            "~/projects/alpha"
        );
    }

    #[test]
    fn test_apply_filter_resets_selected_day_when_out_of_bounds() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let groups = vec![
            make_daily_group(today, vec![make_session("~/projects/a", None, None)]),
            make_daily_group(
                today - chrono::Duration::days(1),
                vec![make_session("~/projects/b", None, None)],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.selected_day = 5;
        state.project_filter = Some("~/projects/a".to_string());
        state.apply_filter();

        assert!(state.selected_day < state.daily_groups.len());
    }

    #[test]
    fn test_rebuild_project_list() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);
        let groups = vec![
            make_daily_group(
                today,
                vec![
                    make_session_with_tokens("~/projects/big", 5000, 3000, "sonnet"),
                    make_session_with_tokens("~/projects/small", 100, 50, "sonnet"),
                ],
            ),
            make_daily_group(
                yesterday,
                vec![make_session_with_tokens("~/projects/big", 2000, 1000, "sonnet")],
            ),
        ];
        let mut state = make_test_app_state(groups);
        state.rebuild_project_list();

        assert_eq!(state.project_list.len(), 2);
        assert_eq!(state.project_list[0].0, "~/projects/big");
        assert!(state.project_list[0].1 > state.project_list[1].1);
        assert_eq!(state.project_list[0].2, today);
    }

    #[test]
    fn test_rebuild_project_list_skips_subagents() {
        use crate::test_helpers::helpers::*;
        let today = chrono::Local::now().date_naive();
        let mut subagent = make_session("~/projects/agent-task", None, None);
        subagent.is_subagent = true;
        let groups = vec![make_daily_group(
            today,
            vec![make_session("~/projects/main", None, None), subagent],
        )];
        let mut state = make_test_app_state(groups);
        state.rebuild_project_list();

        assert_eq!(state.project_list.len(), 1);
        assert_eq!(state.project_list[0].0, "~/projects/main");
    }

    fn make_buffer(width: u16, height: u16, lines: &[&str]) -> ratatui::buffer::Buffer {
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for (row, line) in lines.iter().enumerate() {
            buf.set_string(0, row as u16, line, ratatui::style::Style::default());
        }
        buf
    }

    #[test]
    fn test_extract_ascii_single_line() {
        let buf = make_buffer(20, 3, &["Hello, World!       ", "Second line         "]);
        let sel = (0, 0, 12, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello, World!");
    }

    #[test]
    fn test_extract_ascii_multi_line() {
        let buf = make_buffer(20, 3, &["Hello               ", "World               "]);
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn test_extract_cjk_no_extra_spaces() {
        let buf = make_buffer(20, 2, &["自動スクロール      "]);
        let sel = (0, 0, 19, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "自動スクロール");
    }

    #[test]
    fn test_extract_cjk_mixed_with_ascii() {
        let buf = make_buffer(30, 2, &["Hello自動World      "]);
        let sel = (0, 0, 29, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello自動World");
    }

    #[test]
    fn test_extract_cjk_partial_selection() {
        let buf = make_buffer(20, 2, &["あいうえお          "]);
        let sel = (2, 0, 7, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "いうえ");
    }

    #[test]
    fn test_extract_with_clamp_area() {
        let buf = make_buffer(
            40,
            5,
            &[
                "│ sidebar │content line 1               ",
                "│ sidebar │content line 2               ",
                "│ sidebar │content line 3               ",
            ],
        );
        let conv_area = ratatui::layout::Rect::new(11, 0, 29, 3);
        let sel = (11, 0, 39, 2);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), None, 0);
        assert_eq!(text, "content line 1\ncontent line 2\ncontent line 3");
    }

    #[test]
    fn test_extract_clamp_excludes_outside_rows() {
        let buf = make_buffer(
            30,
            5,
            &[
                "header                        ",
                "content A                     ",
                "content B                     ",
                "footer                        ",
            ],
        );
        let conv_area = ratatui::layout::Rect::new(0, 1, 30, 2);
        let sel = (0, 1, 29, 2);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), None, 0);
        assert_eq!(text, "content A\ncontent B");
    }

    #[test]
    fn test_extract_clamp_not_applied_when_start_outside() {
        let buf = make_buffer(
            40,
            3,
            &[
                "│ sidebar │ content                     ",
                "│ sidebar │ more                        ",
            ],
        );
        let conv_area = ratatui::layout::Rect::new(11, 0, 29, 2);
        let sel = (0, 0, 39, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), None, 0);
        assert!(text.contains("sidebar"));
    }

    #[test]
    fn test_extract_reversed_selection() {
        let buf = make_buffer(20, 2, &["Hello World         "]);
        let sel = (10, 0, 0, 0);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn test_extract_trailing_empty_lines_removed() {
        let buf = make_buffer(20, 5, &["Hello", "World", "   ", "   "]);
        let sel = (0, 0, 19, 3);
        let text = extract_selected_text_from_buffer(&sel, &buf, None, None, 0);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn test_join_conversation_lines_word_wrap() {
        let lines = vec![
            "  This is a very long line that was word".to_string(),
            "  wrapped by the renderer".to_string(),
        ];
        let flags = vec![false, true];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "This is a very long line that was word wrapped by the renderer");
    }

    #[test]
    fn test_join_conversation_lines_preserves_paragraph_break() {
        let lines = vec![
            "  Short line".to_string(),
            "  Another line".to_string(),
        ];
        let flags = vec![false, false];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "Short line\nAnother line");
    }

    #[test]
    fn test_join_conversation_lines_strips_arrow_prefix() {
        let lines = vec![
            "▶ First message".to_string(),
            "  continuation".to_string(),
        ];
        let flags = vec![false, false];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "First message\ncontinuation");
    }

    #[test]
    fn test_join_conversation_lines_cjk_word_wrap() {
        let lines = vec![
            "  あいうえおかきくけこさしすせそたちつてと".to_string(),
            "  なにぬねの".to_string(),
        ];
        let flags = vec![false, true];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(
            text,
            "あいうえおかきくけこさしすせそたちつてと なにぬねの"
        );
    }

    #[test]
    fn test_join_conversation_lines_empty() {
        let lines: Vec<String> = vec![];
        let flags: Vec<bool> = vec![];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "");
    }

    #[test]
    fn test_join_conversation_lines_empty_lines_between() {
        let lines = vec![
            "  Paragraph one end that fills the width".to_string(),
            "".to_string(),
            "  Paragraph two".to_string(),
        ];
        let flags = vec![false, false, false];
        let text = join_conversation_lines(&lines, &flags);
        assert_eq!(text, "Paragraph one end that fills the width\n\nParagraph two");
    }

    #[test]
    fn test_extract_with_wrap_flags_removes_newlines() {
        let buf = make_buffer(20, 2, &[
            "  hello world this",
            "  is a test",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 20, 2);
        let flags = vec![false, true];
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 0);
        assert_eq!(text, "hello world this is a test");
    }

    #[test]
    fn test_extract_with_wrap_flags_preserves_real_newlines() {
        let buf = make_buffer(20, 2, &[
            "  line one",
            "  line two",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 20, 2);
        let flags = vec![false, false];
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 0);
        assert_eq!(text, "line one\nline two");
    }

    #[test]
    fn test_extract_with_wrap_flags_scroll_offset() {
        let buf = make_buffer(20, 2, &[
            "  wrapped line",
            "  continuation",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 20, 2);
        let flags: Vec<bool> = vec![false; 5].into_iter()
            .chain(vec![false, true])
            .collect();
        let sel = (0, 0, 19, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 5);
        assert_eq!(text, "wrapped line continuation");
    }

    #[test]
    fn test_extract_with_wrap_flags_cjk() {
        let buf = make_buffer(24, 2, &[
            "  あいうえおかきくけこ",
            "  さしすせそ",
        ]);
        let conv_area = ratatui::layout::Rect::new(0, 0, 24, 2);
        let flags = vec![false, true];
        let sel = (0, 0, 23, 1);
        let text = extract_selected_text_from_buffer(&sel, &buf, Some(conv_area), Some(&flags), 0);
        assert_eq!(text, "あいうえおかきくけこ さしすせそ");
    }
}
