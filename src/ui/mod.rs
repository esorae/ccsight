mod dashboard;
mod insights;

use std::sync::OnceLock;

use chrono::Local;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

use crate::aggregator::{CostCalculator, SessionInfo};
use crate::search;
use crate::{AppState, ConversationBlock, ConversationMessage, ConversationPane, SummaryType, Tab};

pub mod theme {
    use ratatui::style::Color;

    // Base: Terracotta orange
    pub const PRIMARY: Color = Color::Rgb(218, 119, 86);
    // Muted sand
    pub const SECONDARY: Color = Color::Rgb(168, 154, 140);
    // Sage green (warm)
    pub const SUCCESS: Color = Color::Rgb(130, 166, 110);
    // Amber gold
    pub const WARNING: Color = Color::Rgb(210, 160, 70);
    // Terracotta red
    pub const ERROR: Color = Color::Rgb(195, 90, 80);
    pub const DANGER: Color = Color::Rgb(180, 70, 70);
    // Teal accent (complement)
    pub const ACCENT: Color = Color::Rgb(86, 165, 180);

    // Neutral tones
    pub const WARM: Color = Color::Rgb(175, 145, 125);
    pub const MUTED: Color = Color::Rgb(130, 120, 110);
    pub const DIM: Color = Color::Rgb(140, 130, 120);
    pub const FAINT: Color = Color::Rgb(55, 50, 48);
    pub const BORDER: Color = Color::Rgb(90, 85, 80);
    pub const SEPARATOR: Color = Color::Rgb(60, 55, 50);

    // Text colors
    pub const TEXT_BRIGHT: Color = Color::Rgb(240, 235, 230);
    pub const TEXT_DARK: Color = Color::Rgb(30, 28, 26);
    pub const LABEL_MUTED: Color = Color::Rgb(150, 145, 140);
    pub const LABEL_SUBTLE: Color = Color::Rgb(125, 120, 115);

    // Model colors
    pub const MODEL_OPUS: Color = Color::Rgb(170, 120, 200);
    pub const MODEL_SONNET: Color = Color::Rgb(100, 160, 210);
    pub const MODEL_HAIKU: Color = Color::Rgb(130, 190, 160);

    // Special elements
    pub const BRANCH: Color = Color::Rgb(120, 140, 170);
    pub const LINK: Color = Color::Rgb(100, 140, 180);
    pub const THINKING: Color = Color::Rgb(140, 125, 165);
    pub const SEARCH_MATCH: Color = Color::Rgb(60, 55, 35);
    pub const SEARCH_CURRENT: Color = Color::Rgb(130, 110, 70);
    pub const SELECTION: Color = Color::Rgb(60, 60, 120);

    // Heatmap (terracotta gradient)
    pub const HEATMAP_EMPTY: Color = Color::Rgb(35, 32, 30);
    pub const HEATMAP_LOW: Color = Color::Rgb(80, 55, 45);
    pub const HEATMAP_MID: Color = Color::Rgb(140, 85, 65);
    pub const HEATMAP_HIGH: Color = Color::Rgb(200, 110, 80);

    // PRIMARY color base values for dynamic intensity
    pub const PRIMARY_R: f64 = 218.0;
    pub const PRIMARY_G: f64 = 119.0;
    pub const PRIMARY_B: f64 = 86.0;

    pub fn primary_with_intensity(intensity: f64) -> Color {
        Color::Rgb(
            (PRIMARY_R * intensity) as u8,
            (PRIMARY_G * intensity) as u8,
            (PRIMARY_B * intensity) as u8,
        )
    }
}

#[derive(Clone)]
pub enum BreakdownItem {
    Project(String, u64, f64),
    Model(String, u64, f64),
    Tool(String, usize, f64),
}

pub fn truncate_to_display_width(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut width = 0;
    let mut result = String::new();
    for ch in s.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result
}

pub(crate) fn shorten_project(name: &str) -> &str {
    std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name)
}

pub(crate) fn weekday_occurrence_count(
    calendar_days: usize,
    first_date: chrono::NaiveDate,
    weekday: chrono::Weekday,
) -> u32 {
    use chrono::Datelike;
    let full_weeks = calendar_days as u32 / 7;
    let remainder = calendar_days as u32 % 7;
    let extra = if remainder > 0 {
        let first_wd = first_date.weekday().num_days_from_monday();
        let target_wd = weekday.num_days_from_monday();
        let offset = (target_wd + 7 - first_wd) % 7;
        if offset < remainder { 1 } else { 0 }
    } else {
        0
    };
    (full_weeks + extra).max(1)
}

pub fn cost_style(cost: f64) -> Style {
    let c = cost.max(0.0);
    let color = if c > 100.0 {
        theme::DANGER
    } else if c > 50.0 {
        theme::ERROR
    } else if c > 20.0 {
        theme::WARNING
    } else {
        theme::SUCCESS
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub(crate) fn format_cost(cost: f64, precision: usize) -> String {
    let c = cost.max(0.0);
    match precision {
        0 => format!("${c:.0}"),
        _ => format!("${c:.2}"),
    }
}

pub(crate) fn calc_scroll(
    area_height: u16,
    item_count: usize,
    scroll: usize,
    header: u16,
) -> (usize, usize, usize) {
    let visible = area_height.saturating_sub(header) as usize;
    let max_scroll = item_count.saturating_sub(visible);
    (visible, max_scroll, scroll.min(max_scroll))
}


pub fn model_color(model: &str) -> Color {
    match model {
        m if m.contains("opus") => theme::MODEL_OPUS,
        m if m.contains("sonnet") => theme::MODEL_SONNET,
        m if m.contains("haiku") => theme::MODEL_HAIKU,
        _ => theme::LABEL_MUTED,
    }
}

fn draw_scrollbar(frame: &mut Frame, area: Rect, scroll: usize, total: usize, visible: usize) {
    if total <= visible || area.height < 3 {
        return;
    }

    let track_height = area.height.saturating_sub(2) as usize;
    if track_height == 0 {
        return;
    }

    let thumb_size = ((visible as f64 / total as f64) * track_height as f64)
        .ceil()
        .max(1.0) as usize;
    let thumb_size = thumb_size.min(track_height);

    let max_scroll = total.saturating_sub(visible);
    let thumb_pos = if max_scroll > 0 {
        ((scroll as f64 / max_scroll as f64) * (track_height - thumb_size) as f64).round() as usize
    } else {
        0
    };

    let scrollbar_x = area.x + area.width.saturating_sub(1);
    for i in 0..track_height {
        let y = area.y + 1 + i as u16;
        let ch = if i >= thumb_pos && i < thumb_pos + thumb_size {
            "█"
        } else {
            "░"
        };
        let span = Span::styled(ch, Style::default().fg(theme::DIM));
        frame.render_widget(Paragraph::new(span), Rect::new(scrollbar_x, y, 1, 1));
    }
}

fn get_cat_n_pattern() -> &'static regex::Regex {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    PATTERN.get_or_init(|| regex::Regex::new(r"^\s*\d+[→\t]").unwrap())
}

fn get_syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn get_theme_set() -> &'static ThemeSet {
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

pub fn warmup_syntax_highlighting() {
    let _ = get_syntax_set();
    let _ = get_theme_set();
}

pub use crate::text::{parse_text_with_code_blocks, TextSegment};

fn syntect_to_ratatui_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

fn truncate_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthChar;
    let mut remaining = max_width;
    let mut result = Vec::new();
    for span in spans {
        if remaining == 0 {
            break;
        }
        let mut width = 0;
        let mut truncated = String::new();
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if width + ch_width > remaining {
                break;
            }
            truncated.push(ch);
            width += ch_width;
        }
        remaining -= width;
        if !truncated.is_empty() {
            result.push(Span::styled(truncated, span.style));
        }
    }
    result
}

fn highlight_xml_tags(line: &str) -> Line<'static> {
    if !line.contains('<') {
        return Line::from(line.to_string());
    }
    let mut spans = Vec::new();
    let mut last = 0;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(end) = line[i..].find('>') {
                let tag = &line[i..i + end + 1];
                if tag.len() >= 3
                    && (tag.as_bytes()[1].is_ascii_alphabetic()
                        || tag.as_bytes()[1] == b'/')
                {
                    if last < i {
                        spans.push(Span::raw(line[last..i].to_string()));
                    }
                    spans.push(Span::styled(
                        tag.to_string(),
                        Style::default().fg(theme::DIM),
                    ));
                    last = i + end + 1;
                    i = last;
                    continue;
                }
            }
        i += 1;
    }
    if last < line.len() {
        spans.push(Span::raw(line[last..].to_string()));
    }
    if spans.is_empty() {
        Line::from(line.to_string())
    } else {
        Line::from(spans)
    }
}

fn highlight_code_line(
    line: &str,
    highlighter: &mut HighlightLines,
    syntax_set: &SyntaxSet,
) -> Vec<Span<'static>> {
    let cat_n_pattern = get_cat_n_pattern();

    let (prefix, code_part) = if let Some(mat) = cat_n_pattern.find(line) {
        let prefix = &line[..mat.end()];
        let code = &line[mat.end()..];
        (
            Some(Span::styled(
                prefix.to_string(),
                Style::default().fg(theme::DIM),
            )),
            code,
        )
    } else {
        (None, line)
    };

    let mut spans = Vec::new();
    if let Some(p) = prefix {
        spans.push(p);
    }

    if let Ok(highlighted) = highlighter.highlight_line(code_part, syntax_set) {
        for (style, text) in highlighted {
            spans.push(Span::styled(
                text.to_string(),
                Style::default().fg(syntect_to_ratatui_color(style.foreground)),
            ));
        }
    } else {
        spans.push(Span::raw(code_part.to_string()));
    }

    spans
}

pub fn render_tool_result_with_highlighting(content: &str, max_width: usize) -> (Vec<Line<'static>>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;

    let syntax_set = get_syntax_set();
    let theme_set = get_theme_set();
    let theme = &theme_set.themes["base16-ocean.dark"];

    let mut lines = Vec::new();
    let mut wrap_flags = Vec::new();
    let cat_n_pattern = get_cat_n_pattern();

    let has_line_numbers = content.lines().take(3).any(|l| cat_n_pattern.is_match(l));

    if has_line_numbers {
        let extension = content
            .lines()
            .find_map(|l| {
                if let Some(mat) = cat_n_pattern.find(l) {
                    let after = &l[mat.end()..];
                    if after.contains("fn ") || after.contains("let ") || after.contains("impl ") {
                        return Some("rs");
                    }
                    if after.contains("function") || after.contains("const ") {
                        return Some("js");
                    }
                    if after.contains("def ") || after.contains("import ") {
                        return Some("py");
                    }
                }
                None
            })
            .unwrap_or("txt");

        let syntax = syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
        let mut highlighter = HighlightLines::new(syntax, theme);

        for line in content.lines().take(50) {
            let spans = highlight_code_line(line, &mut highlighter, syntax_set);
            lines.push(Line::from(spans));
            wrap_flags.push(false);
        }
        if content.lines().count() > 50 {
            lines.push(Line::from(Span::styled(
                format!("... ({} more lines)", content.lines().count() - 50),
                Style::default().fg(theme::DIM),
            )));
            wrap_flags.push(false);
        }
    } else {
        let (wrapped, flags) = wrap_text_with_continuation(content, max_width);
        for line in wrapped.into_iter().take(30) {
            lines.push(Line::from(Span::styled(
                line,
                Style::default().fg(theme::SECONDARY),
            )));
        }
        wrap_flags.extend(flags.into_iter().take(30));
    }

    (lines, wrap_flags)
}

pub fn render_text_with_highlighting(text: &str, max_width: usize) -> (Vec<Line<'static>>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;

    let syntax_set = get_syntax_set();
    let theme_set = get_theme_set();
    let theme = &theme_set.themes["base16-ocean.dark"];

    let segments = parse_text_with_code_blocks(text);
    let mut lines = Vec::new();
    let mut wrap_flags = Vec::new();

    for segment in segments {
        match segment {
            TextSegment::Plain(plain) => {
                let (wrapped, flags) = wrap_text_with_continuation(&plain, max_width);
                for line in wrapped {
                    lines.push(highlight_xml_tags(&line));
                }
                wrap_flags.extend(flags);
            }
            TextSegment::Code { lang, content } => {
                lines.push(Line::from(Span::styled(
                    format!("```{}", lang.as_deref().unwrap_or("")),
                    Style::default().fg(theme::DIM),
                )));
                wrap_flags.push(false);

                let ext = lang.as_deref().unwrap_or("txt");
                let syntax = syntax_set
                    .find_syntax_by_extension(ext)
                    .or_else(|| syntax_set.find_syntax_by_name(ext))
                    .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
                let mut highlighter = HighlightLines::new(syntax, theme);

                for code_line in content.lines().take(30) {
                    let spans = highlight_code_line(code_line, &mut highlighter, syntax_set);
                    lines.push(Line::from(spans));
                    wrap_flags.push(false);
                }
                if content.lines().count() > 30 {
                    lines.push(Line::from(Span::styled(
                        format!("... ({} more lines)", content.lines().count() - 30),
                        Style::default().fg(theme::DIM),
                    )));
                    wrap_flags.push(false);
                }

                lines.push(Line::from(Span::styled(
                    "```",
                    Style::default().fg(theme::DIM),
                )));
                wrap_flags.push(false);
            }
        }
    }

    (lines, wrap_flags)
}

pub fn is_tool_only_message(msg: &ConversationMessage) -> bool {
    !msg.blocks.is_empty()
        && msg.blocks.iter().all(|b| {
            matches!(
                b,
                ConversationBlock::ToolUse { .. } | ConversationBlock::ToolResult { .. }
            )
        })
}

pub fn is_thinking_only_message(msg: &ConversationMessage) -> bool {
    !msg.blocks.is_empty()
        && msg
            .blocks
            .iter()
            .all(|b| matches!(b, ConversationBlock::Thinking(_)))
}

pub fn extract_message_text(msg: &ConversationMessage) -> String {
    let mut parts: Vec<String> = Vec::new();

    for block in &msg.blocks {
        match block {
            ConversationBlock::Text(text) => {
                parts.push(text.clone());
            }
            ConversationBlock::ToolUse {
                name,
                input_summary,
            } => {
                parts.push(format!("[Tool: {name}] {input_summary}"));
            }
            ConversationBlock::ToolResult { content, is_error } => {
                let prefix = if *is_error { "[Error] " } else { "" };
                parts.push(format!("{prefix}{content}"));
            }
            ConversationBlock::Thinking(text) => {
                parts.push(format!("[Thinking] {text}"));
            }
        }
    }

    parts.join("\n\n")
}

fn compute_search_matches(
    rendered: &Option<(
        Vec<ratatui::text::Line<'static>>,
        Vec<(usize, usize)>,
        Vec<bool>,
        Option<usize>,
    )>,
    query: &str,
) -> Vec<usize> {
    let mut matches = Vec::new();
    if query.is_empty() {
        return matches;
    }
    let query_lower = query.to_lowercase();
    if let Some((ref lines, _, _, _)) = *rendered {
        for (line_idx, line) in lines.iter().enumerate() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.to_lowercase().contains(&query_lower) {
                matches.push(line_idx);
            }
        }
    }
    matches
}

pub fn update_pane_search_matches(pane: &mut ConversationPane) {
    pane.search_matches = compute_search_matches(&pane.rendered, &pane.search_query);
    pane.search_current = 0;
    if let Some(&first_match) = pane.search_matches.first() {
        pane.scroll = first_match;
    }
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();

    let show_warning = state.retention_warning.is_some() && !state.retention_warning_dismissed && !state.loading;
    let warning_height = if show_warning { 4 } else { 0 };

    let chunks = Layout::vertical([
        Constraint::Length(1),              // Header
        Constraint::Length(warning_height), // Warning banner
        Constraint::Length(1),              // Tabs
        Constraint::Min(0),                 // Content
    ])
    .split(area);

    draw_header(frame, chunks[0], state);

    if show_warning {
        draw_retention_warning(frame, chunks[1], state);
    }

    if state.loading {
        let f = state.animation_frame;

        // Timing - balanced speed
        let slow = f / 2;

        // Elegant color palette
        let primary = theme::PRIMARY;
        let warm = theme::WARM;
        let dim = theme::DIM;
        let faint = theme::FAINT;

        // Logo with creative animation (Claude logo - aligned)
        let logo_lines = [" ▐▛███▜▌", "▝▜█████▛▘", " ▘▘ ▝▝"];

        // Random fragment characters for the chaos phase
        let fragment_chars = ['░', '▒', '▓', '█', '▄', '▀', '▌', '▐', '▖', '▗', '▘', '▝'];

        // Build logo from random fragments assembling into final form
        // Animation cycle: 0-5 blank, 5-20 chaos starts, 20-80 lock-in, 80-120 visible, 120-150 fade out
        let cycle_total = 150;
        let cycle = slow % cycle_total;
        let fade_start = 120;

        let build_logo_line = |line: &str, line_idx: usize| -> Vec<Span> {
            let chars: Vec<char> = line.chars().collect();

            chars
                .iter()
                .enumerate()
                .map(|(i, &c)| {
                    if c == ' ' {
                        return Span::styled(" ", Style::default());
                    }

                    // Pseudo-random seed for this character position
                    let seed = (i * 17 + line_idx * 31 + 7) % 100;
                    // Character locks in at this frame (spread across frames 20-80)
                    let lock_frame = 20 + (seed * 60 / 100);

                    // Fade out phase (frames 120-150)
                    if cycle >= fade_start {
                        let fade_progress = (cycle - fade_start) as f32 / 30.0;
                        // Each character fades out at different times based on seed
                        let char_fade_start = (seed as f32 / 100.0) * 0.6;
                        let char_fade = ((fade_progress - char_fade_start) / 0.4).clamp(0.0, 1.0);
                        let brightness = 1.0 - char_fade;

                        if brightness < 0.05 {
                            return Span::styled(" ", Style::default());
                        }

                        let color = theme::primary_with_intensity(brightness as f64);
                        Span::styled(c.to_string(), Style::default().fg(color))
                    } else if cycle >= lock_frame {
                        // Character is locked in - show final form with shimmer
                        let settle_time = cycle - lock_frame;
                        let shimmer = if settle_time < 10 {
                            // Brief bright flash when locking in
                            1.2 - (settle_time as f32 * 0.02)
                        } else {
                            (slow as f32 * 0.15 + i as f32 * 0.5).sin() * 0.15 + 0.85
                        };

                        let color = theme::primary_with_intensity((shimmer as f64).min(1.17));
                        Span::styled(c.to_string(), Style::default().fg(color))
                    } else if cycle >= 5 {
                        // Chaos phase - show random fragments that shift around
                        let chaos_seed = (slow + i * 13 + line_idx * 23) % fragment_chars.len();
                        let fragment = fragment_chars[chaos_seed];

                        // Fragments get brighter as we approach lock-in
                        let proximity = (cycle as f32 / lock_frame as f32).min(1.0);
                        let brightness = 0.3 + proximity * 0.5;

                        // Occasionally flicker to the correct character
                        let flicker = (slow + i * 7).is_multiple_of(12) && proximity > 0.7;
                        let display_char = if flicker { c } else { fragment };

                        let color = theme::primary_with_intensity(brightness as f64);
                        Span::styled(display_char.to_string(), Style::default().fg(color))
                    } else {
                        // Initial blank phase
                        Span::styled(" ", Style::default())
                    }
                })
                .collect()
        };

        let logo1_spans = build_logo_line(logo_lines[0], 0);
        let logo2_spans = build_logo_line(logo_lines[1], 1);
        let logo3_spans = build_logo_line(logo_lines[2], 2);

        // Claude Code style star field with spinner characters
        let star_chars = ['·', '✢', '✳', '✶', '✻', '✽'];
        let make_starfield = |offset: usize| -> Vec<Span> {
            (0..32)
                .map(|i| {
                    let seed = (i * 13 + offset) % 97;
                    let twinkle = (slow + seed) % 48;
                    let (ch, color) = if seed.is_multiple_of(8) {
                        let char_idx = (twinkle / 8) % star_chars.len();
                        match twinkle {
                            0..=7 => (star_chars[char_idx], primary),
                            8..=23 => (star_chars[(char_idx + 1) % star_chars.len()], warm),
                            24..=35 => ('·', dim),
                            _ => (' ', faint),
                        }
                    } else {
                        (' ', faint)
                    };
                    Span::styled(ch.to_string(), Style::default().fg(color))
                })
                .collect()
        };

        // Title with Claude Code style icon and gentle wave
        let icon_frames = ['✻', '✶', '✳', '✢'];
        let icon_idx = (slow / 6) % icon_frames.len();
        let icon = icon_frames[icon_idx];

        let title = "C C S I G H T";
        let mut title_spans: Vec<Span> = vec![Span::styled(
            format!("{icon} "),
            Style::default().fg(primary).add_modifier(Modifier::BOLD),
        )];
        title_spans.extend(title.chars().enumerate().map(|(i, c)| {
            let wave = ((slow as f32 * 0.1 + i as f32 * 0.3).sin() * 15.0 + 15.0) as u8;
            let color = Color::Rgb(150 + wave, 110 + wave / 2, 90 + wave / 3);
            Span::styled(
                c.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        }));

        // Claude Code style spinner
        let spinner_frames = ['·', '✢', '✳', '✶', '✻', '✽'];
        let spinner_idx = (slow / 4) % spinner_frames.len();

        // Animated spinner with trail effect
        let bar: Vec<Span> = (0..6)
            .map(|i| {
                let frame_idx = (spinner_idx + 6 - i) % spinner_frames.len();
                let intensity = 1.0 - (i as f32 * 0.15);
                let color = theme::primary_with_intensity(intensity as f64);
                Span::styled(
                    format!(" {} ", spinner_frames[frame_idx]),
                    Style::default().fg(color),
                )
            })
            .collect();

        // Status message - creative rotating messages
        let messages = [
            "Deliberating",
            "Reticulating",
            "Vibing",
            "Mulling",
            "Puzzling",
            "Wibbling",
            "Elucidating",
            "Sussing",
            "Concocting",
            "Envisioning",
            "Actualizing",
            "Processing",
            "Channelling",
            "Wrangling",
            "Stewing",
            "Smooshing",
            "Moseying",
            "Germinating",
            "Brewing",
            "Schlepping",
            "Shimmying",
            "Effecting",
        ];
        let msg_idx = (slow / 25) % messages.len();
        let msg = messages[msg_idx];

        // Gentle cursor blink
        let cursor = if (slow / 8).is_multiple_of(2) {
            "▎"
        } else {
            " "
        };

        // Decorative line
        let deco_line: String = (0..28)
            .map(|i| {
                let pos = (slow + i * 2) % 56;
                if pos == i || pos == 56 - i {
                    '─'
                } else {
                    ' '
                }
            })
            .collect();

        let loading_text = vec![
            Line::from(""),
            Line::from(make_starfield(0)),
            Line::from(""),
            Line::from(""),
            Line::from(
                vec![Span::raw("        ")]
                    .into_iter()
                    .chain(logo1_spans)
                    .collect::<Vec<_>>(),
            ),
            Line::from(
                vec![Span::raw("        ")]
                    .into_iter()
                    .chain(logo2_spans)
                    .collect::<Vec<_>>(),
            ),
            Line::from(
                vec![Span::raw("        ")]
                    .into_iter()
                    .chain(logo3_spans)
                    .collect::<Vec<_>>(),
            ),
            Line::from(""),
            Line::from(
                vec![Span::raw("        ")]
                    .into_iter()
                    .chain(title_spans)
                    .collect::<Vec<_>>(),
            ),
            Line::from(""),
            Line::from(Span::styled(
                format!("    {deco_line}"),
                Style::default().fg(faint),
            )),
            Line::from(""),
            Line::from(
                vec![Span::raw("      ")]
                    .into_iter()
                    .chain(bar)
                    .collect::<Vec<_>>(),
            ),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("        {msg}..."), Style::default().fg(dim)),
                Span::styled(cursor, Style::default().fg(primary)),
            ]),
            Line::from(""),
            Line::from(make_starfield(17)),
            Line::from(""),
            Line::from(Span::styled(
                "        press q to quit",
                Style::default().fg(faint),
            )),
        ];
        let loading = Paragraph::new(loading_text)
            .block(Block::default().borders(Borders::NONE))
            .centered();
        let content_height = 18;
        let split_result = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(content_height),
            Constraint::Min(0),
        ])
        .flex(ratatui::layout::Flex::Center)
        .split(chunks[3]);
        let centered_area = split_result.get(1).copied().unwrap_or(chunks[3]);
        frame.render_widget(loading, centered_area);
    } else if let Some(err) = state.error.clone() {
        draw_tabs(frame, chunks[2], state);
        let error = Paragraph::new(format!("Error: {err}"))
            .style(Style::default().fg(theme::ERROR))
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Span::styled(" Error ", Style::default().fg(theme::ERROR))));
        frame.render_widget(error, chunks[3]);
    } else {
        if !state.show_conversation {
            draw_tabs(frame, chunks[2], state);
        }
        match state.tab {
            Tab::Dashboard => dashboard::draw_dashboard(frame, chunks[3], state),
            Tab::Daily => {
                if !state.show_conversation {
                    draw_daily(frame, chunks[3], state);
                }
            }
            Tab::Insights => insights::draw_insights(frame, chunks[3], state),
        }
    }

    if state.show_detail {
        draw_detail_popup(frame, area, state);
    }

    if state.show_dashboard_detail {
        dashboard::draw_dashboard_detail_popup(frame, area, state);
    }

    if state.show_insights_detail {
        insights::draw_insights_detail_popup(frame, area, state);
    }

    if state.show_conversation {
        let conv_layout = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
        let conv_area = conv_layout[0];
        let help_area = conv_layout[1];

        if !state.panes.is_empty() {
            let pane_count = state.panes.len();
            let min_pane_width = crate::MIN_PANE_WIDTH;

            // Dynamic session list width based on available space
            let session_list_width = if conv_area.width >= 100 {
                36
            } else if conv_area.width >= 80 {
                30
            } else if conv_area.width >= 68 {
                26
            } else {
                0
            };

            let show_session_list = !state.session_list_hidden && session_list_width > 0;

            let available_for_panes = if show_session_list {
                conv_area.width.saturating_sub(session_list_width)
            } else {
                conv_area.width
            };

            let pane_width = available_for_panes / pane_count as u16;
            let layout_too_narrow = pane_width < min_pane_width && pane_count > 1;

            let mut constraints = Vec::new();
            if show_session_list {
                constraints.push(Constraint::Length(session_list_width));
            }
            let pane_percentage = 100 / pane_count as u16;
            for _ in 0..pane_count {
                constraints.push(Constraint::Percentage(pane_percentage));
            }
            let layout = Layout::horizontal(constraints).split(conv_area);

            let layout_offset = if show_session_list { 1 } else { 0 };

            if show_session_list {
                draw_split_session_list(frame, layout[0], state, state.active_pane_index.is_none());
            } else {
                state.session_list_area = None;
            }

            // Store pane areas for click detection
            state.pane_areas.clear();
            for i in 0..pane_count {
                state.pane_areas.push(layout[i + layout_offset]);
            }

            let sessions: &[SessionInfo] = state
                .daily_groups
                .get(state.selected_day)
                .map_or(&[], |g| g.sessions.as_slice());
            let selecting = state.selecting;
            let active_pane_index = state.active_pane_index;
            for i in 0..state.panes.len() {
                let is_active = active_pane_index == Some(i);
                let pane_area = layout[i + layout_offset];
                let ca = draw_conversation_pane(
                    frame,
                    pane_area,
                    &mut state.panes[i],
                    is_active,
                    &state.toast_message,
                    &state.toast_time,
                    layout_too_narrow,
                    selecting,
                    sessions,
                    &state.original_daily_groups,
                    &state.pins,
                );
                if is_active {
                    state.conversation_content_area = ca;
                }
            }
        } else {
            state.pane_areas.clear();
        }

        let help_spans = vec![
            Span::styled(" Esc", Style::default().fg(theme::PRIMARY)),
            Span::styled(":back ", Style::default().fg(theme::DIM)),
            Span::styled("↑↓", Style::default().fg(theme::PRIMARY)),
            Span::styled(":scroll ", Style::default().fg(theme::DIM)),
            Span::styled("/", Style::default().fg(theme::PRIMARY)),
            Span::styled(":search ", Style::default().fg(theme::DIM)),
            Span::styled("i", Style::default().fg(theme::PRIMARY)),
            Span::styled(":info ", Style::default().fg(theme::DIM)),
            Span::styled("s", Style::default().fg(theme::PRIMARY)),
            Span::styled(":summary ", Style::default().fg(theme::DIM)),
            Span::styled("y", Style::default().fg(theme::PRIMARY)),
            Span::styled(":copy ", Style::default().fg(theme::DIM)),
            Span::styled("H", Style::default().fg(theme::PRIMARY)),
            Span::styled(":list", Style::default().fg(theme::DIM)),
        ];
        let help_line = Paragraph::new(Line::from(help_spans));
        frame.render_widget(help_line, help_area);

        if state.show_conversation_detail {
            let fp = state
                .active_pane_index
                .and_then(|i| state.panes.get(i))
                .and_then(|p| p.file_path.clone())
                .or_else(|| crate::get_conv_session_file(state, state.selected_session));
            let session = fp.and_then(|fp| {
                state.original_daily_groups.iter().find_map(|g| {
                    g.sessions.iter().find(|s| s.file_path == fp)
                })
            });
            if let Some(session) = session {
                let pinned = state.pins.is_pinned(&session.file_path);
                draw_session_detail(frame, area, session, " Space:pin  i:close ", pinned);
            }
        }
    }

    if state.show_filter_popup {
        draw_filter_popup(frame, area, state);
    }

    if state.show_project_popup {
        draw_project_popup(frame, area, state);
    }

    if state.show_summary {
        draw_summary(frame, chunks[3], state);
    }

    if state.show_help {
        draw_help_popup(frame, area, state);
    }

    if state.search_mode {
        draw_search_popup(frame, area, state);
    }

    if let Some((sc, sr, ec, er)) = state.text_selection {
        let (start_col, start_row, end_col, end_row) = if (sr, sc) <= (er, ec) {
            (sc, sr, ec, er)
        } else {
            (ec, er, sc, sr)
        };

        let clamp_area = if state.show_conversation {
            state.conversation_content_area.filter(|ca| {
                start_row >= ca.y
                    && start_row < ca.y + ca.height
                    && start_col >= ca.x
                    && start_col < ca.x + ca.width
            })
        } else {
            None
        };

        let buf = frame.buffer_mut();
        let buf_area = buf.area;
        for row in start_row..=end_row {
            if row < buf_area.y || row >= buf_area.y + buf_area.height {
                continue;
            }
            if let Some(ca) = clamp_area
                && (row < ca.y || row >= ca.y + ca.height) {
                    continue;
                }
            let col_start = if row == start_row {
                start_col
            } else {
                clamp_area.map_or(buf_area.x, |ca| ca.x)
            };
            let col_end = if row == end_row {
                end_col
            } else {
                clamp_area.map_or(buf_area.x + buf_area.width - 1, |ca| {
                    ca.x + ca.width - 1
                })
            };
            for col in col_start..=col_end.min(buf_area.x + buf_area.width - 1) {
                let cell = &mut buf[(col, row)];
                cell.set_bg(theme::SELECTION);
            }
        }
    }

}

fn draw_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let soft = theme::WARM;
    let dim = theme::DIM;

    let mut spans = vec![
        Span::styled("  ◈  ", Style::default().fg(theme::PRIMARY)),
        Span::styled(
            "C C S I G H T",
            Style::default().fg(soft).add_modifier(Modifier::BOLD),
        ),
    ];

    let session_count: usize = state
        .daily_groups
        .iter()
        .map(|g| g.sessions.iter().filter(|s| !s.is_subagent).count())
        .sum();
    if session_count > 0 {
        spans.push(Span::styled(
            format!("  ·  {session_count} sessions"),
            Style::default().fg(dim),
        ));
    }

    if let Some(ref cache) = state.cache_stats
        && cache.cached_files > 0 {
            spans.push(Span::styled(
                format!(
                    "  ·  cache {}/{}",
                    cache.cached_files,
                    cache.cached_files + cache.parsed_files
                ),
                Style::default().fg(theme::DIM),
            ));
        }

    let title = Paragraph::new(Line::from(spans));
    frame.render_widget(title, area);
}

fn draw_retention_warning(frame: &mut Frame, area: Rect, state: &AppState) {
    if let Some(ref warning) = state.retention_warning {
        let line1 = if warning.is_default {
            "⚠ Log retention period is not set (default: 30 days). Setting a longer period is recommended for ccsight.".to_string()
        } else {
            format!("⚠ Log retention period is set to {} days. A longer period is recommended for ccsight.", warning.days)
        };
        let line2 = if warning.is_default {
            "  → Add { \"cleanupPeriodDays\": 36500 } to ~/.claude/settings.json"
        } else {
            "  → Increase cleanupPeriodDays in ~/.claude/settings.json (e.g., 36500)"
        };

        let content = vec![
            Line::from(Span::styled(line1, Style::default().fg(theme::WARNING))),
            Line::from(Span::styled(line2, Style::default().fg(theme::DIM))),
            Line::from(vec![
                Span::styled("  Docs: ", Style::default().fg(theme::DIM)),
                Span::styled(
                    "https://code.claude.com/docs/en/settings",
                    Style::default().fg(theme::LINK),
                ),
                Span::styled("  |  x to dismiss", Style::default().fg(theme::DIM)),
            ]),
        ];

        let banner = Paragraph::new(content).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme::SEPARATOR)),
        );
        frame.render_widget(banner, area);
    }
}

fn draw_tabs(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let dim = theme::DIM;

    let today = chrono::Local::now().date_naive();
    let today_sessions = state
        .daily_groups
        .iter()
        .find(|g| g.date == today)
        .map_or(0, |g| g.sessions.iter().filter(|s| !s.is_subagent).count());
    let today_tokens: u64 = state
        .daily_groups
        .iter()
        .find(|g| g.date == today)
        .map_or(0, |g| {
            g.sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .flat_map(|s| s.day_tokens_by_model.values())
                .map(super::aggregator::stats::TokenStats::work_tokens)
                .sum()
        });

    let tabs_data = [
        (Tab::Dashboard, "1", "Dashboard".to_string()),
        (Tab::Daily, "2", format!("Daily ({today_sessions})")),
        (
            Tab::Insights,
            "3",
            format!("Insights ({})", crate::format_number(today_tokens)),
        ),
    ];

    // Clear and rebuild tab areas
    state.tab_areas.clear();
    let mut current_x = area.x + 1; // Start after initial space

    let mut all_spans = vec![Span::styled(" ", Style::default())];
    for (i, (tab, key, label)) in tabs_data.iter().enumerate() {
        let is_selected = state.tab == *tab;
        let tab_width = if is_selected {
            unicode_width::UnicodeWidthStr::width(label.as_str()) + 2 // " label "
        } else {
            key.len() + 1 + unicode_width::UnicodeWidthStr::width(label.as_str()) + 1 // "N:label "
        };

        // Store clickable area for this tab
        state
            .tab_areas
            .push((*tab, Rect::new(current_x, area.y, tab_width as u16, 1)));

        if is_selected {
            all_spans.push(Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(theme::TEXT_DARK)
                    .bg(theme::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            all_spans.push(Span::styled(
                format!("{key}:"),
                Style::default().fg(theme::FAINT),
            ));
            all_spans.push(Span::styled(format!("{label} "), Style::default().fg(dim)));
        }

        current_x += tab_width as u16;

        if i < 2 {
            all_spans.push(Span::styled("  ", Style::default()));
            current_x += 2;
        }
    }

    let filter_label = if state.period_filter != crate::PeriodFilter::All {
        let range = state.period_filter.date_range_label();
        if range.is_empty() {
            format!(" {} ", state.period_filter.label())
        } else {
            format!(" {} {} ", state.period_filter.label(), range)
        }
    } else {
        " f:Filter ".to_string()
    };
    let filter_width = unicode_width::UnicodeWidthStr::width(filter_label.as_str()) as u16;

    let project_label = if let Some(ref project) = state.project_filter {
        let short = project.rsplit('/').next().unwrap_or(project);
        format!(" {short} ")
    } else {
        " p:Project ".to_string()
    };
    let project_width = unicode_width::UnicodeWidthStr::width(project_label.as_str()) as u16;

    let pin_count = state.pins.entries().len();
    let pin_label = if pin_count > 0 {
        format!(" *{pin_count} ")
    } else {
        String::new()
    };
    let pin_width = unicode_width::UnicodeWidthStr::width(pin_label.as_str()) as u16;
    let help_label = " ? ";
    let help_width = 3u16;

    let buttons_width = filter_width + project_width + pin_width + help_width + 1;
    if area.width > buttons_width + current_x - area.x {
        let right_x = area.x + area.width - buttons_width;
        let gap = (right_x - current_x) as usize;

        let filter_area = Rect::new(right_x, area.y, filter_width, 1);
        state.filter_popup_area_trigger = Some(filter_area);
        let filter_style = if state.period_filter != crate::PeriodFilter::All {
            Style::default()
                .fg(theme::TEXT_DARK)
                .bg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::DIM)
        };
        all_spans.push(Span::raw(" ".repeat(gap)));
        all_spans.push(Span::styled(filter_label, filter_style));

        let project_area = Rect::new(right_x + filter_width, area.y, project_width, 1);
        state.project_popup_area_trigger = Some(project_area);
        let project_style = if state.project_filter.is_some() {
            Style::default()
                .fg(theme::TEXT_DARK)
                .bg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::DIM)
        };
        all_spans.push(Span::styled(project_label, project_style));

        if pin_count > 0 {
            let pin_area =
                Rect::new(right_x + filter_width + project_width, area.y, pin_width, 1);
            state.pin_view_trigger = Some(pin_area);
            all_spans.push(Span::styled(
                pin_label,
                Style::default().fg(theme::WARNING),
            ));
        } else {
            state.pin_view_trigger = None;
        }

        let help_x = right_x + filter_width + project_width + pin_width;
        let help_area = Rect::new(help_x, area.y, help_width, 1);
        state.help_trigger = Some(help_area);
        all_spans.push(Span::styled(help_label, Style::default().fg(theme::DIM)));
    } else {
        state.filter_popup_area_trigger = None;
        state.project_popup_area_trigger = None;
        state.pin_view_trigger = None;
        state.help_trigger = None;
    }

    let tab_line = Paragraph::new(Line::from(all_spans));
    frame.render_widget(tab_line, area);
}


fn draw_daily(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if state.daily_groups.is_empty() {
        let empty = Paragraph::new("No sessions found")
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Span::styled(" Daily ", Style::default().fg(theme::PRIMARY))));
        frame.render_widget(empty, area);
        return;
    }

    if state.selected_day >= state.daily_groups.len() {
        state.selected_day = state.daily_groups.len().saturating_sub(1);
    }
    let group = &state.daily_groups[state.selected_day];
    let today = Local::now().date_naive();
    let is_today = group.date == today;

    let all_sessions = &group.sessions;
    let sessions: Vec<_> = all_sessions.iter().filter(|s| !s.is_subagent).collect();

    let show_stats_panel = area.width >= 60;

    let (header_chunk, stats_chunk, sessions_chunk, help_chunk) = if show_stats_panel {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(9),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], Some(chunks[1]), chunks[2], chunks[3])
    } else {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], None, chunks[1], chunks[2])
    };

    let date_str = group.date.format("%Y-%m-%d (%a)").to_string();
    let date_label = if is_today {
        format!("🟢 {date_str} - Today")
    } else {
        date_str
    };

    let left_arrow = if state.selected_day < state.daily_groups.len().saturating_sub(1) {
        "◀ "
    } else {
        "  "
    };
    let right_arrow = if state.selected_day > 0 { " ▶" } else { "  " };

    let nav_text = format!(
        "{}{}{}  ({}/{})",
        left_arrow,
        date_label,
        right_arrow,
        state.selected_day + 1,
        state.daily_groups.len(),
    );

    let nav = Paragraph::new(nav_text)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)))
        .centered();

    state.daily_header_area = Some(header_chunk);
    frame.render_widget(nav, header_chunk);

    let continued_count = sessions.iter().filter(|s| s.is_continued).count();
    let new_count = sessions.len() - continued_count;

    let mut hourly_tokens: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
    let mut project_tokens: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    let mut model_tokens: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut tool_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for s in all_sessions {
        for (hour, tokens) in &s.day_hourly_work_tokens {
            *hourly_tokens.entry(*hour).or_insert(0) += tokens;
        }
        let mut session_total: u64 = 0;
        for (model, ts) in &s.day_tokens_by_model {
            let model_total = ts.work_tokens();
            *model_tokens.entry(model.clone()).or_insert(0) += model_total;
            session_total += model_total;
        }
        for (tool, count) in &s.day_tool_usage {
            *tool_counts.entry(tool.clone()).or_insert(0) += count;
        }
        let proj = shorten_project(&s.project_name).to_string();
        *project_tokens.entry(proj).or_insert(0) += session_total;
    }

    let max_hourly_raw = hourly_tokens.values().max().copied().unwrap_or(0);
    let max_hourly = max_hourly_raw.max(1);
    let active_hours: usize = hourly_tokens.values().filter(|&&t| t > 0).count();
    let peak_hour = hourly_tokens
        .iter()
        .max_by_key(|(_, t)| *t)
        .map(|(h, t)| (*h, *t));

    let total_day_tokens: u64 = all_sessions
        .iter()
        .map(|s| {
            s.day_tokens_by_model
                .values()
                .map(super::aggregator::stats::TokenStats::work_tokens)
                .sum::<u64>()
        })
        .sum();

    let calculator = CostCalculator::global();
    let total_day_cost: f64 = all_sessions
        .iter()
        .map(|s| {
            s.day_tokens_by_model
                .iter()
                .map(|(model, ts)| {
                    calculator
                        .calculate_cost(ts, Some(model.as_str()))
                        .unwrap_or(0.0)
                })
                .sum::<f64>()
        })
        .sum();

    let show_timeline = area.width >= 80;
    let (timeline_area, breakdown_area) = if let Some(stats_area) = stats_chunk {
        if show_timeline {
            let panel =
                Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
                    .split(stats_area);
            (Some(panel[0]), Some(panel[1]))
        } else {
            (None, Some(stats_area))
        }
    } else {
        (None, None)
    };

    if let Some(timeline_rect) = timeline_area {
        let bar_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let inner_height = timeline_rect.height.saturating_sub(4) as usize;

        let mut timeline_lines: Vec<Line> = Vec::new();

        let has_data = max_hourly_raw > 0;
        let y_label_width = if has_data { 6 } else { 0 };
        for row in (0..inner_height).rev() {
            let threshold = (row as f64 + 0.5) / inner_height as f64;
            let y_label = if !has_data {
                String::new()
            } else if row == inner_height - 1 {
                format!("{:>5} ", crate::format_number(max_hourly))
            } else {
                " ".repeat(y_label_width)
            };
            let mut row_chars = String::new();
            for hour in 0..24u8 {
                let tokens = hourly_tokens.get(&hour).copied().unwrap_or(0);
                let ratio = if max_hourly > 0 {
                    tokens as f64 / max_hourly as f64
                } else {
                    0.0
                };
                if ratio >= threshold {
                    row_chars.push(bar_chars[7]);
                } else if ratio >= threshold - (1.0 / inner_height as f64) && ratio > 0.0 {
                    let sub_level = ((ratio - (threshold - 1.0 / inner_height as f64))
                        * inner_height as f64
                        * 8.0) as usize;
                    row_chars.push(bar_chars[sub_level.min(7)]);
                } else {
                    row_chars.push(' ');
                }
            }
            timeline_lines.push(Line::from(vec![
                Span::styled(y_label, Style::default().fg(theme::DIM)),
                Span::styled(row_chars, Style::default().fg(theme::PRIMARY)),
            ]));
        }

        let time_label = if timeline_rect.width >= 30 {
            "0     6    12    18   24"
        } else {
            "0  6  12  18  24"
        };
        timeline_lines.push(Line::from(Span::styled(
            time_label,
            Style::default().fg(theme::DIM),
        )).alignment(ratatui::layout::Alignment::Center));

        let inner_width = timeline_rect.width.saturating_sub(2) as usize;
        if inner_width >= 28 {
            let peak_info = if let Some((hour, _)) = peak_hour {
                format!(" peak:{}-{}", hour, hour + 1)
            } else {
                String::new()
            };
            timeline_lines.push(Line::from(vec![
                Span::styled(
                    format!("active:{active_hours}h"),
                    Style::default().fg(theme::DIM),
                ),
                Span::styled(peak_info, Style::default().fg(theme::WARM)),
                Span::styled(" ", Style::default().fg(theme::DIM)),
                Span::styled(
                    crate::format_number(total_day_tokens),
                    Style::default().fg(theme::PRIMARY),
                ),
                Span::styled(" ", Style::default().fg(theme::DIM)),
                Span::styled(
                    format!("${:.0}", total_day_cost.max(0.0)),
                    cost_style(total_day_cost),
                ),
            ]).alignment(ratatui::layout::Alignment::Center));
        } else {
            timeline_lines.push(Line::from(vec![
                Span::styled(
                    format!("{active_hours}h "),
                    Style::default().fg(theme::SUCCESS),
                ),
                Span::styled(
                    format!("${:.0}", total_day_cost.max(0.0)),
                    cost_style(total_day_cost),
                ),
            ]).alignment(ratatui::layout::Alignment::Center));
        }

        let timeline =
            Paragraph::new(timeline_lines).block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .title(Span::styled(" Activity ", Style::default().fg(theme::PRIMARY)),
            ));
        frame.render_widget(timeline, timeline_rect);
    }

    let mut sorted_projects: Vec<_> = project_tokens.iter().collect();
    sorted_projects.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let total_project_tokens: u64 = project_tokens.values().sum();
    let mut sorted_models: Vec<_> = model_tokens.iter().collect();
    sorted_models.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    let total_model_tokens: u64 = model_tokens.values().sum();

    let mut sorted_tools: Vec<_> = tool_counts.iter().collect();
    sorted_tools.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let total_tool_count: usize = tool_counts.values().sum();

    let mut all_items: Vec<BreakdownItem> = vec![];
    for (proj, tokens) in &sorted_projects {
        let pct = if total_project_tokens > 0 {
            **tokens as f64 / total_project_tokens as f64 * 100.0
        } else {
            0.0
        };
        all_items.push(BreakdownItem::Project((*proj).clone(), **tokens, pct));
    }
    for (model, tokens) in &sorted_models {
        let pct = if total_model_tokens > 0 {
            **tokens as f64 / total_model_tokens as f64 * 100.0
        } else {
            0.0
        };
        all_items.push(BreakdownItem::Model((*model).clone(), **tokens, pct));
    }
    let tools_start_idx = sorted_projects.len() + sorted_models.len();
    for (tool, count) in &sorted_tools {
        let pct = if total_tool_count > 0 {
            **count as f64 / total_tool_count as f64 * 100.0
        } else {
            0.0
        };
        all_items.push(BreakdownItem::Tool((*tool).clone(), **count, pct));
    }

    let total_items = all_items.len();

    fn render_column_items(
        items: &[(String, String, f64)],
        color: Color,
        max_lines: usize,
        col_width: usize,
    ) -> Vec<Line<'static>> {
        items
            .iter()
            .take(max_lines)
            .map(|(name, info, pct)| {
                let bar_len = (*pct / 100.0 * 3.0).round() as usize;
                let bar = "█".repeat(bar_len);
                let max_name = col_width.saturating_sub(bar_len + info.len() + 3);
                let short: String = name.chars().take(max_name).collect();
                Line::from(vec![
                    Span::styled(format!(" {bar}"), Style::default().fg(color)),
                    Span::styled(
                        format!(" {short} {info}"),
                        Style::default().fg(theme::TEXT_BRIGHT),
                    ),
                ])
            })
            .collect()
    }

    state.breakdown_panel_area = breakdown_area;
    let breakdown_popup_data = if let Some(breakdown_rect) = breakdown_area {
        let outer_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER))
            .title(Span::styled(
                format!(" Breakdown ({total_items}) [b] "),
                Style::default().fg(theme::PRIMARY),
            ));
        let inner = outer_block.inner(breakdown_rect);
        frame.render_widget(outer_block, breakdown_rect);

        let cols = Layout::horizontal([
            Constraint::Percentage(33),
            Constraint::Percentage(34),
            Constraint::Percentage(33),
        ])
        .split(inner);

        let max_lines = inner.height.saturating_sub(1) as usize;

        let proj_items: Vec<_> = sorted_projects.iter().map(|(name, tokens)| {
            let short = shorten_project(name);
            let pct = if total_project_tokens > 0 { **tokens as f64 / total_project_tokens as f64 * 100.0 } else { 0.0 };
            (short.to_string(), format!("{pct:.0}%"), pct)
        }).collect();
        let model_items: Vec<_> = sorted_models.iter().map(|(name, tokens)| {
            let short = crate::aggregator::normalize_model_name(name);
            let pct = if total_model_tokens > 0 { **tokens as f64 / total_model_tokens as f64 * 100.0 } else { 0.0 };
            (short, format!("{pct:.0}%"), pct)
        }).collect();
        let tool_items: Vec<_> = sorted_tools.iter().map(|(name, count)| {
            let pct = if total_tool_count > 0 { **count as f64 / total_tool_count as f64 * 100.0 } else { 0.0 };
            ((*name).clone(), format!("{count}x"), pct)
        }).collect();

        let proj_lines = render_column_items(&proj_items, theme::WARM, max_lines, cols[0].width as usize);
        let model_lines = render_column_items(&model_items, theme::PRIMARY, max_lines, cols[1].width as usize);
        let tool_lines = render_column_items(&tool_items, theme::SUCCESS, max_lines, cols[2].width as usize);

        let proj_title = format!(" Projects({}) ", sorted_projects.len());
        let model_title = format!(" Models({}) ", sorted_models.len());
        let tool_title = format!(" Tools({}) ", sorted_tools.len());

        frame.render_widget(
            Paragraph::new(proj_lines).block(Block::default().title(
                Span::styled(proj_title, Style::default().fg(theme::WARM)))),
            cols[0],
        );
        frame.render_widget(
            Paragraph::new(model_lines).block(Block::default().title(
                Span::styled(model_title, Style::default().fg(theme::PRIMARY)))),
            cols[1],
        );
        frame.render_widget(
            Paragraph::new(tool_lines).block(Block::default().title(
                Span::styled(tool_title, Style::default().fg(theme::SUCCESS)))),
            cols[2],
        );

        if state.daily_breakdown_focus {
            Some((all_items, sorted_projects.len(), tools_start_idx))
        } else {
            None
        }
    } else {
        None
    };

    let content_width = area.width.saturating_sub(4) as usize;
    let max_summary_len = content_width.saturating_sub(4).max(15);

    let session_calculator = CostCalculator::global();

    let items: Vec<ListItem> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let start_time = s.day_first_timestamp.with_timezone(&Local).format("%H:%M");
            let end_time = s.day_last_timestamp.with_timezone(&Local).format("%H:%M");
            let time_str = format!("{start_time}–{end_time}");
            let session_tokens: u64 = s
                .day_tokens_by_model
                .values()
                .map(super::aggregator::stats::TokenStats::work_tokens)
                .sum();
            let tokens_str = crate::format_number(session_tokens);
            let session_cost: f64 = s.day_tokens_by_model.iter()
                .map(|(m, t)| session_calculator.calculate_cost(t, Some(m)).unwrap_or(0.0))
                .sum();
            let cost_str = format_cost(session_cost, 0);

            let model_short = s
                .model
                .as_ref().map_or_else(|| "?".to_string(), |m| crate::aggregator::normalize_model_name(m));
            let model_clr = s
                .model
                .as_ref()
                .map_or(theme::LABEL_MUTED, |m| model_color(m));

            let is_selected = i == state.selected_session;
            let is_updating = state.updating_session == Some((state.selected_day, i));
            let now = chrono::Utc::now();
            let is_recent = (now - s.day_last_timestamp).num_minutes() < 5;
            let prefix = if is_updating {
                "🔄"
            } else if is_selected {
                "▶ "
            } else {
                "  "
            };

            let project_short = shorten_project(&s.project_name);

            let summary_text = s.summary.as_ref().map(|sum| {
                let truncated = truncate_to_display_width(sum, max_summary_len);
                if unicode_width::UnicodeWidthStr::width(sum.as_str()) > max_summary_len {
                    format!("{truncated}...")
                } else {
                    truncated
                }
            });

            let time_style = if is_recent {
                Style::default().fg(theme::ACCENT)
            } else {
                Style::default().fg(theme::LABEL_SUBTLE)
            };

            let branch_short = s.git_branch.as_ref().map(|b| {
                let name = b.split('/').next_back().unwrap_or(b);
                if name.chars().count() > 12 {
                    format!("{}…", name.chars().take(11).collect::<String>())
                } else {
                    name.to_string()
                }
            });

            let mut line1_spans = vec![Span::raw(prefix)];
            let pinned = state.pins.is_pinned(&s.file_path);
            if pinned {
                line1_spans.push(Span::styled("*", Style::default().fg(theme::WARNING)));
                line1_spans.push(Span::styled(
                    if s.is_continued { "»" } else { "·" },
                    Style::default().fg(if s.is_continued { theme::PRIMARY } else { theme::SUCCESS }),
                ));
            } else {
                line1_spans.push(Span::styled(
                    if s.is_continued { " »" } else { " ·" },
                    Style::default().fg(if s.is_continued { theme::PRIMARY } else { theme::SUCCESS }),
                ));
            }
            line1_spans.push(Span::styled(format!("{time_str} "), time_style));
            line1_spans.push(Span::styled(
                project_short.to_string(),
                Style::default().fg(theme::WARM),
            ));
            if let Some(ref branch) = branch_short {
                line1_spans.push(Span::styled(
                    format!("#{branch}"),
                    Style::default().fg(theme::BRANCH),
                ));
            }
            line1_spans.push(Span::styled(
                format!("  {tokens_str}"),
                Style::default().fg(theme::PRIMARY),
            ));
            line1_spans.push(Span::styled(
                format!(" {cost_str}"),
                cost_style(session_cost),
            ));
            line1_spans.push(Span::styled(
                format!(" [{model_short}]"),
                Style::default().fg(model_clr),
            ));

            let line1 = Line::from(line1_spans);

            let line2_spans = if let Some(summary) = summary_text {
                vec![
                    Span::raw("  "),
                    Span::styled(summary, Style::default().fg(theme::TEXT_BRIGHT)),
                ]
            } else if let Some(ref title) = s.custom_title {
                vec![
                    Span::raw("  "),
                    Span::styled(title.clone(), Style::default().fg(theme::DIM)),
                ]
            } else {
                vec![
                    Span::raw("  "),
                    Span::styled("—", Style::default().fg(theme::FAINT)),
                ]
            };
            let line2 = Line::from(line2_spans);

            let has_summary = s.summary.is_some();
            let item = ListItem::new(vec![line1, line2]);
            if is_updating {
                item.style(Style::default().bg(theme::WARNING).fg(theme::TEXT_DARK))
            } else if is_selected {
                item.style(Style::default().bg(theme::FAINT))
            } else if !has_summary {
                item.style(Style::default().fg(theme::DIM))
            } else {
                item
            }
        })
        .collect();

    let item_height = 2;
    let visible_items_count = (sessions_chunk.height.saturating_sub(2) / item_height) as usize;
    let scroll_offset = if state.selected_session >= visible_items_count {
        state.selected_session - visible_items_count + 1
    } else {
        0
    };

    let visible_items: Vec<ListItem> = items
        .into_iter()
        .skip(scroll_offset)
        .take(visible_items_count)
        .collect();

    let title = Line::from(vec![
        Span::styled(
            format!(
                " Sessions ({}/{}) ",
                state.selected_session + 1,
                sessions.len()
            ),
            Style::default().fg(theme::PRIMARY),
        ),
        Span::styled(
            format!("new: {new_count}"),
            Style::default().fg(theme::SUCCESS),
        ),
        Span::raw(" "),
        Span::styled(
            format!("continued: {continued_count}"),
            Style::default().fg(theme::PRIMARY),
        ),
    ]);

    let list = List::new(visible_items).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::BORDER)).title(title));

    frame.render_widget(list, sessions_chunk);

    draw_scrollbar(
        frame,
        sessions_chunk,
        scroll_offset,
        sessions.len(),
        visible_items_count,
    );

    state.session_list_area = Some((sessions_chunk, scroll_offset, item_height as usize));

    let help_spans = vec![
        Span::styled(" ?", Style::default().fg(theme::PRIMARY)),
        Span::styled(":help ", Style::default().fg(theme::DIM)),
        Span::styled("q", Style::default().fg(theme::PRIMARY)),
        Span::styled(":quit ", Style::default().fg(theme::DIM)),
        Span::styled("←→", Style::default().fg(theme::PRIMARY)),
        Span::styled(":day ", Style::default().fg(theme::DIM)),
        Span::styled("↑↓", Style::default().fg(theme::PRIMARY)),
        Span::styled(":session ", Style::default().fg(theme::DIM)),
        Span::styled("i", Style::default().fg(theme::PRIMARY)),
        Span::styled(":info ", Style::default().fg(theme::DIM)),
        Span::styled("Enter", Style::default().fg(theme::PRIMARY)),
        Span::styled(":view ", Style::default().fg(theme::DIM)),
        Span::styled("S", Style::default().fg(theme::PRIMARY)),
        Span::styled(":summary ", Style::default().fg(theme::DIM)),
        Span::styled("b", Style::default().fg(theme::PRIMARY)),
        Span::styled(":breakdown ", Style::default().fg(theme::DIM)),
        Span::styled("/", Style::default().fg(theme::PRIMARY)),
        Span::styled(":search ", Style::default().fg(theme::DIM)),
        Span::styled("Space", Style::default().fg(theme::PRIMARY)),
        Span::styled(":pin ", Style::default().fg(theme::DIM)),
        Span::styled("m", Style::default().fg(theme::PRIMARY)),
        Span::styled(":pins", Style::default().fg(theme::DIM)),
    ];
    let help_line = Paragraph::new(Line::from(help_spans));
    frame.render_widget(help_line, help_chunk);

    if let Some((items, models_start, tools_start)) = breakdown_popup_data {
        draw_breakdown_detail_popup(frame, area, &items, models_start, tools_start, state);
    }
}

pub use crate::conversation::load_conversation;


fn draw_split_session_list(frame: &mut Frame, area: Rect, state: &mut AppState, is_active: bool) {
    use ratatui::widgets::{List, ListItem};

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let item_height: usize = 2;

    let border_color = if is_active {
        theme::PRIMARY
    } else {
        theme::BORDER
    };

    struct SessionDisplay {
        file_path: std::path::PathBuf,
        time_or_date: String,
        project_short: String,
        summary: Option<String>,
        is_recent: bool,
        is_pinned: bool,
        is_continued: bool,
    }

    let now = chrono::Utc::now();
    let proj_max_len = area.width.saturating_sub(11) as usize;
    let summary_max_len = area.width.saturating_sub(6) as usize;

    let (sessions_display, title) = match state.conv_list_mode {
        crate::ConvListMode::Pinned => {
        let pinned: Vec<SessionDisplay> = state
            .pins
            .entries()
            .iter()
            .map(|entry| {
                state.original_daily_groups.iter().find_map(|g| {
                    g.sessions
                        .iter()
                        .find(|s| s.file_path == entry.path)
                        .map(|s| SessionDisplay {
                            file_path: s.file_path.clone(),
                            time_or_date: g.date.format("%m-%d").to_string(),
                            project_short: shorten_project(&s.project_name).to_string(),
                            summary: s
                                .summary
                                .as_deref()
                                .or(s.custom_title.as_deref())
                                .map(ToString::to_string),
                            is_recent: false,
                            is_pinned: true,
                            is_continued: s.is_continued,
                        })
                })
                .unwrap_or_else(|| SessionDisplay {
                    file_path: entry.path.clone(),
                    time_or_date: "??/??".to_string(),
                    project_short: "(deleted)".to_string(),
                    summary: None,
                    is_recent: false,
                    is_pinned: true,
                    is_continued: false,
                })
            })
            .collect();
        let count = pinned.len();
        (pinned, format!(" * Pinned ({count}) "))
    }
    crate::ConvListMode::All => {
        let pins_ref = &state.pins;
        let all: Vec<SessionDisplay> = state
            .original_daily_groups
            .iter()
            .flat_map(|g| {
                g.sessions
                    .iter()
                    .filter(|s| !s.is_subagent)
                    .map(move |s| SessionDisplay {
                        file_path: s.file_path.clone(),
                        time_or_date: g.date.format("%m-%d").to_string(),
                        project_short: shorten_project(&s.project_name).to_string(),
                        summary: s
                            .summary
                            .as_deref()
                            .or(s.custom_title.as_deref())
                            .map(ToString::to_string),
                        is_recent: false,
                        is_pinned: pins_ref.is_pinned(&s.file_path),
                        is_continued: s.is_continued,
                    })
            })
            .collect();
        let count = all.len();
        (all, format!(" All ({count}) "))
    }
    crate::ConvListMode::Day => {
        if state.daily_groups.is_empty() {
            let empty = Paragraph::new("No sessions")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border_color))
                        .title(Span::styled(
                            " Sessions ",
                            Style::default().fg(theme::PRIMARY),
                        )),
                )
                .style(Style::default().fg(theme::DIM));
            frame.render_widget(empty, area);
            return;
        }
        let group = &state.daily_groups[state.selected_day];
        let sessions: Vec<SessionDisplay> = group
            .sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .map(|s| {
                let is_recent = (now - s.day_last_timestamp).num_minutes() < 5;
                SessionDisplay {
                    file_path: s.file_path.clone(),
                    time_or_date: s
                        .day_last_timestamp
                        .with_timezone(&Local)
                        .format("%H:%M")
                        .to_string(),
                    project_short: shorten_project(&s.project_name).to_string(),
                    summary: s
                        .summary
                        .as_deref()
                        .or(s.custom_title.as_deref())
                        .map(ToString::to_string),
                    is_recent,
                    is_pinned: state.pins.is_pinned(&s.file_path),
                    is_continued: s.is_continued,
                }
            })
            .collect();
        let date_str = group.date.format("%m-%d").to_string();
        let count = sessions.len();
        (sessions, format!(" {date_str} ({count}) "))
    }
    };

    let items: Vec<ListItem> = sessions_display
        .iter()
        .enumerate()
        .map(|(i, sd)| {
            let is_selected = i == state.selected_session;
            let pane_idx = state
                .panes
                .iter()
                .position(|p| p.file_path.as_deref() == Some(sd.file_path.as_path()));

            let prefix = match (is_selected, pane_idx) {
                (true, Some(idx)) => format!("▶{}", idx + 1),
                (true, None) => "▶ ".to_string(),
                (false, Some(idx)) => format!(" {}", idx + 1),
                (false, None) => "  ".to_string(),
            };


            let proj_display =
                truncate_to_display_width(&sd.project_short, proj_max_len);

            let style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else if pane_idx.is_some() {
                Style::default().fg(theme::WARM)
            } else {
                Style::default().fg(theme::DIM)
            };

            let time_style = if sd.is_recent {
                Style::default().fg(theme::ACCENT)
            } else {
                Style::default().fg(theme::PRIMARY)
            };

            let state_color = if sd.is_continued {
                theme::PRIMARY
            } else {
                theme::SUCCESS
            };

            let mut line1_spans = vec![Span::styled(prefix, style)];
            if sd.is_pinned {
                line1_spans.push(Span::styled("*", Style::default().fg(theme::WARNING)));
                line1_spans.push(Span::styled(
                    if sd.is_continued { "»" } else { "·" },
                    Style::default().fg(state_color),
                ));
            } else {
                line1_spans.push(Span::styled(
                    if sd.is_continued { " »" } else { " ·" },
                    Style::default().fg(state_color),
                ));
            }
            line1_spans.push(Span::styled(format!("{} ", sd.time_or_date), time_style));
            line1_spans.push(Span::styled(proj_display, style));
            let line1 = Line::from(line1_spans);

            let summary_text = sd
                .summary
                .as_deref()
                .unwrap_or("—");
            let summary_display =
                truncate_to_display_width(summary_text, summary_max_len);
            let line2 = Line::from(vec![
                Span::raw("   "),
                Span::styled(summary_display, Style::default().fg(theme::DIM)),
            ]);

            ListItem::new(vec![line1, line2])
        })
        .collect();

    let help_line = if is_active {
        let mode_label = match state.conv_list_mode {
            crate::ConvListMode::Day => "*",
            crate::ConvListMode::Pinned => "All",
            crate::ConvListMode::All => "Day",
        };
        if area.width >= 36 {
            let mut spans = vec![
                Span::styled(" j/k", Style::default().fg(theme::PRIMARY)),
                Span::styled(":sel ", Style::default().fg(theme::DIM)),
                Span::styled("Sp", Style::default().fg(theme::PRIMARY)),
                Span::styled(":pin ", Style::default().fg(theme::DIM)),
                Span::styled("S-Tab", Style::default().fg(theme::PRIMARY)),
                Span::styled(format!(":{mode_label}"), Style::default().fg(theme::DIM)),
            ];
            if state.conv_list_mode == crate::ConvListMode::Day {
                spans.extend_from_slice(&[
                    Span::styled(" H/L", Style::default().fg(theme::PRIMARY)),
                    Span::styled(":day", Style::default().fg(theme::DIM)),
                ]);
            }
            Line::from(spans)
        } else {
            Line::from(vec![
                Span::styled(" Sp", Style::default().fg(theme::PRIMARY)),
                Span::styled(":pin ", Style::default().fg(theme::DIM)),
                Span::styled("S-Tab", Style::default().fg(theme::PRIMARY)),
                Span::styled(format!(":{mode_label}"), Style::default().fg(theme::DIM)),
            ])
        }
    } else {
        Line::default()
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(theme::PRIMARY)))
            .title_bottom(help_line),
    );

    let mut list_state = ratatui::widgets::ListState::default()
        .with_selected(Some(state.selected_session));
    frame.render_stateful_widget(list, area, &mut list_state);
    state.session_list_area = Some((inner, list_state.offset(), item_height));
}

fn draw_conversation_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &mut ConversationPane,
    is_active: bool,
    toast_message: &Option<String>,
    _toast_time: &Option<std::time::Instant>,
    _layout_too_narrow: bool,
    selecting: bool,
    sessions: &[SessionInfo],
    all_groups: &[crate::aggregator::DailyGroup],
    pins_ref: &crate::pins::Pins,
) -> Option<Rect> {
    use ratatui::widgets::Clear;

    frame.render_widget(Clear, area);

    let has_session = pane.file_path.is_some();
    let info_height: u16 = if has_session { 3 } else { 0 };
    let info_area = if has_session {
        Some(Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: info_height,
        })
    } else {
        None
    };
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1 + info_height,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2 + info_height),
    };

    let border_color = if is_active {
        theme::PRIMARY
    } else {
        theme::BORDER
    };

    let draw_block = |frame: &mut Frame, scroll_info: &str, pane: &ConversationPane| {
        let msg_count = pane.message_lines.len();
        let msg_indicator = if msg_count > 0 {
            format!(" [{}/{}] ", pane.selected_message + 1, msg_count)
        } else {
            String::new()
        };
        let session = pane.file_path.as_ref().and_then(|fp| {
            sessions
                .iter()
                .find(|s| &s.file_path == fp)
                .or_else(|| {
                    all_groups
                        .iter()
                        .flat_map(|g| g.sessions.iter())
                        .find(|s| &s.file_path == fp)
                })
        });
        let title = if let Some(s) = session {
            let available = area.width.saturating_sub(2) as usize;
            let pin = if pins_ref.is_pinned(&s.file_path) { "*" } else { "" };
            let proj = shorten_project(&s.project_name);
            let branch = s.git_branch.as_ref().map(|b| {
                let name = b.split('/').next_back().unwrap_or(b);
                if name.chars().count() > 10 {
                    format!("#{}", name.chars().take(9).collect::<String>())
                } else {
                    format!("#{name}")
                }
            }).unwrap_or_default();
            let model = s.model.as_ref()
                .map(|m| format!(" [{}]", crate::aggregator::normalize_model_name(m)))
                .unwrap_or_default();
            let start = s.day_first_timestamp.with_timezone(&chrono::Local);
            let end = s.day_last_timestamp.with_timezone(&chrono::Local);
            let time = format!(" {}–{}", start.format("%H:%M"), end.format("%H:%M"));

            let full = format!(" {pin}{proj}{branch}{time}{model} ");
            let mid = format!(" {pin}{proj}{branch}{model} ");
            let short = format!(" {pin}{proj}{branch} ");
            let minimal = format!(" {pin}{proj} ");

            if unicode_width::UnicodeWidthStr::width(full.as_str()) <= available {
                full
            } else if unicode_width::UnicodeWidthStr::width(mid.as_str()) <= available {
                mid
            } else if unicode_width::UnicodeWidthStr::width(short.as_str()) <= available {
                short
            } else {
                minimal
            }
        } else {
            String::new()
        };
        let bottom_text = if scroll_info.is_empty() {
            msg_indicator.trim().to_string()
        } else if msg_indicator.is_empty() {
            scroll_info.to_string()
        } else {
            format!("{} | {}", msg_indicator.trim(), scroll_info)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(theme::PRIMARY)))
            .title_bottom(Line::from(Span::styled(
                bottom_text,
                Style::default().fg(theme::DIM),
            )))
            .border_style(Style::default().fg(if pane.search_mode {
                theme::WARM
            } else {
                border_color
            }));
        frame.render_widget(block, area);
    };

    if pane.messages.is_empty() {
        if pane.loading {
            let loading = Paragraph::new("Loading...");
            frame.render_widget(loading, inner);
            draw_block(frame, "", pane);
        } else if pane.file_path.is_none() {
            let hint = Paragraph::new("Select a session (Enter)")
                .style(Style::default().fg(theme::DIM))
                .alignment(ratatui::layout::Alignment::Center);
            let centered_area = Rect {
                x: inner.x,
                y: inner.y + inner.height / 2,
                width: inner.width,
                height: 1,
            };
            frame.render_widget(hint, centered_area);
            draw_block(frame, "", pane);
        } else {
            let empty = Paragraph::new("No messages found");
            frame.render_widget(empty, inner);
            draw_block(frame, "", pane);
        }
        return None;
    }

    if pane.last_width != Some(inner.width) {
        pane.rendered = None;
        pane.last_width = Some(inner.width);
    }

    let focused_msg_idx = pane
        .message_lines
        .get(pane.selected_message)
        .map(|&(_, idx)| idx);

    let needs_rerender = match &pane.rendered {
        Some((_, _, _, cached_focused)) => *cached_focused != focused_msg_idx,
        None => true,
    };

    if needs_rerender {
        let rendered = render_conversation_lines(&pane.messages, inner.width, focused_msg_idx);
        pane.message_lines = rendered.1.clone();
        pane.rendered = Some((rendered.0, rendered.1, rendered.2, focused_msg_idx));

        if !pane.search_query.is_empty() {
            update_pane_search_matches(pane);
        }

        if let Some(ref saved_ts) = pane.focused_timestamp.take()
            && let Some(msg_idx) = pane
                .messages
                .iter()
                .position(|m| m.timestamp.as_ref() == Some(saved_ts))
                && let Some(line_idx) = pane
                    .message_lines
                    .iter()
                    .position(|&(_, idx)| idx == msg_idx)
                {
                    pane.selected_message = line_idx;
                }
    }

    let cached = pane.rendered.as_ref()?;

    let search_bar_height = if pane.search_mode || !pane.search_query.is_empty() {
        1
    } else {
        0
    };
    let content_height = inner.height.saturating_sub(search_bar_height);
    let content_area = Rect {
        height: content_height,
        ..inner
    };
    let search_area = Rect {
        y: inner.y + content_height,
        height: search_bar_height,
        ..inner
    };

    let visible_height = content_area.height as usize;
    let total_lines = cached.0.len();
    let max_scroll = total_lines.saturating_sub(visible_height);
    let msg_count = pane.message_lines.len();

    if msg_count > 0 && (pane.selected_message == usize::MAX || pane.selected_message >= msg_count)
    {
        pane.selected_message = msg_count - 1;
    }

    let selected_msg_idx = pane.selected_message;
    let selected_start = pane
        .message_lines
        .get(selected_msg_idx)
        .map_or(0, |&(line, _)| line);
    let selected_end = pane
        .message_lines
        .get(selected_msg_idx + 1)
        .map_or(total_lines, |&(line, _)| line);

    if pane.scroll == usize::MAX {
        if let Some(&(last_pos, _)) = cached.1.last() {
            pane.scroll = last_pos.min(max_scroll);
        } else {
            pane.scroll = max_scroll;
        }
    } else if pane.scroll > max_scroll {
        pane.scroll = max_scroll;
    }

    if !selecting {
        let msg_in_view =
            selected_start < pane.scroll + visible_height && selected_end > pane.scroll;
        if !msg_in_view {
            if selected_end <= pane.scroll {
                pane.scroll = selected_start;
            } else if selected_start >= pane.scroll + visible_height {
                pane.scroll = selected_end.saturating_sub(visible_height);
            }
        }
    }

    pane.scroll = pane.scroll.min(max_scroll);
    let scroll = pane.scroll;

    let query_lower = pane.search_query.to_lowercase();

    let visible_lines: Vec<Line> = cached
        .0
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(line_idx, line)| {
            let is_selected = line_idx >= selected_start && line_idx < selected_end;

            if !query_lower.is_empty() && pane.search_matches.contains(&line_idx) {
                let is_current = pane.search_matches.get(pane.search_current) == Some(&line_idx);
                let bg_color = if is_current {
                    theme::SEARCH_CURRENT
                } else {
                    theme::SEARCH_MATCH
                };
                Line::from(
                    line.spans
                        .iter()
                        .map(|span| Span::styled(span.content.clone(), span.style.bg(bg_color)))
                        .collect::<Vec<_>>(),
                )
            } else if is_selected {
                let mut spans: Vec<Span> = Vec::with_capacity(line.spans.len() + 1);
                if line_idx == selected_start {
                    spans.push(Span::styled("▶ ", Style::default().fg(theme::PRIMARY)));
                } else {
                    spans.push(Span::styled("  ", Style::default()));
                }
                spans.extend(line.spans.iter().cloned());
                Line::from(spans)
            } else {
                let mut spans: Vec<Span> = Vec::with_capacity(line.spans.len() + 1);
                spans.push(Span::styled("  ", Style::default()));
                spans.extend(line.spans.iter().cloned());
                Line::from(spans)
            }
        })
        .collect();
    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, content_area);

    if search_bar_height > 0 {
        let match_info = if pane.search_matches.is_empty() {
            if pane.search_query.is_empty() {
                String::new()
            } else {
                " (no match)".to_string()
            }
        } else {
            format!(
                " ({}/{})",
                pane.search_current + 1,
                pane.search_matches.len()
            )
        };
        let search_text = format!("/{}", pane.search_query);
        let cursor = if pane.search_mode { "█" } else { "" };
        let hint = if pane.search_mode {
            "  [Enter: confirm, Esc: cancel]"
        } else {
            "  [n/N: next/prev, Esc: close]"
        };
        let search_line = Line::from(vec![
            Span::styled(&search_text, Style::default().fg(theme::WARM)),
            Span::styled(cursor, Style::default().fg(theme::WARM)),
            Span::styled(&match_info, Style::default().fg(theme::DIM)),
            Span::styled(hint, Style::default().fg(theme::DIM)),
        ]);
        frame.render_widget(Paragraph::new(search_line), search_area);
    }

    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll < max_scroll;
    let scroll_indicator = if can_scroll_up && can_scroll_down {
        "▲▼ "
    } else if can_scroll_up {
        "▲ "
    } else if can_scroll_down {
        "▼ "
    } else {
        ""
    };
    let scroll_info = format!(
        " {} {}/{} ",
        scroll_indicator,
        scroll + 1,
        max_scroll.max(1) + 1
    );
    draw_block(frame, &scroll_info, pane);

    let scrollbar_area = Rect {
        y: area.y + info_height,
        height: area.height.saturating_sub(info_height),
        ..area
    };
    draw_scrollbar(frame, scrollbar_area, scroll, total_lines, visible_height);

    if let Some(info_rect) = info_area
        && let Some(session) = pane.file_path.as_ref().and_then(|fp| {
            sessions
                .iter()
                .find(|s| &s.file_path == fp)
                .or_else(|| {
                    all_groups
                        .iter()
                        .flat_map(|g| g.sessions.iter())
                        .find(|s| &s.file_path == fp)
                })
        }) {
            let calculator = crate::aggregator::CostCalculator::global();
            let duration_mins =
                (session.day_last_timestamp - session.session_first_timestamp).num_minutes();
            let dur = if duration_mins >= 60 {
                format!("{}h{}m", duration_mins / 60, duration_mins % 60)
            } else {
                format!("{duration_mins}m")
            };
            let work_tokens = session.day_input_tokens + session.day_output_tokens;
            let cost: f64 = session
                .day_tokens_by_model
                .iter()
                .map(|(m, t)| calculator.calculate_cost(t, Some(m)).unwrap_or(0.0))
                .sum();

            let summary = session
                .summary
                .as_deref()
                .or(session.custom_title.as_deref())
                .unwrap_or("—");
            let summary_max = info_rect.width.saturating_sub(2) as usize;
            let summary_display = truncate_to_display_width(summary, summary_max);

            let line1 = Line::from(vec![
                Span::styled(
                    format!(" {summary_display}"),
                    Style::default().fg(theme::TEXT_BRIGHT),
                ),
            ]);

            let short_id: String = session.file_path.file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("-")
                .chars().take(8).collect();

            let mut line2_spans: Vec<Span> = vec![
                Span::styled(format!(" {dur}"), Style::default().fg(theme::LABEL_SUBTLE)),
                Span::styled(format!("  {}", crate::format_number(work_tokens)), Style::default().fg(theme::PRIMARY)),
                Span::styled(format!("  {}", format_cost(cost, 0)), cost_style(cost)),
                Span::styled(format!("  {short_id}"), Style::default().fg(theme::FAINT)),
            ];
            if !session.day_tool_usage.is_empty() {
                let mut tools: Vec<_> = session.day_tool_usage.iter().collect();
                tools.sort_by(|a, b| b.1.cmp(a.1));
                let prefix_width = dur.len() + crate::format_number(work_tokens).len() + format_cost(cost, 0).len() + short_id.len() + 10;
                let max_width = info_rect.width.saturating_sub(2) as usize;
                let mut used = prefix_width;
                line2_spans.push(Span::raw("  "));
                for (name, count) in tools.iter().take(6) {
                    let part = format!("{name}({count}) ");
                    if used + part.len() > max_width {
                        break;
                    }
                    used += part.len();
                    line2_spans.push(Span::styled(part, Style::default().fg(theme::DIM)));
                }
            }
            let line2 = Line::from(line2_spans);

            let sep_color = if is_active {
                theme::PRIMARY
            } else {
                theme::BORDER
            };
            let separator = Line::from(Span::styled(
                "─".repeat(info_rect.width as usize),
                Style::default().fg(sep_color),
            ));

            let info_paragraph = Paragraph::new(vec![line1, line2, separator]);
            frame.render_widget(info_paragraph, info_rect);
        }

    if is_active
        && let Some(msg) = toast_message {
            let toast_width = unicode_width::UnicodeWidthStr::width(msg.as_str()) as u16 + 4;
            let toast_area = Rect {
                x: area.x + area.width.saturating_sub(toast_width + 2),
                y: area.y + area.height.saturating_sub(3),
                width: toast_width,
                height: 1,
            };
            let toast = Paragraph::new(format!(" {msg} "))
                .style(Style::default().fg(theme::TEXT_DARK).bg(theme::SUCCESS));
            frame.render_widget(toast, toast_area);
        }

    Some(content_area)
}

fn render_conversation_lines(
    messages: &[ConversationMessage],
    width: u16,
    focused_msg_idx: Option<usize>,
) -> (Vec<Line<'static>>, Vec<(usize, usize)>, Vec<bool>) {
    use crate::text::wrap_text_with_continuation;

    const MAX_TOTAL_LINES: usize = 10000;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut wrap_flags: Vec<bool> = Vec::new();
    let mut message_positions: Vec<(usize, usize)> = Vec::new();
    let content_width = width.saturating_sub(3) as usize;
    let line_max_width = width.saturating_sub(3) as usize;
    let mut i = 0;

    macro_rules! push_line {
        ($line:expr) => {{
            lines.push($line);
            wrap_flags.push(false);
        }};
    }

    while i < messages.len() {
        if lines.len() >= MAX_TOTAL_LINES {
            push_line!(Line::from(Span::styled(
                format!("... ({} more messages)", messages.len() - i),
                Style::default().fg(theme::LABEL_SUBTLE),
            )));
            break;
        }
        let msg = &messages[i];

        if is_tool_only_message(msg) {
            let group_start = i;
            let mut tool_uses: Vec<(&str, &str)> = Vec::new();
            let mut tool_results: Vec<(&str, bool)> = Vec::new();

            while i < messages.len() && is_tool_only_message(&messages[i]) {
                for block in &messages[i].blocks {
                    match block {
                        ConversationBlock::ToolUse {
                            name,
                            input_summary,
                        } => {
                            tool_uses.push((name.as_str(), input_summary.as_str()));
                        }
                        ConversationBlock::ToolResult { content, is_error } => {
                            tool_results.push((content.as_str(), *is_error));
                        }
                        _ => {}
                    }
                }
                i += 1;
            }

            message_positions.push((lines.len(), group_start));

            let is_focused = focused_msg_idx == Some(group_start);
            if is_focused {
                push_line!(Line::from(vec![
                    Span::styled("🔧 ", Style::default()),
                    Span::styled(
                        format!("[Tools: {} calls] ", tool_uses.len()),
                        Style::default().fg(theme::PRIMARY).bold(),
                    ),
                    Span::styled("▼", Style::default().fg(theme::LABEL_SUBTLE)),
                ]));
                push_line!(Line::from(""));

                for (idx, (name, summary)) in tool_uses.iter().enumerate() {
                    let result_status = tool_results.get(idx).map_or_else(
                        || Span::raw(""),
                        |(_, is_err)| {
                            if *is_err {
                                Span::styled(" ✗", Style::default().fg(theme::ERROR))
                            } else {
                                Span::styled(" ✓", Style::default().fg(theme::SUCCESS))
                            }
                        },
                    );

                    let display = if summary.is_empty() {
                        format!("  {} {}", name, "")
                    } else {
                        let short = truncate_to_display_width(summary, 50);
                        if unicode_width::UnicodeWidthStr::width(&**summary) > 50 {
                            format!("  {name} {short}...")
                        } else {
                            format!("  {name} {short}")
                        }
                    };
                    push_line!(Line::from(truncate_spans(vec![
                        Span::styled(display, Style::default().fg(theme::PRIMARY)),
                        result_status,
                    ], line_max_width)));
                }
                push_line!(Line::from(""));
            } else {
                let mut tool_order: Vec<(&str, usize)> = Vec::new();
                for (name, _) in &tool_uses {
                    if let Some(entry) = tool_order.iter_mut().find(|(n, _)| n == name) {
                        entry.1 += 1;
                    } else {
                        tool_order.push((name, 1));
                    }
                }
                let summary: Vec<String> = tool_order
                    .iter()
                    .map(|(name, count)| {
                        if *count > 1 {
                            format!("{name}×{count}")
                        } else {
                            name.to_string()
                        }
                    })
                    .collect();

                let has_error = tool_results.iter().any(|(_, is_err)| *is_err);
                let status_icon = if has_error { "⚠" } else { "✓" };
                let status_color = if has_error {
                    theme::WARNING
                } else {
                    theme::SUCCESS
                };

                push_line!(Line::from(truncate_spans(vec![
                    Span::styled("🔧 ", Style::default()),
                    Span::styled(
                        format!("[{}] ", summary.join(", ")),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(status_icon, Style::default().fg(status_color)),
                    Span::styled(" ▶", Style::default().fg(theme::LABEL_SUBTLE)),
                ], line_max_width)));
            }

            push_line!(Line::from(Span::styled(
                "─".repeat(line_max_width),
                Style::default().fg(theme::LABEL_SUBTLE),
            )));
            continue;
        }

        message_positions.push((lines.len(), i));
        let (role_style, role_icon) = if msg.role == "user" {
            (Style::default().fg(theme::SUCCESS).bold(), "👤")
        } else {
            (Style::default().fg(theme::MUTED).bold(), "🤖")
        };

        let mut header_spans = vec![
            Span::raw(role_icon.to_string()),
            Span::raw(" "),
            Span::styled(msg.role.to_uppercase(), role_style),
        ];

        if let Some(ref ts) = msg.timestamp {
            header_spans.push(Span::styled(
                format!("  {ts}"),
                Style::default().fg(theme::LABEL_SUBTLE),
            ));
        }

        if let Some(ref model) = msg.model {
            header_spans.push(Span::styled(
                format!("  [{}]", crate::aggregator::normalize_model_name(model)),
                Style::default().fg(theme::SECONDARY),
            ));
        }

        if let Some((input, output)) = msg.tokens
            && (input > 0 || output > 0) {
                header_spans.push(Span::styled(
                    format!(
                        "  in:{} out:{}",
                        crate::format_number(input),
                        crate::format_number(output)
                    ),
                    Style::default().fg(theme::PRIMARY),
                ));
            }

        push_line!(Line::from(truncate_spans(header_spans, line_max_width)));
        push_line!(Line::from(""));

        let mut line_count: usize = 0;
        let max_lines: usize = 100;

        for block in &msg.blocks {
            if line_count >= max_lines {
                push_line!(Line::from(Span::styled(
                    "  ... (truncated)".to_string(),
                    Style::default().fg(theme::LABEL_SUBTLE),
                )));
                break;
            }

            match block {
                ConversationBlock::Text(text) => {
                    let (hl_lines, hl_flags) = render_text_with_highlighting(text, content_width);
                    for (hl_line, flag) in hl_lines.into_iter().zip(hl_flags) {
                        if line_count >= max_lines {
                            break;
                        }
                        lines.push(hl_line);
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
                ConversationBlock::Thinking(thinking) => {
                    let char_count = thinking.chars().count();
                    let header = format!("💭 Thinking ({char_count} chars)");
                    push_line!(Line::from(Span::styled(
                        header,
                        Style::default()
                            .fg(theme::THINKING)
                            .add_modifier(Modifier::ITALIC),
                    )));
                    line_count += 1;

                    let max_thinking_lines = 8;
                    let truncated: String = thinking.chars().take(500).collect();
                    let display = if char_count > 500 {
                        format!("{truncated}...")
                    } else {
                        truncated
                    };

                    let (wrapped, wflags) = wrap_text_with_continuation(&display, content_width);
                    for (wrapped_line, flag) in wrapped.into_iter().zip(wflags).take(max_thinking_lines) {
                        if line_count >= max_lines {
                            break;
                        }
                        lines.push(Line::from(Span::styled(
                            format!("  {wrapped_line}"),
                            Style::default().fg(theme::DIM),
                        )));
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
                ConversationBlock::ToolUse {
                    name,
                    input_summary,
                } => {
                    let tool_line = if input_summary.is_empty() {
                        format!("  [Tool] {name}")
                    } else {
                        format!("  [Tool] {name}: {input_summary}")
                    };
                    let (wrapped, wflags) = wrap_text_with_continuation(&tool_line, content_width + 2);
                    for (wrapped_line, flag) in wrapped.into_iter().zip(wflags).take(2) {
                        lines.push(Line::from(Span::styled(
                            wrapped_line,
                            Style::default().fg(theme::PRIMARY),
                        )));
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
                ConversationBlock::ToolResult { content, is_error } => {
                    let result_style = if *is_error {
                        Style::default().fg(theme::ERROR)
                    } else {
                        Style::default().fg(theme::SUCCESS)
                    };
                    let prefix = if *is_error { "  [Error]" } else { "  [Result]" };
                    push_line!(Line::from(Span::styled(prefix.to_string(), result_style)));
                    line_count += 1;

                    let (result_lines, result_flags) = render_tool_result_with_highlighting(content, content_width);
                    for (rl, flag) in result_lines.into_iter().zip(result_flags) {
                        if line_count >= max_lines {
                            break;
                        }
                        lines.push(rl);
                        wrap_flags.push(flag);
                        line_count += 1;
                    }
                }
            }
        }

        push_line!(Line::from(Span::styled(
            "─".repeat(width.saturating_sub(2) as usize),
            Style::default().fg(theme::LABEL_SUBTLE),
        )));
        i += 1;
    }

    (lines, message_positions, wrap_flags)
}


fn draw_summary(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use ratatui::widgets::{Clear, Wrap};

    frame.render_widget(Clear, area);

    let target_info = match &state.summary_type {
        Some(SummaryType::Session(session)) => {
            let time = session
                .day_first_timestamp
                .with_timezone(&Local)
                .format("%H:%M");
            format!("{} @ {}", session.project_name, time)
        }
        Some(SummaryType::Day(day)) => day.date.format("%Y-%m-%d").to_string(),
        None => String::new(),
    };

    let title = if state.generating_summary {
        format!(" Generating: {target_info} ")
    } else if target_info.is_empty() {
        " Summary [q:close j/k:scroll r:regenerate] ".to_string()
    } else {
        format!(" {target_info} [q:close j/k:scroll r:regenerate] ")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().fg(theme::PRIMARY)))
        .border_style(Style::default().fg(theme::PRIMARY));

    let inner = block.inner(area);
    state.summary_popup_area = Some(area);
    frame.render_widget(block, area);

    if state.generating_summary {
        let f = state.animation_frame;
        let slow = f / 2;

        let star_chars = ['·', '✢', '✳', '✶', '✻', '✽'];

        let messages = [
            "Analyzing conversation",
            "Summarizing insights",
            "Distilling patterns",
            "Synthesizing context",
            "Processing thoughts",
        ];
        let msg_idx = (slow / 30) % messages.len();
        let msg = messages[msg_idx];

        let make_starfield = |width: usize, offset: usize| -> Vec<Span> {
            (0..width)
                .map(|i| {
                    let seed = (i * 17 + offset) % 97;
                    let twinkle = (slow + seed) % 48;
                    let (ch, intensity) = if seed.is_multiple_of(7) {
                        let char_idx = (twinkle / 8) % star_chars.len();
                        match twinkle {
                            0..=8 => (star_chars[char_idx], 1.0),
                            9..=20 => (star_chars[(char_idx + 1) % star_chars.len()], 0.7),
                            21..=32 => ('·', 0.4),
                            _ => (' ', 0.0),
                        }
                    } else {
                        (' ', 0.0)
                    };
                    let color = theme::primary_with_intensity(intensity);
                    Span::styled(ch.to_string(), Style::default().fg(color))
                })
                .collect()
        };

        let spinner_idx = (slow / 4) % star_chars.len();
        let spinner: Vec<Span> = (0..6)
            .map(|i| {
                let frame_idx = (spinner_idx + 6 - i) % star_chars.len();
                let intensity = 1.0 - (i as f32 * 0.15);
                let color = theme::primary_with_intensity(intensity as f64);
                Span::styled(
                    format!(" {}", star_chars[frame_idx]),
                    Style::default().fg(color),
                )
            })
            .collect();

        let wave_msg: Vec<Span> = msg
            .chars()
            .enumerate()
            .map(|(i, c)| {
                let wave = ((slow as f32 * 0.12 + i as f32 * 0.25).sin() * 0.25 + 0.75) as f64;
                let color = theme::primary_with_intensity(wave);
                Span::styled(c.to_string(), Style::default().fg(color))
            })
            .collect();

        let dots_phase = (slow / 6) % 4;
        let dots_spans: Vec<Span> = (0..3)
            .map(|i| {
                let visible = i < dots_phase;
                let intensity = if visible { 0.6 } else { 0.15 };
                let color = theme::primary_with_intensity(intensity);
                Span::styled(".", Style::default().fg(color))
            })
            .collect();

        let width = inner.width as usize;
        let center_y = inner.height / 2;

        let lines: Vec<Line> = vec![
            Line::from(make_starfield(width, 0)),
            Line::from(make_starfield(width, 33)),
            Line::from(vec![]),
            Line::from(spinner).alignment(ratatui::layout::Alignment::Center),
            Line::from(vec![]),
            {
                let mut msg_line = wave_msg;
                msg_line.extend(dots_spans);
                Line::from(msg_line).alignment(ratatui::layout::Alignment::Center)
            },
            Line::from(vec![]),
            Line::from(make_starfield(width, 66)),
            Line::from(make_starfield(width, 99)),
        ];

        let total_lines = lines.len() as u16;
        let start_y = center_y.saturating_sub(total_lines / 2);

        let text_area = Rect {
            x: inner.x,
            y: inner.y.saturating_add(start_y),
            width: inner.width,
            height: inner.height.saturating_sub(start_y),
        };

        let loading = Paragraph::new(lines);
        frame.render_widget(loading, text_area);
        return;
    }

    let content = &state.summary_content;

    let padded_inner = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(1),
        height: inner.height,
    };

    let paragraph = Paragraph::new(content.as_str()).wrap(Wrap { trim: false });
    let total_lines = paragraph.line_count(padded_inner.width);
    let max_scroll = total_lines.saturating_sub(padded_inner.height as usize);
    state.summary_scroll = state.summary_scroll.min(max_scroll);

    let paragraph = paragraph.scroll((state.summary_scroll as u16, 0));
    frame.render_widget(paragraph, padded_inner);
}

fn draw_session_detail(
    frame: &mut Frame,
    area: Rect,
    session: &crate::aggregator::SessionInfo,
    footer: &str,
    is_pinned: bool,
) {
    use ratatui::widgets::Clear;

    let popup_width = 70u16.min(area.width.saturating_sub(4));
    let popup_height = 24u16.min(area.height.saturating_sub(4));
    let popup_area = Rect {
        x: (area.width.saturating_sub(popup_width)) / 2,
        y: (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let session_start = session
        .session_first_timestamp
        .with_timezone(&chrono::Local);
    let end = session.day_last_timestamp.with_timezone(&chrono::Local);
    let duration_mins =
        (session.day_last_timestamp - session.session_first_timestamp).num_minutes();
    let duration_str = if duration_mins >= 60 {
        format!("{}h{}m", duration_mins / 60, duration_mins % 60)
    } else {
        format!("{duration_mins}m")
    };

    let cache_write: u64 = session
        .day_tokens_by_model
        .values()
        .map(|t| t.cache_creation_tokens)
        .sum();
    let cache_read: u64 = session
        .day_tokens_by_model
        .values()
        .map(|t| t.cache_read_tokens)
        .sum();
    let work_tokens = session.day_input_tokens + session.day_output_tokens;
    let total_tokens = work_tokens + cache_write + cache_read;

    let calculator = crate::aggregator::CostCalculator::global();
    let cost: f64 = session
        .day_tokens_by_model
        .iter()
        .map(|(m, t)| calculator.calculate_cost(t, Some(m)).unwrap_or(0.0))
        .sum();

    let model_name = session
        .model
        .as_ref()
        .map_or_else(|| "?".to_string(), |m| crate::aggregator::normalize_model_name(m));
    let model_clr = session
        .model
        .as_ref()
        .map_or(theme::LABEL_MUTED, |m| model_color(m));

    let label_style = Style::default().fg(theme::DIM);

    let mut lines: Vec<Line> = Vec::new();

    // Header: project#branch [Model]  tokens $cost
    let mut header = vec![Span::raw("  ")];
    if is_pinned {
        header.push(Span::styled("* ", Style::default().fg(theme::WARNING)));
    }
    let marker = if session.is_continued { "» " } else { "" };
    if !marker.is_empty() {
        header.push(Span::styled(marker, Style::default().fg(theme::PRIMARY)));
    }
    header.push(Span::styled(
        shorten_project(&session.project_name),
        Style::default().fg(theme::WARM).bold(),
    ));
    if let Some(ref branch) = session.git_branch {
        let short = branch.split('/').next_back().unwrap_or(branch);
        header.push(Span::styled(
            format!("#{short}"),
            Style::default().fg(theme::BRANCH),
        ));
    }
    header.push(Span::styled(
        format!("  [{model_name}]"),
        Style::default().fg(model_clr),
    ));
    header.push(Span::styled(
        format!("  {}", crate::format_number(work_tokens)),
        Style::default().fg(theme::PRIMARY),
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(header));

    // Summary
    let summary_text = session
        .summary
        .as_deref()
        .or(session.custom_title.as_deref())
        .unwrap_or("—");
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(summary_text, Style::default().fg(theme::TEXT_BRIGHT)),
    ]));
    lines.push(Line::from(""));

    // Time
    let time_display = if session_start.date_naive() == end.date_naive() {
        format!(
            "{}–{}  {}",
            session_start.format("%Y-%m-%d %H:%M"),
            end.format("%H:%M"),
            duration_str,
        )
    } else {
        format!(
            "{}–{}  {}",
            session_start.format("%Y-%m-%d %H:%M"),
            end.format("%Y-%m-%d %H:%M"),
            duration_str,
        )
    };
    lines.push(Line::from(vec![
        Span::styled("  Time      ", label_style),
        Span::styled(time_display, Style::default().fg(theme::LABEL_SUBTLE)),
    ]));

    // Tokens detail
    lines.push(Line::from(vec![
        Span::styled("  Tokens    ", label_style),
        Span::styled(
            format!("total:{}", crate::format_number(total_tokens)),
            Style::default().fg(theme::PRIMARY),
        ),
        Span::styled(
            format!(
                "  in:{} out:{} cw:{} cr:{}",
                crate::format_number(session.day_input_tokens),
                crate::format_number(session.day_output_tokens),
                crate::format_number(cache_write),
                crate::format_number(cache_read),
            ),
            label_style,
        ),
    ]));

    // Cost detail
    lines.push(Line::from(vec![
        Span::styled("  Cost      ", label_style),
        Span::styled(format_cost(cost, 2), cost_style(cost)),
    ]));

    lines.push(Line::from(""));

    // Model breakdown
    if session.day_tokens_by_model.len() > 1 {
        lines.push(Line::from(Span::styled(
            "  Models",
            Style::default().fg(theme::PRIMARY).bold(),
        )));
        let mut models: Vec<_> = session.day_tokens_by_model.iter().collect();
        models.sort_by(|a, b| b.1.work_tokens().cmp(&a.1.work_tokens()));
        for (model, tokens) in &models {
            let normalized = crate::aggregator::normalize_model_name(model);
            let clr = model_color(model);
            let model_cost = calculator.calculate_cost(tokens, Some(model)).unwrap_or(0.0);
            lines.push(Line::from(vec![
                Span::styled(format!("    {normalized:<16}"), Style::default().fg(clr)),
                Span::styled(
                    format!("{:>6}", crate::format_number(tokens.work_tokens())),
                    Style::default().fg(theme::PRIMARY),
                ),
                Span::styled(
                    format!("  {}", format_cost(model_cost, 0)),
                    cost_style(model_cost),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Tools
    if !session.day_tool_usage.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Tools",
            Style::default().fg(theme::PRIMARY).bold(),
        )));
        let mut tools: Vec<_> = session.day_tool_usage.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1));
        let tool_strs: Vec<String> = tools
            .iter()
            .take(8)
            .map(|(name, count)| format!("{name}({count})"))
            .collect();
        let inner_width = popup_width.saturating_sub(6) as usize;
        let mut current_line = String::from("    ");
        for (i, tool_str) in tool_strs.iter().enumerate() {
            let sep = if i > 0 { "  " } else { "" };
            if current_line.len() + sep.len() + tool_str.len() > inner_width {
                lines.push(Line::from(Span::styled(
                    current_line.clone(),
                    label_style,
                )));
                current_line = format!("    {tool_str}");
            } else {
                current_line = format!("{current_line}{sep}{tool_str}");
            }
        }
        if !current_line.trim().is_empty() {
            lines.push(Line::from(Span::styled(current_line, label_style)));
        }
        lines.push(Line::from(""));
    }

    // Directory
    lines.push(Line::from(vec![
        Span::styled("  Directory ", label_style),
        Span::styled(&session.project_name, Style::default().fg(theme::LABEL_SUBTLE)),
    ]));

    // Session ID + resume command
    let session_id = session
        .file_path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("-");
    lines.push(Line::from(vec![
        Span::styled("  ID        ", label_style),
        Span::styled(session_id, Style::default().fg(theme::LABEL_SUBTLE)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Resume    ", label_style),
        Span::styled(format!("claude -r {session_id}"), Style::default().fg(theme::ACCENT)),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PRIMARY))
        .title(Span::styled(
            " Session Detail ",
            Style::default().fg(theme::PRIMARY).bold(),
        ))
        .title_bottom(Line::from(Span::styled(
            footer,
            Style::default().fg(theme::DIM),
        )));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup_area);
}

fn draw_detail_popup(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(group) = state.daily_groups.get(state.selected_day) else {
        return;
    };
    let sessions: Vec<_> = group.sessions.iter().filter(|s| !s.is_subagent).collect();
    let Some(session) = sessions.get(state.selected_session) else {
        return;
    };
    let pinned = state.pins.is_pinned(&session.file_path);
    draw_session_detail(
        frame,
        area,
        session,
        " Space:pin  s:summary  r:regen  Esc:close ",
        pinned,
    );
}



fn draw_breakdown_detail_popup(
    frame: &mut Frame,
    area: Rect,
    items: &[BreakdownItem],
    models_start_idx: usize,
    tools_start_idx: usize,
    state: &mut AppState,
) {
    use ratatui::widgets::Clear;

    let popup_width = 80.min(area.width.saturating_sub(4));
    let item_count = items.len() as u16;
    let popup_height = (item_count + 4).min(area.height.saturating_sub(4)).min(30);

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let (visible_height, max_scroll, scroll) =
        calc_scroll(popup_height, items.len(), state.daily_breakdown_scroll, 3);
    state.daily_breakdown_max_scroll = max_scroll;

    let mut lines: Vec<Line> = vec![];
    for (i, item) in items.iter().enumerate().skip(scroll).take(visible_height) {
        let (label, bar_color, name, info, pct) = match item {
            BreakdownItem::Project(name, tokens, pct) => {
                let label =
                    if i == 0 || (i > 0 && !matches!(&items[i - 1], BreakdownItem::Project(..))) {
                        "Projects  "
                    } else {
                        "          "
                    };
                (
                    label,
                    theme::WARM,
                    name.clone(),
                    crate::format_number(*tokens),
                    *pct,
                )
            }
            BreakdownItem::Model(name, tokens, pct) => {
                let label = if i == models_start_idx {
                    "Models    "
                } else {
                    "          "
                };
                let short = crate::aggregator::normalize_model_name(name);
                (
                    label,
                    theme::PRIMARY,
                    short,
                    crate::format_number(*tokens),
                    *pct,
                )
            }
            BreakdownItem::Tool(name, count, pct) => {
                let label = if i == tools_start_idx {
                    "Tools     "
                } else {
                    "          "
                };
                (label, theme::SUCCESS, name.clone(), count.to_string(), *pct)
            }
        };

        let bar_len = (pct / 100.0 * 8.0).round().min(8.0) as usize;
        let bar = format!(
            "{}{}",
            "█".repeat(bar_len.max(1)),
            "░".repeat(8 - bar_len.max(1))
        );

        let display_text = format!(" {name} ({info}) {pct:.0}%");

        lines.push(Line::from(vec![
            Span::styled(format!(" {label}"), Style::default().fg(theme::DIM)),
            Span::styled(bar, Style::default().fg(bar_color)),
            Span::styled(display_text, Style::default().fg(theme::TEXT_BRIGHT)),
        ]));
    }

    let total_items = items.len();
    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll + visible_height < total_items;
    let scroll_indicator = match (can_scroll_up, can_scroll_down) {
        (true, true) => " ▲▼ ",
        (true, false) => " ▲ ",
        (false, true) => " ▼ ",
        (false, false) => "",
    };

    let popup = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::PRIMARY))
            .title(Span::styled(
                format!(" Breakdown ({total_items}) "),
                Style::default()
                    .fg(theme::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(" j/k: scroll  b/Esc: close ", Style::default().fg(theme::DIM)),
                Span::styled(scroll_indicator, Style::default().fg(theme::WARNING)),
            ])),
    );

    frame.render_widget(popup, popup_area);
}

fn draw_filter_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let total_items = crate::PeriodFilter::ALL_VARIANTS.len() + 1;
    let extra_lines: u16 = if state.filter_input_mode { 2 } else { 0 };
    let popup_width = 44.min(area.width.saturating_sub(4));
    let popup_height = (total_items as u16 + 4 + extra_lines).min(area.height.saturating_sub(4));

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.filter_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let mut lines: Vec<Line> = Vec::new();
    for (i, variant) in crate::PeriodFilter::ALL_VARIANTS.iter().enumerate() {
        let is_selected = i == state.filter_popup_selected && !state.filter_input_mode;
        let is_current = *variant == state.period_filter;

        let marker = if is_current { "●" } else { " " };
        let range_label = variant.date_range_label();
        let text = if range_label.is_empty() {
            format!(" {} {}", marker, variant.label())
        } else {
            format!(" {} {} {}", marker, variant.label(), range_label)
        };

        let style = if is_selected {
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .bg(Color::Rgb(40, 50, 60))
                .add_modifier(Modifier::BOLD)
        } else if is_current {
            Style::default().fg(theme::PRIMARY)
        } else {
            Style::default().fg(theme::SECONDARY)
        };

        lines.push(Line::from(Span::styled(text, style)));
    }

    let custom_idx = crate::PeriodFilter::ALL_VARIANTS.len();
    let is_custom_selected = state.filter_popup_selected == custom_idx && !state.filter_input_mode;
    let is_custom_current = matches!(state.period_filter, crate::PeriodFilter::Custom(_, _));
    let custom_marker = if is_custom_current { "●" } else { " " };
    let custom_label = if is_custom_current {
        format!(
            " {} Custom {}",
            custom_marker,
            state.period_filter.date_range_label()
        )
    } else {
        format!(" {custom_marker} Custom...")
    };
    let custom_style = if is_custom_selected {
        Style::default()
            .fg(theme::TEXT_BRIGHT)
            .bg(Color::Rgb(40, 50, 60))
            .add_modifier(Modifier::BOLD)
    } else if is_custom_current {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::SECONDARY)
    };
    lines.push(Line::from(Span::styled(custom_label, custom_style)));

    if state.filter_input_mode {
        lines.push(Line::from(""));
        let input_color = if state.filter_input_error {
            theme::ERROR
        } else {
            theme::TEXT_BRIGHT
        };
        let byte_cursor = state.filter_input.char_indices()
            .nth(state.filter_input_cursor)
            .map_or(state.filter_input.len(), |(i, _)| i);
        let (before, after) = state.filter_input.split_at(byte_cursor);
        let cursor_char_len = after.chars().next().map_or(0, char::len_utf8);
        let cursor_char = if after.is_empty() { "_" } else { &after[..cursor_char_len] };
        let rest = if after.is_empty() { "" } else { &after[cursor_char_len..] };
        lines.push(Line::from(vec![
            Span::styled("   > ", Style::default().fg(theme::DIM)),
            Span::styled(before.to_string(), Style::default().fg(input_color)),
            Span::styled(
                cursor_char.to_string(),
                Style::default().fg(theme::TEXT_BRIGHT).bg(theme::PRIMARY),
            ),
            Span::styled(rest.to_string(), Style::default().fg(input_color)),
        ]));
    }

    let footer = if state.filter_input_mode {
        " YYYY-MM-DD · YYYY-MM · YYYY-MM-DD..YYYY-MM-DD "
    } else {
        " j/k  Enter  Esc "
    };

    let popup = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::PRIMARY))
            .title(Span::styled(
                " Filter Period ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(footer).style(Style::default().fg(theme::DIM))),
    );

    frame.render_widget(popup, popup_area);
}

fn draw_project_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let total = state.project_list.len() + 1;
    let max_visible: usize = 20;
    let visible = total.min(max_visible);
    let popup_width = 60u16.min(area.width.saturating_sub(4));
    let popup_height = (visible as u16 + 3)
        .min(area.height.saturating_sub(4))
        .min(23);

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    state.project_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let inner_height = popup_height.saturating_sub(3) as usize;
    let sel = state.project_popup_selected;
    let mut scroll_val = state.project_popup_scroll;
    if sel < scroll_val {
        scroll_val = sel;
    } else if inner_height > 0 && sel >= scroll_val + inner_height {
        scroll_val = sel + 1 - inner_height;
    }
    state.project_popup_scroll = scroll_val;

    let mut lines: Vec<Line> = Vec::new();
    for i in scroll_val..(scroll_val + inner_height).min(total) {
        let is_selected = i == sel;
        if i == 0 {
            let is_current = state.project_filter.is_none();
            let marker = if is_current { "\u{25cf}" } else { " " };
            let text = format!(" {marker} All");
            let style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .bg(Color::Rgb(40, 50, 60))
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(theme::PRIMARY)
            } else {
                Style::default().fg(theme::SECONDARY)
            };
            lines.push(Line::from(Span::styled(text, style)));
        } else if let Some((name, tokens, last_date)) = state.project_list.get(i - 1) {
            let is_current = state.project_filter.as_ref() == Some(name);
            let marker = if is_current { "\u{25cf}" } else { " " };
            let short = name.rsplit('/').next().unwrap_or(name);
            let token_str = format_token_short(*tokens);
            let date_str = last_date.format("%Y-%m-%d").to_string();
            let suffix = format!("{token_str}  {date_str}");
            let inner_width = popup_width.saturating_sub(2) as usize;
            let prefix_len = 3 + 2; // " X " marker + "  " separator before suffix
            let max_name_len = inner_width.saturating_sub(prefix_len + suffix.len());
            let short_width = unicode_width::UnicodeWidthStr::width(short);
            let display_name: String = if short_width > max_name_len {
                truncate_to_display_width(short, max_name_len)
            } else {
                short.to_string()
            };
            let display_name_width = unicode_width::UnicodeWidthStr::width(display_name.as_str());
            let pad = max_name_len.saturating_sub(display_name_width);
            let text = format!(
                " {} {}{:pad$}  {}",
                marker,
                display_name,
                "",
                suffix,
                pad = pad
            );
            let style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .bg(Color::Rgb(40, 50, 60))
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(theme::PRIMARY)
            } else {
                Style::default().fg(theme::SECONDARY)
            };
            lines.push(Line::from(Span::styled(text, style)));
        }
    }

    let footer = " j/k  Enter  Esc ";

    let popup = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::PRIMARY))
            .title(Span::styled(
                " Filter Project ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(footer).style(Style::default().fg(theme::DIM))),
    );

    frame.render_widget(popup, popup_area);
}

fn format_token_short(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.0}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

fn draw_help_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use ratatui::widgets::Clear;

    let popup_width = 76.min(area.width.saturating_sub(4));
    let popup_height = area.height.saturating_sub(4).min(44);

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let mut content = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Global",
            Style::default().fg(theme::PRIMARY).bold(),
        )),
        Line::from("  Tab / 1,2,3   Switch tabs (Dashboard/Daily/Insights)"),
        Line::from("  /             Search sessions"),
        Line::from("  f             Open period filter"),
        Line::from("  p             Open project filter"),
        Line::from("  ?             Show this help"),
        Line::from("  q             Quit application"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Dashboard ", Style::default().fg(theme::WARM).bold()),
            Span::styled("(Tab 1)", Style::default().fg(theme::DIM)),
        ]),
        Line::from("  ←/→ h/l       Switch panels"),
        Line::from("  ↑/↓ j/k       Scroll panel content"),
        Line::from("  Enter         Expand panel detail"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Daily ", Style::default().fg(theme::WARM).bold()),
            Span::styled("(Tab 2)", Style::default().fg(theme::DIM)),
        ]),
        Line::from("  ←/→ h/l       Navigate days"),
        Line::from("  ↑/↓ j/k       Select session (or scroll breakdown)"),
        Line::from("  b             Toggle breakdown focus"),
        Line::from("  t             Jump to today"),
        Line::from("  Enter         Open conversation"),
        Line::from("  s / S         Session / Day summary (AI)"),
        Line::from("  R             Regenerate & write summary to JSONL"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Conversation ", Style::default().fg(theme::WARM).bold()),
            Span::styled("(from Daily)", Style::default().fg(theme::DIM)),
        ]),
        Line::from("  j/k           Select message (up/down)"),
        Line::from("  d/u           Scroll page (20 lines)"),
        Line::from("  (auto-expand when focused)"),
        Line::from("  y             Copy message to clipboard"),
        Line::from("  /             Search in conversation"),
        Line::from("  n/N           Next / Previous search match"),
        Line::from("  g/G           Top / Bottom"),
        Line::from("  q/Esc         Close (or exit search)"),
        Line::from(""),
        Line::from(Span::styled(
            "  CLI Options ",
            Style::default().fg(theme::WARM).bold(),
        )),
    ];
    let inner_w = popup_width.saturating_sub(4) as usize;
    let flag_col = 16;
    for (flag, help) in crate::cli_help_lines() {
        let line = format!("  {flag:<flag_col$} {help}");
        let display: String = line.chars().take(inner_w).collect();
        content.push(Line::from(display));
    }
    content.extend_from_slice(&[
        Line::from(""),
        Line::from(Span::styled(
            "  Note ",
            Style::default().fg(theme::PRIMARY).bold(),
        )),
        Line::from("  Tokens = input + output (excludes cache)"),
        Line::from("  Costs  = estimated from API pricing"),
        Line::from("  Cache  = ~/.cache/ccsight/cache.json"),
        Line::from("  Pins   = ~/.config/ccsight/pins.json"),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PRIMARY))
        .title(Span::styled(" Help [j/k: scroll] ", Style::default().fg(theme::PRIMARY)));
    let inner = block.inner(popup_area);
    let total_lines = content.len() as u16;
    let max_scroll = total_lines.saturating_sub(inner.height);
    state.help_scroll = state.help_scroll.min(max_scroll);

    let popup = Paragraph::new(content)
        .block(block)
        .scroll((state.help_scroll, 0));

    frame.render_widget(popup, popup_area);

    draw_scrollbar(
        frame,
        popup_area,
        state.help_scroll as usize,
        total_lines as usize,
        inner.height as usize,
    );
}

fn draw_search_popup(frame: &mut Frame, area: Rect, state: &mut crate::AppState) {
    use ratatui::widgets::Clear;

    let popup_width = (area.width as f32 * 0.8) as u16;
    let popup_height = 24.min(area.height.saturating_sub(4));

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: 3,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let inner = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(popup_area);
    state.search_results_area = Some(inner[1]);

    let title = if state.searching {
        " Search (Esc: cancel, Enter: select) [Searching...] "
    } else {
        " Search (Esc: cancel, Enter: select) "
    };

    let input_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PRIMARY))
        .title(Span::styled(title, Style::default().fg(theme::PRIMARY)));

    let input_text = format!("/{}", state.search_query);
    let input = Paragraph::new(input_text)
        .style(Style::default().fg(theme::TEXT_BRIGHT))
        .block(input_block);
    frame.render_widget(input, inner[0]);

    let results_block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_style(Style::default().fg(theme::PRIMARY));

    if state.search_results.is_empty() {
        let no_results = if state.search_query.is_empty() {
            "Type to search projects, summaries, branches, dates, content..."
        } else if state.searching {
            "Searching content..."
        } else {
            "No results found"
        };
        let text = Paragraph::new(no_results)
            .style(Style::default().fg(theme::LABEL_SUBTLE))
            .block(results_block);
        frame.render_widget(text, inner[1]);
    } else {
        let item_height = 2usize;
        let visible_items = inner[1].height.saturating_sub(2) as usize / item_height;
        let start = if state.search_selected >= visible_items {
            state.search_selected - visible_items + 1
        } else {
            0
        };

        let inner_w = inner[1].width.saturating_sub(2) as usize;
        let items: Vec<ListItem> = state
            .search_results
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_items)
            .map(|(i, result)| {
                let group = &state.daily_groups[result.day_idx];
                let session = group.sessions.iter().filter(|s| !s.is_subagent).nth(result.session_idx).unwrap_or_else(|| &group.sessions[0]);
                let date_str = group.date.format("%m-%d").to_string();
                let project = shorten_project(&session.project_name);
                let branch = session.git_branch.as_ref()
                    .map(|b| format!("#{}", b.split('/').next_back().unwrap_or(b)))
                    .unwrap_or_default();

                let match_indicator = match result.match_type {
                    search::SearchMatchType::ProjectName => "[proj]",
                    search::SearchMatchType::Summary => "[sum]",
                    search::SearchMatchType::GitBranch => "[git]",
                    search::SearchMatchType::SessionId => "[id]",
                    search::SearchMatchType::Date => "[date]",
                    search::SearchMatchType::Content => "[msg]",
                };

                let summary = session.summary.as_deref()
                    .or(session.custom_title.as_deref())
                    .unwrap_or("");
                let snippet_text = result.snippet.as_deref().unwrap_or(summary);
                let snippet = truncate_to_display_width(snippet_text, inner_w.saturating_sub(8));

                let selected = i == state.search_selected;
                let sel_style = Style::default().bg(theme::FAINT).fg(theme::TEXT_BRIGHT);

                let line1 = Line::from(vec![
                    Span::styled(
                        format!("{date_str} "),
                        if selected { sel_style } else { Style::default().fg(theme::PRIMARY) },
                    ),
                    Span::styled(
                        format!("{project}{branch} "),
                        if selected { sel_style } else { Style::default().fg(theme::TEXT_BRIGHT) },
                    ),
                    Span::styled(
                        match_indicator,
                        if selected { sel_style } else { Style::default().fg(theme::DIM) },
                    ),
                ]);
                let line2 = Line::from(vec![
                    Span::styled(
                        format!("  {snippet}"),
                        if selected { sel_style } else { Style::default().fg(theme::LABEL_SUBTLE) },
                    ),
                ]);
                ListItem::new(vec![line1, line2])
            })
            .collect();

        let list = List::new(items).block(results_block);
        frame.render_widget(list, inner[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_scroll_basic() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 0, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 12);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_calc_scroll_with_scroll_offset() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 5, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 12);
        assert_eq!(scroll, 5);
    }

    #[test]
    fn test_calc_scroll_clamps_to_max() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 100, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 12);
        assert_eq!(scroll, 12);
    }

    #[test]
    fn test_calc_scroll_items_fit_in_view() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 5, 0, 2);
        assert_eq!(visible, 8);
        assert_eq!(max_scroll, 0);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_calc_scroll_different_header() {
        let (visible, max_scroll, scroll) = calc_scroll(10, 20, 0, 4);
        assert_eq!(visible, 6);
        assert_eq!(max_scroll, 14);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_calc_scroll_zero_height() {
        let (visible, max_scroll, scroll) = calc_scroll(0, 10, 0, 2);
        assert_eq!(visible, 0);
        assert_eq!(max_scroll, 10);
        assert_eq!(scroll, 0);
    }

    #[test]
    fn test_shorten_model_name_opus() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-5-20251101"),
            "Opus 4.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-1-20250805"),
            "Opus 4.1"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-20250514"),
            "Opus 4"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-opus-20240229"),
            "Opus 3"
        );
    }

    #[test]
    fn test_shorten_model_name_sonnet() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-sonnet-4-5-20250929"),
            "Sonnet 4.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-sonnet-4-20250514"),
            "Sonnet 4"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-5-sonnet-20241022"),
            "Sonnet 3.5"
        );
    }

    #[test]
    fn test_shorten_model_name_haiku() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-haiku-4-5-20251001"),
            "Haiku 4.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-5-haiku-20241022"),
            "Haiku 3.5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-3-haiku-20240307"),
            "Haiku 3"
        );
    }

    #[test]
    fn test_shorten_model_name_fallback() {
        assert_eq!(crate::aggregator::normalize_model_name("unknown"), "Other");
        assert_eq!(
            crate::aggregator::normalize_model_name("some-other-model"),
            "Other"
        );
    }

    #[test]
    fn test_shorten_model_name_new_versions() {
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-opus-4-6-20260101"),
            "Opus 4.6"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-sonnet-5-20260101"),
            "Sonnet 5"
        );
        assert_eq!(
            crate::aggregator::normalize_model_name("claude-haiku-5-1-20260101"),
            "Haiku 5.1"
        );
    }

    // message judgment function tests
    #[test]
    fn test_is_tool_only_message_true() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/path".to_string(),
                },
                ConversationBlock::ToolResult {
                    content: "content".to_string(),
                    is_error: false,
                },
            ],
            timestamp: None,
            model: None,
            tokens: None,
        };
        assert!(is_tool_only_message(&msg));
    }

    #[test]
    fn test_is_tool_only_message_false() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![
                ConversationBlock::Text("Hello".to_string()),
                ConversationBlock::ToolUse {
                    name: "Read".to_string(),
                    input_summary: "/path".to_string(),
                },
            ],
            timestamp: None,
            model: None,
            tokens: None,
        };
        assert!(!is_tool_only_message(&msg));
    }

    #[test]
    fn test_is_thinking_only_message_true() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Thinking("thinking...".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
        };
        assert!(is_thinking_only_message(&msg));
    }

    #[test]
    fn test_extract_message_text_from_text_block() {
        let msg = ConversationMessage {
            role: "assistant".to_string(),
            blocks: vec![ConversationBlock::Text("Hello world".to_string())],
            timestamp: None,
            model: None,
            tokens: None,
        };
        assert_eq!(extract_message_text(&msg), "Hello world");
    }

    #[test]
    fn test_extract_message_text_from_tool_result() {
        let msg = ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::ToolResult {
                content: "result content".to_string(),
                is_error: false,
            }],
            timestamp: None,
            model: None,
            tokens: None,
        };
        assert_eq!(extract_message_text(&msg), "result content");
    }

    #[test]
    fn test_extract_message_text_error_result() {
        let msg = ConversationMessage {
            role: "user".to_string(),
            blocks: vec![ConversationBlock::ToolResult {
                content: "error message".to_string(),
                is_error: true,
            }],
            timestamp: None,
            model: None,
            tokens: None,
        };
        assert_eq!(extract_message_text(&msg), "[Error] error message");
    }

    // theme function tests
    #[test]
    fn test_model_color_opus() {
        assert_eq!(model_color("claude-opus-4-5"), theme::MODEL_OPUS);
        assert_eq!(model_color("opus"), theme::MODEL_OPUS);
    }

    #[test]
    fn test_model_color_sonnet() {
        assert_eq!(model_color("claude-sonnet-4"), theme::MODEL_SONNET);
        assert_eq!(model_color("sonnet"), theme::MODEL_SONNET);
    }

    #[test]
    fn test_model_color_haiku() {
        assert_eq!(model_color("claude-haiku-4"), theme::MODEL_HAIKU);
        assert_eq!(model_color("haiku"), theme::MODEL_HAIKU);
    }

    #[test]
    fn test_model_color_unknown() {
        assert_eq!(model_color("unknown-model"), theme::LABEL_MUTED);
    }

    #[test]
    fn test_cost_style_high() {
        let style = cost_style(150.0);
        assert_eq!(style.fg, Some(theme::DANGER));
    }

    #[test]
    fn test_cost_style_medium_high() {
        let style = cost_style(75.0);
        assert_eq!(style.fg, Some(theme::ERROR));
    }

    #[test]
    fn test_cost_style_medium() {
        let style = cost_style(30.0);
        assert_eq!(style.fg, Some(theme::WARNING));
    }

    #[test]
    fn test_cost_style_low() {
        let style = cost_style(10.0);
        assert_eq!(style.fg, Some(theme::SUCCESS));
    }

    fn create_test_state() -> crate::AppState {
        use crate::aggregator::{DailyGroup, SessionInfo, TokenStats};

        let today = chrono::Local::now().date_naive();
        let first_date = today - chrono::Duration::days(9); // 10 calendar days

        let mut hourly_work = std::collections::HashMap::new();
        hourly_work.insert(10u8, 3000u64);
        hourly_work.insert(11u8, 3000u64);

        let mut day_tokens_by_model = std::collections::HashMap::new();
        day_tokens_by_model.insert(
            "claude-sonnet-4-20250514".to_string(),
            crate::aggregator::ModelTokens {
                input_tokens: 4000,
                output_tokens: 800,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        );

        let mut day_tool_usage = std::collections::HashMap::new();
        day_tool_usage.insert("Bash".to_string(), 5);

        let mut day_language_usage = std::collections::HashMap::new();
        day_language_usage.insert("Rust".to_string(), 10);
        day_language_usage.insert("TypeScript".to_string(), 5);

        let mut day_extension_usage = std::collections::HashMap::new();
        day_extension_usage.insert("rs".to_string(), 10);
        day_extension_usage.insert("ts".to_string(), 3);
        day_extension_usage.insert("tsx".to_string(), 2);

        let session = SessionInfo {
            file_path: std::path::PathBuf::from("/tmp/test.jsonl"),
            project_name: "test-project".to_string(),
            git_branch: None,
            session_first_timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            model: Some("claude-sonnet-4-20250514".to_string()),
            day_input_tokens: 5000,
            day_output_tokens: 1000,
            day_tokens_by_model,
            day_hourly_activity: std::collections::HashMap::new(),
            day_hourly_work_tokens: hourly_work,
            day_tool_usage,
            day_language_usage,
            day_extension_usage,
            day_first_timestamp: chrono::Utc::now() - chrono::Duration::hours(1),
            day_last_timestamp: chrono::Utc::now(),
            summary: None,
            custom_title: None,
            is_subagent: false,
            is_continued: false,
        };

        let group = DailyGroup {
            date: today,
            sessions: vec![session],
        };

        let past_group = DailyGroup {
            date: first_date,
            sessions: vec![],
        };

        let mut stats = crate::aggregator::Stats::default();
        stats.total_tokens = TokenStats {
            input_tokens: 50000,
            output_tokens: 10000,
            cache_creation_tokens: 0,
            cache_read_tokens: 40000,
        };
        stats.tool_success_count = 90;
        stats.tool_error_count = 10;
        stats.total_sessions_count = 10;
        stats.sessions_with_summary = 8;
        stats.tool_usage.insert("Bash".to_string(), 50);
        stats.tool_usage.insert("Read".to_string(), 30);
        stats.language_usage.insert("Rust".to_string(), 120);
        stats.language_usage.insert("TypeScript".to_string(), 85);
        stats.language_usage.insert("Other".to_string(), 30);
        stats.extension_usage.insert("rs".to_string(), 120);
        stats.extension_usage.insert("ts".to_string(), 60);
        stats.extension_usage.insert("tsx".to_string(), 25);
        stats.extension_usage.insert("example".to_string(), 15);
        stats.extension_usage.insert("xyz".to_string(), 10);
        stats.extension_usage.insert("abc".to_string(), 5);

        let mut aggregated_model_tokens = std::collections::HashMap::new();
        aggregated_model_tokens.insert(
            "Sonnet 4".to_string(),
            TokenStats {
                input_tokens: 40000,
                output_tokens: 10000,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
        );

        let daily_groups = vec![past_group, group];
        let daily_costs = vec![(today, 10.0), (first_date, 5.0)];
        let model_costs = vec![("Sonnet 4".to_string(), 100.0)];

        crate::AppState {
            needs_draw: false,
            tab: crate::Tab::Insights,
            pins: crate::pins::Pins::empty(),
            conv_list_mode: crate::ConvListMode::Day,
            stats: stats.clone(),
            total_cost: 100.0,
            cost_without_subagents: 90.0,
            model_costs: model_costs.clone(),
            aggregated_model_tokens: aggregated_model_tokens.clone(),
            models_without_pricing: std::collections::HashSet::new(),
            daily_groups: daily_groups.clone(),
            daily_costs: daily_costs.clone(),
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
            loading: false,
            error: None,
            file_count: 10,
            cache_stats: None,
            dashboard_panel: 0,
            dashboard_scroll: [0; 7],
            show_dashboard_detail: false,
            search_mode: false,
            search_query: String::new(),
            search_results: Vec::new(),
            search_selected: 0,
            search_task: None,
            searching: false,
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
            data_limit: 50,
            animation_frame: 0,
            retention_warning: None,
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
            period_filter: crate::PeriodFilter::All,
            show_filter_popup: false,
            filter_popup_selected: 0,
            filter_input_mode: false,
            filter_input: String::new(),
            filter_input_cursor: 0,
            filter_input_error: false,
            project_filter: None,
            show_project_popup: false,
            project_popup_selected: 0,
            project_popup_scroll: 0,
            project_list: vec![
                ("~/projects/app-a".to_string(), 50000, today),
                (
                    "~/projects/other-project".to_string(),
                    20000,
                    today - chrono::Duration::days(3),
                ),
            ],
            original_daily_groups: daily_groups,
            original_daily_costs: daily_costs,
            original_stats: stats,
            original_total_cost: 100.0,
            original_cost_without_subagents: 90.0,
            original_model_costs: model_costs,
            original_aggregated_model_tokens: aggregated_model_tokens,
        }
    }

    fn render_to_text(state: &mut crate::AppState, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn render_buffer(
        state: &mut crate::AppState,
        width: u16,
        height: u16,
    ) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn test_draw_insights_renders_without_panic() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);
        assert!(text.contains("/day"), "should contain /day metric");
    }

    #[test]
    fn test_draw_insights_uses_calendar_days() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);

        // total_work_tokens = 50000 + 10000 = 60000
        // calendar_days = 10
        // tokens_per_day = 60000 / 10 = 6000 = "6.00K"
        // If active_days were used: 60000 / 2 = 30000 = "30.0K"
        assert!(
            text.contains("6.00K/day"),
            "tokens/day should use calendar_days (10), got buffer:\n{}",
            text
        );
        assert!(
            !text.contains("30.0K/day"),
            "should NOT use active_days (2) for tokens/day"
        );
    }

    #[test]
    fn test_draw_dashboard_renders_without_panic() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_insights_weekly_monthly_no_bottom_padding() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let buffer = render_buffer(&mut state, 120, 35);

        let help_row = buffer.area.height - 1;
        let panel_last_row = help_row - 1;
        let mut has_border = false;
        for x in 0..buffer.area.width {
            let sym = buffer[(x, panel_last_row)].symbol();
            if sym == "─" || sym == "┘" || sym == "└" || sym == "┴" {
                has_border = true;
                break;
            }
        }
        assert!(
            has_border,
            "last row before help should be panel bottom border, not empty space"
        );
    }

    #[test]
    fn test_draw_daily_renders_without_panic() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Daily;
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_draw_insights_detail_popup_renders() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.show_insights_detail = true;

        let panel_markers = [
            (0, "Cache Hit Rate"),
            (1, "/day"),
            (2, "Monday"),
            (3, "avg"),
        ];
        for (panel, expected) in panel_markers {
            state.insights_panel = panel;
            let text = render_to_text(&mut state, 120, 35);
            assert!(
                text.contains(expected),
                "insights detail panel {} should contain '{}', got:\n{}",
                panel,
                expected,
                text
            );
        }
    }

    #[test]
    fn test_draw_dashboard_detail_popup_renders() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.show_dashboard_detail = true;

        let panel_markers = [
            (0, "Daily Costs"),
            (1, "close"), // Projects: dynamic title, verify popup footer
            (2, "Model Tokens"),
            (3, "Tool Usage"),
            (4, "Languages"),
            (5, "Daily Activity"),
            (6, "Hourly avg"),
        ];
        for (panel, expected) in panel_markers {
            state.dashboard_panel = panel;
            let text = render_to_text(&mut state, 120, 35);
            assert!(
                text.contains(expected),
                "dashboard detail panel {} should contain '{}', got:\n{}",
                panel,
                expected,
                text
            );
        }
    }

    #[test]
    fn test_draw_help_overlay_renders() {
        let mut state = create_test_state();
        state.show_help = true;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Switch tabs"),
            "help overlay should contain keybinding text 'Switch tabs'"
        );
        assert!(
            text.contains("Quit application"),
            "help overlay should contain 'Quit application'"
        );
    }

    #[test]
    fn test_draw_narrow_terminal() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        render_to_text(&mut state, 60, 20);

        state.tab = crate::Tab::Insights;
        render_to_text(&mut state, 60, 20);

        state.tab = crate::Tab::Daily;
        render_to_text(&mut state, 60, 20);
    }

    #[test]
    fn test_draw_minimal_terminal() {
        let mut state = create_test_state();
        for tab in [
            crate::Tab::Dashboard,
            crate::Tab::Daily,
            crate::Tab::Insights,
        ] {
            state.tab = tab;
            render_to_text(&mut state, 40, 10);
        }
    }

    #[test]
    fn test_draw_loading_state() {
        let mut state = create_test_state();
        state.loading = true;
        // Loading screen shows animated logo, not a tab view
        // Just verify it renders without panic
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_draw_error_state() {
        let mut state = create_test_state();
        state.error = Some("Test error message".to_string());
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Test error message"),
            "error state should display the error message"
        );
    }

    #[test]
    fn test_draw_empty_data() {
        let mut state = create_test_state();
        state.daily_groups.clear();
        state.daily_costs.clear();
        state.total_cost = 0.0;
        state.stats = crate::aggregator::Stats::default();

        for tab in [
            crate::Tab::Dashboard,
            crate::Tab::Daily,
            crate::Tab::Insights,
        ] {
            state.tab = tab;
            render_to_text(&mut state, 120, 35);
        }
    }

    #[test]
    fn test_insights_metrics_values() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);

        // cache_hit_rate = 40000 / (50000 + 40000) * 100 = 44.4%
        assert!(
            text.contains("44.4% cache"),
            "should show Cache Hit Rate"
        );
        // tool_success_rate = 90 / (90+10) * 100 = 90.0%
        assert!(
            text.contains("90.0% success"),
            "should show tool success rate 90.0%"
        );
        // completion_rate = 8 / 10 * 100 = 80%
        assert!(
            text.contains("80% summary"),
            "should show completion rate 80%"
        );
        // avg_cost_per_day = 100.0 / 10 = $10.0
        assert!(
            text.contains("$10.0/day cost"),
            "should show $10.0/day cost"
        );
        // tokens_per_day = 60000 / 10 = 6000
        assert!(
            text.contains("6.00K/day tokens"),
            "should show 6.00K/day tokens"
        );
    }

    #[test]
    fn test_cost_style_boundary_values() {
        // 100.0 is NOT > 100.0, so it should be ERROR, not DANGER
        assert_eq!(cost_style(100.0).fg, Some(theme::ERROR));
        assert_eq!(cost_style(100.01).fg, Some(theme::DANGER));
        // 50.0 is NOT > 50.0, so it should be WARNING, not ERROR
        assert_eq!(cost_style(50.0).fg, Some(theme::WARNING));
        assert_eq!(cost_style(50.01).fg, Some(theme::ERROR));
        // 20.0 is NOT > 20.0, so it should be SUCCESS, not WARNING
        assert_eq!(cost_style(20.0).fg, Some(theme::SUCCESS));
        assert_eq!(cost_style(20.01).fg, Some(theme::WARNING));
    }

    #[test]
    fn test_draw_insights_single_day_data() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        // Only today's data, so calendar_days = 1
        let today = chrono::Local::now().date_naive();
        state.daily_groups.retain(|g| g.date == today);

        let text = render_to_text(&mut state, 120, 35);
        // calendar_days = 1, tokens_per_day = 60000 / 1 = 60000 = "60.0K"
        assert!(
            text.contains("60.0K/day"),
            "with single day, tokens_per_day should be 60.0K, got:\n{}",
            text
        );
    }

    #[test]
    fn test_draw_insights_subagent_excluded() {
        use crate::aggregator::SessionInfo;

        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;

        let today = chrono::Local::now().date_naive();
        let subagent_session = SessionInfo {
            file_path: std::path::PathBuf::from("/tmp/agent-test.jsonl"),
            project_name: "test-project".to_string(),
            git_branch: None,
            session_first_timestamp: chrono::Utc::now(),
            model: Some("claude-haiku-4-5-20250514".to_string()),
            day_input_tokens: 100000,
            day_output_tokens: 100000,
            day_tokens_by_model: std::collections::HashMap::new(),
            day_hourly_activity: std::collections::HashMap::new(),
            day_hourly_work_tokens: std::collections::HashMap::new(),
            day_tool_usage: std::collections::HashMap::new(),
            day_language_usage: std::collections::HashMap::new(),
            day_extension_usage: std::collections::HashMap::new(),
            day_first_timestamp: chrono::Utc::now(),
            day_last_timestamp: chrono::Utc::now(),
            summary: None,
            custom_title: None,
            is_subagent: true,
            is_continued: false,
        };

        // Add subagent session to today's group
        if let Some(group) = state.daily_groups.iter_mut().find(|g| g.date == today) {
            group.sessions.push(subagent_session);
        }

        let text = render_to_text(&mut state, 120, 35);
        // tokens_per_day should still be 6.00K (subagent excluded from count)
        // total_sessions in draw_insights counts only non-subagent sessions
        assert!(
            text.contains("1 sessions"),
            "subagent sessions should be excluded from session count, got:\n{}",
            text
        );
    }

    #[test]
    fn test_draw_empty_data_all_panels() {
        let mut state = create_test_state();
        state.daily_groups.clear();
        state.daily_costs.clear();
        state.total_cost = 0.0;
        state.stats = crate::aggregator::Stats::default();

        state.tab = crate::Tab::Insights;
        for panel in 0..4 {
            state.insights_panel = panel;
            render_to_text(&mut state, 120, 35);
        }

        state.show_insights_detail = true;
        for panel in 0..4 {
            state.insights_panel = panel;
            render_to_text(&mut state, 120, 35);
        }
    }

    #[test]
    fn test_insights_detail_popup_calendar_days_consistency() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.show_insights_detail = true;
        state.insights_panel = 0;

        let text = render_to_text(&mut state, 120, 35);
        // Popup-specific text: "/day   tokens" (with extra spaces) is unique to detail popup
        // Main view uses "6.00K/day tokens" (no extra spaces)
        assert!(
            text.contains("Cache Hit Rate"),
            "detail popup panel 0 should be visible"
        );
        assert!(
            text.contains("/day"),
            "detail popup should show /day metric"
        );
        // Verify calendar_days is used: total is "$100.00 / 10d"
        assert!(
            text.contains("10 days"),
            "detail popup should show 10 calendar days"
        );
    }

    #[test]
    fn test_insights_popup_on_non_insights_tab() {
        // show_insights_detail is checked outside of tab guard in draw()
        // Verify it doesn't panic on Dashboard or Daily tabs
        let mut state = create_test_state();
        state.show_insights_detail = true;
        state.insights_panel = 0;

        state.tab = crate::Tab::Dashboard;
        render_to_text(&mut state, 120, 35);

        state.tab = crate::Tab::Daily;
        render_to_text(&mut state, 120, 35);
    }

    #[test]
    fn test_model_efficiency_in_detail_popup() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Dashboard;
        state.show_dashboard_detail = true;
        state.dashboard_panel = 2;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("/MTok"),
            "Dashboard detail popup should contain /MTok, got:\n{}",
            text
        );
        assert!(
            text.contains("rate/MTok"),
            "Dashboard detail popup should contain documented rate, got:\n{}",
            text
        );
    }

    #[test]
    fn test_monthly_forecast_in_insights() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("est"),
            "Insights Monthly panel should contain forecast 'est', got:\n{}",
            text
        );
    }

    #[test]
    fn test_monthly_forecast_in_insights_detail() {
        let mut state = create_test_state();
        state.tab = crate::Tab::Insights;
        state.show_insights_detail = true;
        state.insights_panel = 3;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("est"),
            "Insights detail popup panel 3 should contain forecast 'est', got:\n{}",
            text
        );
    }

    #[test]
    fn test_period_filter_label() {
        use crate::PeriodFilter;
        assert_eq!(PeriodFilter::All.label(), "All");
        assert_eq!(PeriodFilter::Today.label(), "Today");
        assert_eq!(PeriodFilter::Last7d.label(), "7d");
        assert_eq!(PeriodFilter::Last30d.label(), "30d");
        assert_eq!(PeriodFilter::ThisMonth.label(), "This Month");
        assert_eq!(PeriodFilter::LastMonth.label(), "Last Month");
        assert_eq!(PeriodFilter::Last90d.label(), "90d");
    }

    #[test]
    fn test_period_filter_date_range() {
        use crate::PeriodFilter;
        use chrono::Datelike;

        let (start, end) = PeriodFilter::All.date_range();
        assert!(start.is_none());
        assert!(end.is_none());

        let (start, end) = PeriodFilter::Today.date_range();
        let today = chrono::Local::now().date_naive();
        assert_eq!(start, Some(today));
        assert!(end.is_none());

        let (start, end) = PeriodFilter::LastMonth.date_range();
        assert!(start.is_some());
        assert!(end.is_some());
        let s = start.unwrap();
        let e = end.unwrap();
        assert_eq!(s.day(), 1);
        assert!(e < today);
        assert_eq!(e.month(), s.month());
    }

    #[test]
    fn test_period_filter_all_variants_count() {
        assert_eq!(crate::PeriodFilter::ALL_VARIANTS.len(), 8);
    }

    #[test]
    fn test_apply_filter_7d_excludes_old_data() {
        let mut state = create_test_state();
        let today = chrono::Local::now().date_naive();
        let old_date = today - chrono::Duration::days(9);

        assert_eq!(state.daily_groups.len(), 2);
        assert!(state.daily_groups.iter().any(|g| g.date == old_date));

        state.period_filter = crate::PeriodFilter::Last7d;
        state.apply_filter();

        assert!(!state.daily_groups.iter().any(|g| g.date == old_date));
        assert!(state.daily_groups.iter().any(|g| g.date == today));
    }

    #[test]
    fn test_apply_filter_all_restores_data() {
        let mut state = create_test_state();
        let original_len = state.daily_groups.len();

        state.period_filter = crate::PeriodFilter::Last7d;
        state.apply_filter();
        assert!(state.daily_groups.len() < original_len);

        state.period_filter = crate::PeriodFilter::All;
        state.apply_filter();
        assert_eq!(state.daily_groups.len(), original_len);
    }

    #[test]
    fn test_filter_header_shows_label() {
        let mut state = create_test_state();
        state.period_filter = crate::PeriodFilter::Last7d;
        state.apply_filter();
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("7d"),
            "Header should show filter label '7d' when filter is active, got:\n{}",
            text
        );
    }

    #[test]
    fn test_filtered_rendering_no_panic() {
        let mut state = create_test_state();
        for filter in crate::PeriodFilter::ALL_VARIANTS {
            state.period_filter = filter;
            state.apply_filter();
            for tab in [
                crate::Tab::Dashboard,
                crate::Tab::Daily,
                crate::Tab::Insights,
            ] {
                state.tab = tab;
                render_to_text(&mut state, 120, 35);
            }
        }
    }

    #[test]
    fn test_filter_popup_renders() {
        let mut state = create_test_state();
        state.show_filter_popup = true;
        state.filter_popup_selected = 2;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Filter Period"),
            "Filter popup should show title"
        );
        assert!(text.contains("All"), "Filter popup should show All option");
        assert!(
            text.contains("Today"),
            "Filter popup should show Today option"
        );
    }

    #[test]
    fn test_help_popup_shows_filter_keybind() {
        let mut state = create_test_state();
        state.show_help = true;
        let text = render_to_text(&mut state, 120, 40);
        assert!(
            text.contains("period filter"),
            "Help popup should mention period filter, got:\n{}",
            text
        );
    }

    #[test]
    fn test_project_popup_renders() {
        let mut state = create_test_state();
        state.show_project_popup = true;
        state.project_popup_selected = 1;
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("Filter Project"),
            "Project popup should show title, got:\n{}",
            text
        );
        assert!(
            text.contains("app-a"),
            "Project popup should show project name, got:\n{}",
            text
        );
    }

    #[test]
    fn test_project_filter_header() {
        let mut state = create_test_state();
        state.project_filter = Some("~/projects/app-a".to_string());
        let text = render_to_text(&mut state, 120, 35);
        assert!(
            text.contains("app-a"),
            "Header should show project name when filter is active, got:\n{}",
            text
        );
    }
}
