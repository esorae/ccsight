use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Frame,
};

use crate::aggregator::CostCalculator;
use crate::AppState;
use super::theme;
use super::{cost_style, calc_scroll};

pub(super) fn draw_dashboard(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let chunks = Layout::vertical([
        Constraint::Length(4),  // Stats cards (with border)
        Constraint::Length(10), // Heatmap + Hourly pattern (fixed to match week rows)
        Constraint::Fill(1),   // Bottom section (scales with terminal)
        Constraint::Length(1), // Help
    ])
    .split(area);

    draw_stats_cards(frame, chunks[0], state);

    let heatmap_row =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(30)]).split(chunks[1]);

    draw_heatmap(
        frame,
        heatmap_row[0],
        state,
        state.dashboard_panel == 5,
        state.dashboard_scroll[5],
    );
    draw_hourly_pattern(
        frame,
        heatmap_row[1],
        state,
        state.dashboard_panel == 6,
        state.dashboard_scroll[6],
    );

    let bottom_rows =
        Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).split(chunks[2]);

    let top_row = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(bottom_rows[0]);

    let bottom_row = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(34),
        Constraint::Percentage(33),
    ])
    .split(bottom_rows[1]);

    // Store panel areas for click detection
    state.dashboard_panel_areas = vec![
        top_row[0],     // 0: Recent Costs
        top_row[1],     // 1: Top Projects
        bottom_row[0],  // 2: Model Tokens
        bottom_row[1],  // 3: Tool Usage
        bottom_row[2],  // 4: Languages
        heatmap_row[0], // 5: Heatmap
        heatmap_row[1], // 6: Hourly Pattern
    ];

    draw_recent_costs(
        frame,
        top_row[0],
        state,
        state.dashboard_panel == 0,
        state.dashboard_scroll[0],
    );
    draw_top_projects(
        frame,
        top_row[1],
        state,
        state.dashboard_panel == 1,
        state.dashboard_scroll[1],
    );
    draw_model_tokens(
        frame,
        bottom_row[0],
        state,
        state.dashboard_panel == 2,
        state.dashboard_scroll[2],
    );
    draw_tool_usage(
        frame,
        bottom_row[1],
        state,
        state.dashboard_panel == 3,
        state.dashboard_scroll[3],
    );
    draw_languages(
        frame,
        bottom_row[2],
        state,
        state.dashboard_panel == 4,
        state.dashboard_scroll[4],
    );

    let help_spans = vec![
        Span::styled(" ?", Style::default().fg(theme::PRIMARY)),
        Span::styled(":help ", Style::default().fg(theme::DIM)),
        Span::styled("q", Style::default().fg(theme::PRIMARY)),
        Span::styled(":quit ", Style::default().fg(theme::DIM)),
        Span::styled("←→", Style::default().fg(theme::PRIMARY)),
        Span::styled(":panel ", Style::default().fg(theme::DIM)),
        Span::styled("↑↓", Style::default().fg(theme::PRIMARY)),
        Span::styled(":scroll ", Style::default().fg(theme::DIM)),
        Span::styled("Enter", Style::default().fg(theme::PRIMARY)),
        Span::styled(":detail ", Style::default().fg(theme::DIM)),
        Span::styled("/", Style::default().fg(theme::PRIMARY)),
        Span::styled(":search ", Style::default().fg(theme::DIM)),
        Span::styled("m", Style::default().fg(theme::PRIMARY)),
        Span::styled(":pins", Style::default().fg(theme::DIM)),
    ];
    let help_line = Paragraph::new(Line::from(help_spans));
    frame.render_widget(help_line, chunks[3]);
}

fn draw_stats_cards(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .title(Span::styled(
            " Overview ",
            Style::default().fg(theme::PRIMARY),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(inner);

    let session_count: usize = state
        .daily_groups
        .iter()
        .map(|g| g.sessions.iter().filter(|s| !s.is_subagent).count())
        .sum();
    let sessions_card = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("{session_count}"),
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )).alignment(Alignment::Center),
        Line::from(Span::styled("sessions", Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    let active_days = state.daily_groups.len();
    let days_card = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("{active_days}"),
            Style::default()
                .fg(theme::WARM)
                .add_modifier(Modifier::BOLD),
        )).alignment(Alignment::Center),
        Line::from(Span::styled("days", Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    let tokens_card = Paragraph::new(vec![
        Line::from(Span::styled(
            crate::format_number(state.stats.total_tokens.work_tokens()),
            Style::default()
                .fg(theme::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )).alignment(Alignment::Center),
        Line::from(Span::styled("tokens", Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    let cost_card = Paragraph::new(vec![
        Line::from(Span::styled(
            super::format_cost(state.total_cost, 2),
            Style::default()
                .fg(theme::WARM)
                .add_modifier(Modifier::BOLD),
        )).alignment(Alignment::Center),
        Line::from(Span::styled("total cost (API est.)", Style::default().fg(theme::DIM)))
            .alignment(Alignment::Center),
    ])
    .block(Block::default().borders(Borders::NONE));

    frame.render_widget(sessions_card, chunks[0]);
    frame.render_widget(days_card, chunks[1]);
    frame.render_widget(tokens_card, chunks[2]);
    frame.render_widget(cost_card, chunks[3]);
}

fn draw_heatmap(frame: &mut Frame, area: Rect, state: &AppState, selected: bool, scroll: usize) {
    use chrono::{Datelike, Duration, Local};

    let today = Local::now().date_naive();
    let available_width = area.width.saturating_sub(2) as usize;
    let max_weeks_for_width = available_width.saturating_sub(4) / 2;
    let weeks = max_weeks_for_width.clamp(13, 52);

    let daily_work: std::collections::HashMap<chrono::NaiveDate, u64> = state
        .daily_groups
        .iter()
        .map(|group| {
            let tokens: u64 = group
                .sessions
                .iter()
                .filter(|s| !s.is_subagent)
                .map(|s| s.day_input_tokens + s.day_output_tokens)
                .sum();
            (group.date, tokens)
        })
        .collect();

    let oldest_date = daily_work.keys().min().copied();
    let max_scroll_weeks = if let Some(oldest) = oldest_date {
        let days_from_oldest = (today - oldest).num_days().max(0) as usize;
        days_from_oldest / 7
    } else {
        0
    };
    let scroll = scroll.min(max_scroll_weeks);

    let scroll_weeks = scroll as i64;
    let today_weekday = today.weekday().num_days_from_sunday() as i64;
    let last_saturday =
        today + Duration::days(6 - today_weekday) - Duration::days(scroll_weeks * 7);
    let adjusted_start = last_saturday - Duration::days((weeks * 7 - 1) as i64);
    let display_end = last_saturday;

    let max_tokens = daily_work.values().max().copied().unwrap_or(1);
    let get_color = |tokens: u64| -> Color {
        if tokens == 0 {
            theme::HEATMAP_EMPTY
        } else {
            let ratio = tokens as f64 / max_tokens as f64;
            if ratio < 0.15 {
                theme::HEATMAP_LOW
            } else if ratio < 0.35 {
                theme::HEATMAP_MID
            } else if ratio < 0.65 {
                theme::HEATMAP_HIGH
            } else {
                theme::PRIMARY
            }
        }
    };

    let month_name = |m: u32| -> &'static str {
        match m {
            1 => "Jan",
            2 => "Feb",
            3 => "Mar",
            4 => "Apr",
            5 => "May",
            6 => "Jun",
            7 => "Jul",
            8 => "Aug",
            9 => "Sep",
            10 => "Oct",
            11 => "Nov",
            12 => "Dec",
            _ => "",
        }
    };

    let mut lines: Vec<Line> = Vec::new();

    let content_width = 4 + weeks * 2;
    let padding = if available_width > content_width {
        (available_width - content_width) / 2
    } else {
        0
    };
    let pad_str = " ".repeat(padding);

    let mut month_row: Vec<Span> = vec![Span::raw(format!("{pad_str}    "))];
    let mut prev_month = 0u32;
    let mut prev_year = 0i32;
    let mut used_chars = 0usize;
    for week in 0..weeks {
        let expected_pos = week * 2;
        let week_start = adjusted_start + Duration::days((week * 7) as i64);
        let month = week_start.month();
        let year = week_start.year();
        if month != prev_month {
            let label = if year != prev_year || week == 0 {
                format!("{}/{}", year % 100, month_name(month))
            } else {
                month_name(month).to_string()
            };
            let gap = expected_pos.saturating_sub(used_chars);
            if gap > 0 {
                month_row.push(Span::raw(" ".repeat(gap)));
                used_chars += gap;
            }
            month_row.push(Span::styled(
                label.clone(),
                Style::default().fg(theme::LABEL_SUBTLE),
            ));
            used_chars += label.len();
            prev_month = month;
            prev_year = year;
        }
    }
    lines.push(Line::from(month_row));

    let day_labels = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    for (day_idx, day_label) in day_labels.iter().enumerate() {
        let label = match day_idx {
            1 | 3 | 5 => *day_label,
            _ => "",
        };
        let mut row_spans: Vec<Span> = vec![Span::styled(
            format!("{pad_str}{label:<4}"),
            Style::default().fg(theme::LABEL_SUBTLE),
        )];

        for week in 0..weeks {
            let date = adjusted_start + Duration::days((week * 7 + day_idx) as i64);
            if date <= display_end && date <= today {
                let tokens = daily_work.get(&date).copied().unwrap_or(0);
                let color = get_color(tokens);
                row_spans.push(Span::styled("■ ", Style::default().fg(color)));
            } else {
                row_spans.push(Span::raw("  "));
            }
        }
        lines.push(Line::from(row_spans));
    }

    let start_str = adjusted_start.format("%m-%d").to_string();
    let end_str = display_end.min(today).format("%m-%d").to_string();
    let legend_bottom = Line::from(vec![
        Span::styled(
            format!(" {start_str} - {end_str}  Less "),
            Style::default().fg(theme::LABEL_SUBTLE),
        ),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_EMPTY)),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_LOW)),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_MID)),
        Span::styled("■ ", Style::default().fg(theme::HEATMAP_HIGH)),
        Span::styled("■ ", Style::default().fg(theme::PRIMARY)),
        Span::styled(" More ", Style::default().fg(theme::LABEL_SUBTLE)),
    ]);

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let actual_end = display_end.min(today);
    let title = if selected {
        format!(
            " ◈ Activity {}/{} - {}/{}",
            adjusted_start.format("%y"),
            adjusted_start.format("%m"),
            actual_end.format("%y"),
            actual_end.format("%m")
        )
    } else {
        format!(
            " ◇ Activity {}/{} - {}/{}",
            adjusted_start.format("%y"),
            adjusted_start.format("%m"),
            actual_end.format("%y"),
            actual_end.format("%m")
        )
    };

    let heatmap = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style))
            .title_bottom(legend_bottom),
    );

    frame.render_widget(heatmap, area);
}

fn draw_hourly_pattern(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    _scroll: usize,
) {
    let mut hourly_total: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
    for group in &state.daily_groups {
        for session in &group.sessions {
            if session.is_subagent {
                continue;
            }
            for (hour, tokens) in &session.day_hourly_work_tokens {
                *hourly_total.entry(*hour).or_insert(0) += tokens;
            }
        }
    }
    let num_days = state.daily_groups.len().max(1) as u64;

    let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
        .iter()
        .map(|(h, t)| (*h, *t / num_days))
        .collect();

    let max_tokens = hourly_avg.values().max().copied().unwrap_or(1);
    let total_avg: u64 = hourly_avg.values().sum();

    let bar_chars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let inner_height = area.height.saturating_sub(3) as usize;

    let mut lines: Vec<Line> = Vec::new();

    // Bar graph (vertical style in rows)
    for row in (0..inner_height).rev() {
        let threshold = (row as f64 + 0.5) / inner_height as f64;
        let mut row_chars = String::new();
        for hour in 0..24u8 {
            let tokens = hourly_avg.get(&hour).copied().unwrap_or(0);
            let ratio = tokens as f64 / max_tokens as f64;
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
        lines.push(Line::from(Span::styled(
            row_chars,
            Style::default().fg(theme::PRIMARY),
        )));
    }

    // Hour labels
    lines.push(Line::from(Span::styled(
        "0     6    12    18   24",
        Style::default().fg(theme::DIM),
    )));

    let peak_hour = hourly_avg.iter().max_by_key(|(_, t)| *t).map(|(h, _)| *h);
    let peak_title = if let Some(h) = peak_hour {
        format!(" Peak: {}-{}h ({}) ", h, h + 1, crate::format_number(total_avg))
    } else {
        format!(" Peak: - ({}) ", crate::format_number(total_avg))
    };

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };
    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };
    let prefix = if selected { "◈" } else { "◇" };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            format!(" {prefix} Hourly avg "),
            title_style,
        )]))
        .title_bottom(Line::from(Span::styled(peak_title, Style::default().fg(theme::DIM))));

    let paragraph = Paragraph::new(lines).centered().block(block);
    frame.render_widget(paragraph, area);
}

fn draw_top_projects(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    scroll: usize,
) {
    let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
    projects.sort_by(|a, b| b.1.work_tokens.cmp(&a.1.work_tokens));

    let total_tokens: u64 = projects.iter().map(|(_, s)| s.work_tokens).sum();
    let (visible_height, _, scroll) = calc_scroll(area.height, projects.len(), scroll, 2);


    let rows: Vec<Row> = projects
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, stats))| {
            let percentage = if total_tokens > 0 {
                (stats.work_tokens as f64 / total_tokens as f64 * 100.0) as u32
            } else {
                0
            };
            let dir_name = super::shorten_project(name);
            let tokens_str = crate::format_number(stats.work_tokens);

            Row::new(vec![
                Cell::from(format!("{}.", i + 1)).style(Style::default().fg(theme::DIM)),
                Cell::from(dir_name),
                Cell::from(tokens_str).style(Style::default().fg(theme::PRIMARY)),
                Cell::from(format!("{percentage}%")).style(Style::default().fg(theme::MUTED)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let title = if selected {
        if let Some((full_path, _)) = projects.get(scroll) {
            format!(" ◈ {full_path}")
        } else {
            format!(" ◈ Projects {}/{}", scroll + 1, projects.len().max(1))
        }
    } else {
        format!(" ◇ Projects {}/{}", scroll + 1, projects.len().max(1))
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_model_tokens(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    scroll: usize,
) {
    let mut models: Vec<_> = state
        .aggregated_model_tokens
        .iter()
        .map(|(name, ts)| {
            let work_tokens = ts.input_tokens + ts.output_tokens;
            let cost = state
                .model_costs
                .iter()
                .find(|(n, _)| n == name)
                .map_or(0.0, |(_, c)| *c);
            (name.clone(), work_tokens, cost)
        })
        .collect();
    models.sort_by(|a, b| b.1.cmp(&a.1));

    let total_tokens: u64 = models.iter().map(|(_, t, _)| *t).sum();

    let (visible_height, _, scroll) = calc_scroll(area.height, models.len(), scroll, 2);

    let rows: Vec<Row> = models
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, tokens, _cost))| {
            let rank = format!("{}.", i + 1);
            let pct = if total_tokens > 0 {
                format!("{:.0}%", *tokens as f64 / total_tokens as f64 * 100.0)
            } else {
                "0%".to_string()
            };
            Row::new(vec![
                Cell::from(rank).style(Style::default().fg(theme::DIM)),
                Cell::from(name.clone()),
                Cell::from(crate::format_number(*tokens))
                    .style(Style::default().fg(theme::PRIMARY)),
                Cell::from(pct)
                    .style(Style::default().fg(theme::DIM)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let title = if selected {
        format!(" ◈ Models {}/{}", scroll + 1, models.len().max(1))
    } else {
        format!(" ◇ Models {}/{}", scroll + 1, models.len().max(1))
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_tool_usage(frame: &mut Frame, area: Rect, state: &AppState, selected: bool, scroll: usize) {
    let mut tools: Vec<_> = state
        .stats
        .tool_usage
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .collect();
    tools.sort_by(|a, b| b.1.cmp(a.1));

    let total_usage: usize = tools.iter().map(|(_, c)| **c).sum();
    let (visible_height, _, scroll) = calc_scroll(area.height, tools.len(), scroll, 2);

    let rows: Vec<Row> = tools
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, count))| {
            let rank = format!("{}.", i + 1);
            let percentage = if total_usage > 0 {
                (**count as f64 / total_usage as f64 * 100.0) as u32
            } else {
                0
            };
            Row::new(vec![
                Cell::from(rank).style(Style::default().fg(theme::DIM)),
                Cell::from(name.as_str()),
                Cell::from(count.to_string()).style(Style::default().fg(theme::PRIMARY)),
                Cell::from(format!("{percentage}%")).style(Style::default().fg(theme::MUTED)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let title = if selected {
        format!(" ◈ Tools {}/{}", scroll + 1, tools.len().max(1))
    } else {
        format!(" ◇ Tools {}/{}", scroll + 1, tools.len().max(1))
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_languages(frame: &mut Frame, area: Rect, state: &AppState, selected: bool, scroll: usize) {
    let mut languages: Vec<_> = state
        .stats
        .language_usage
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .collect();
    languages.sort_by(|a, b| b.1.cmp(a.1));

    let total_usage: usize = languages.iter().map(|(_, c)| **c).sum();
    let (visible_height, _, scroll) = calc_scroll(area.height, languages.len(), scroll, 2);

    let rows: Vec<Row> = languages
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (name, count))| {
            let rank = format!("{}.", i + 1);
            let percentage = if total_usage > 0 {
                (**count as f64 / total_usage as f64 * 100.0) as u32
            } else {
                0
            };
            Row::new(vec![
                Cell::from(rank).style(Style::default().fg(theme::DIM)),
                Cell::from(name.as_str()),
                Cell::from(count.to_string()).style(Style::default().fg(theme::PRIMARY)),
                Cell::from(format!("{percentage}%")).style(Style::default().fg(theme::MUTED)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let title = if selected {
        format!(" ◈ Languages {}/{}", scroll + 1, languages.len().max(1))
    } else {
        format!(" ◇ Languages {}/{}", scroll + 1, languages.len().max(1))
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(4),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

fn draw_recent_costs(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    selected: bool,
    scroll: usize,
) {
    let (visible_height, _, scroll) = calc_scroll(area.height, state.daily_costs.len(), scroll, 2);

    let rows: Vec<Row> = state
        .daily_costs
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, (date, cost))| {
            let date_str = date.format("%m-%d(%a)").to_string();
            let cost_display = cost.max(0.0);
            let cost_str = super::format_cost(cost_display, 2);
            let tokens: u64 = state
                .daily_groups
                .iter()
                .find(|g| &g.date == date)
                .map_or(0, |group| {
                    group
                        .sessions
                        .iter()
                        .filter(|s| !s.is_subagent)
                        .map(|s| s.day_input_tokens + s.day_output_tokens)
                        .sum()
                });
            Row::new(vec![
                Cell::from(format!("{}.", i + 1)).style(Style::default().fg(theme::DIM)),
                Cell::from(date_str),
                Cell::from(crate::format_number(tokens)).style(Style::default().fg(theme::DIM)),
                Cell::from(cost_str).style(cost_style(*cost)),
            ])
        })
        .collect();

    let border_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::BORDER)
    };

    let title_style = if selected {
        Style::default().fg(theme::PRIMARY)
    } else {
        Style::default().fg(theme::DIM)
    };

    let title = if selected {
        format!(" ◈ Costs {}/{}", scroll + 1, state.daily_costs.len().max(1))
    } else {
        format!(" ◇ Costs {}/{}", scroll + 1, state.daily_costs.len().max(1))
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(6),
            Constraint::Length(8),
        ],
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style)),
    );

    frame.render_widget(table, area);
}

pub(super) fn draw_dashboard_detail_popup(frame: &mut Frame, area: Rect, state: &AppState) {
    let popup_width = 90.min(area.width.saturating_sub(4));
    let popup_height = area.height.saturating_sub(4).min(30);
    let content_width = popup_width.saturating_sub(4) as usize;

    let popup_area = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let scroll = state.dashboard_scroll[state.dashboard_panel];
    let visible_height = popup_height.saturating_sub(4) as usize;

    fn truncate(s: &str, max_len: usize) -> String {
        use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
        if UnicodeWidthStr::width(s) <= max_len {
            s.to_string()
        } else {
            let mut width = 0;
            let mut result = String::new();
            for ch in s.chars() {
                let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
                if width + ch_w > max_len.saturating_sub(1) {
                    break;
                }
                result.push(ch);
                width += ch_w;
            }
            format!("{result}…")
        }
    }

    let total_items = match state.dashboard_panel {
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
    };
    let items_per_screen = match state.dashboard_panel {
        2 => visible_height / 3,
        _ => visible_height,
    };

    let can_scroll_up = scroll > 0;
    let can_scroll_down = scroll + items_per_screen < total_items;

    let (title, content) = match state.dashboard_panel {
        0 => {
            use chrono::Datelike;
            let title = " Daily Costs ".to_string();
            let mut lines: Vec<Line> = vec![Line::from("")];
            let max_cost = state
                .daily_costs
                .iter()
                .map(|(_, c)| *c)
                .fold(0.0f64, f64::max);
            let bar_width = 12usize;

            let mut prev_month: Option<u32> = None;
            for (i, (date, cost)) in state
                .daily_costs
                .iter()
                .enumerate()
                .skip(scroll)
                .take(visible_height)
            {
                let month = date.month();
                if let Some(pm) = prev_month
                    && pm != month {
                        let separator = "─".repeat(content_width.saturating_sub(2));
                        lines.push(Line::from(Span::styled(
                            format!("  {separator}"),
                            Style::default().fg(theme::SEPARATOR),
                        )));
                    }
                prev_month = Some(month);

                let cost_display = cost.max(0.0);
                let ratio = if max_cost > 0.0 {
                    *cost / max_cost
                } else {
                    0.0
                };
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = theme::primary_with_intensity(intensity);

                let tokens: u64 = state
                    .daily_groups
                    .iter()
                    .find(|g| &g.date == date)
                    .map_or(0, |group| {
                        group
                            .sessions
                            .iter()
                            .filter(|s| !s.is_subagent)
                            .map(|s| s.day_input_tokens + s.day_output_tokens)
                            .sum()
                    });

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(format!("{}({})", date, date.format("%a")), Style::default().fg(theme::LABEL_MUTED)),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(format!(" {}", super::format_cost(cost_display, 2)), cost_style(*cost)),
                    Span::styled(
                        format!(" ({})", crate::format_number(tokens)),
                        Style::default().fg(theme::DIM),
                    ),
                ]));
            }
            (title, lines)
        }
        1 => {
            let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
            projects.sort_by(|a, b| b.1.work_tokens.cmp(&a.1.work_tokens));
            let selected_path = projects
                .get(scroll)
                .map_or("", |(name, _)| name.as_str());
            let title = format!(" ◈ {selected_path} ");
            let mut lines: Vec<Line> = vec![Line::from("")];
            let max_tokens = projects.first().map_or(1, |(_, s)| s.work_tokens);
            let bar_width = 12usize;
            let name_width = content_width.saturating_sub(45);

            for (i, (name, stats)) in projects
                .iter()
                .enumerate()
                .skip(scroll)
                .take(visible_height)
            {
                let ratio = stats.work_tokens as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = Color::Rgb(
                    (140.0 + 78.0 * intensity) as u8,
                    (100.0 + 68.0 * intensity) as u8,
                    (180.0 + 75.0 * intensity) as u8,
                );
                let display_name = truncate(name, name_width);

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{display_name:<name_width$}"),
                        Style::default().fg(theme::SECONDARY),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {:>9}", crate::format_number(stats.work_tokens)),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::styled(
                        format!(" {:>2}s", stats.sessions),
                        Style::default().fg(theme::DIM),
                    ),
                ]));
            }
            (title, lines)
        }
        2 => {
            let title = " Model Tokens ".to_string();
            let mut lines: Vec<Line> = vec![Line::from("")];
            let calculator = CostCalculator::global();

            let mut model_last_used: std::collections::HashMap<String, chrono::NaiveDate> =
                std::collections::HashMap::new();
            for group in &state.daily_groups {
                for session in &group.sessions {
                    for model_name in session.day_tokens_by_model.keys() {
                        let normalized = crate::aggregator::normalize_model_name(model_name);
                        let entry = model_last_used.entry(normalized).or_insert(group.date);
                        if group.date > *entry {
                            *entry = group.date;
                        }
                    }
                }
            }

            let mut models: Vec<_> = state
                .aggregated_model_tokens
                .iter()
                .map(|(name, ts)| {
                    let work_tokens = ts.input_tokens + ts.output_tokens;
                    let cost = state
                        .model_costs
                        .iter()
                        .find(|(n, _)| n == name)
                        .map_or(0.0, |(_, c)| *c);
                    (name.clone(), ts.clone(), work_tokens, cost)
                })
                .collect();
            models.sort_by(|a, b| b.2.cmp(&a.2));

            let total_tokens: u64 = models.iter().map(|(_, _, t, _)| *t).sum();
            let max_tokens = models.first().map_or(1, |(_, _, t, _)| *t);
            let bar_width = 15usize;
            let name_width = content_width.saturating_sub(50);
            let items_visible = visible_height / 3;

            for (i, (model, ts, work_tokens, cost)) in
                models.iter().enumerate().skip(scroll).take(items_visible)
            {
                let ratio = if max_tokens > 0 {
                    *work_tokens as f64 / max_tokens as f64
                } else {
                    0.0
                };
                let pct = if total_tokens > 0 {
                    (*work_tokens as f64 / total_tokens as f64 * 100.0) as u32
                } else {
                    0
                };
                let filled = (ratio * bar_width as f64).round() as usize;
                let unknown = state.models_without_pricing.contains(model);
                let bar = if unknown {
                    format!("{}{}", "░".repeat(filled), " ".repeat(bar_width - filled))
                } else {
                    format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled))
                };
                let bar_color = if unknown {
                    theme::WARNING
                } else {
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    Color::Rgb(
                        (100.0 + 118.0 * intensity) as u8,
                        (140.0 + 78.0 * intensity) as u8,
                        (200.0 + 55.0 * intensity) as u8,
                    )
                };

                let display_name = truncate(model, name_width);
                let token_info = if unknown {
                    format!(
                        "in:{} out:{} cache:{}  $? (pricing undefined)",
                        crate::format_number(ts.input_tokens),
                        crate::format_number(ts.output_tokens),
                        crate::format_number(ts.cache_creation_tokens + ts.cache_read_tokens),
                    )
                } else {
                    let effective = calculator
                        .cost_breakdown_by_display_name(model, ts)
                        .map(|b| {
                            use crate::aggregator::CostBreakdown;
                            let in_r = CostBreakdown::effective_rate(b.input, ts.input_tokens);
                            let out_r = CostBreakdown::effective_rate(b.output, ts.output_tokens);
                            let mut parts = Vec::new();
                            if let Some(r) = in_r {
                                parts.push(format!("in:${r:.1}"));
                            }
                            if let Some(r) = out_r {
                                parts.push(format!("out:${r:.1}"));
                            }
                            if !parts.is_empty() {
                                format!("  eff/MTok {}", parts.join(" "))
                            } else {
                                String::new()
                            }
                        })
                        .unwrap_or_default();
                    format!(
                        "in:{} out:{} cache:{}  ${:.2}{}",
                        crate::format_number(ts.input_tokens),
                        crate::format_number(ts.output_tokens),
                        crate::format_number(ts.cache_creation_tokens + ts.cache_read_tokens),
                        cost,
                        effective,
                    )
                };

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{display_name:<name_width$}"),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ]));
                let last_used = model_last_used
                    .get(model.as_str())
                    .map(|d| format!(" latest:{}", d.format("%Y-%m-%d")))
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    Span::raw("       "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {}", crate::format_number(*work_tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>2}%"), Style::default().fg(bar_color)),
                    Span::styled(last_used, Style::default().fg(theme::DIM)),
                ]));
                let info_color = if unknown {
                    theme::WARNING
                } else {
                    theme::LABEL_MUTED
                };
                lines.push(Line::from(vec![
                    Span::raw("       "),
                    Span::styled(token_info, Style::default().fg(info_color)),
                ]));
                if let Some(p) = calculator.get_pricing_by_display_name(model) {
                    lines.push(Line::from(vec![
                        Span::raw("       "),
                        Span::styled(
                            format!(
                                "rate/MTok: in:${} out:${} cache_w:${} cache_r:${}",
                                p.input_cost_per_mtok,
                                p.output_cost_per_mtok,
                                p.cache_write_cost_per_mtok,
                                p.cache_read_cost_per_mtok,
                            ),
                            Style::default().fg(theme::DIM),
                        ),
                    ]));
                }
            }
            (title, lines)
        }
        3 => {
            let title = " Tool Usage ".to_string();
            let mut lines: Vec<Line> = vec![Line::from("")];
            let mut tools: Vec<_> = state.stats.tool_usage.iter().collect();
            tools.sort_by(|a, b| b.1.cmp(a.1));
            let max_usage = tools.first().map_or(1, |(_, c)| **c);
            let total_usage: usize = tools.iter().map(|(_, c)| **c).sum();
            let bar_width = 12usize;
            let name_width = content_width.saturating_sub(40);

            for (i, (name, count)) in tools.iter().enumerate().skip(scroll).take(visible_height) {
                let ratio = **count as f64 / max_usage as f64;
                let pct = if total_usage > 0 {
                    (**count as f64 / total_usage as f64 * 100.0) as u32
                } else {
                    0
                };
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = Color::Rgb(
                    (150.0 + 68.0 * intensity) as u8,
                    (180.0 + 38.0 * intensity) as u8,
                    (100.0 + 55.0 * intensity) as u8,
                );
                let display_name = truncate(name, name_width);

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{display_name:<name_width$}"),
                        Style::default().fg(theme::SUCCESS),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {count:>4}"),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                ]));
            }
            (title, lines)
        }
        4 => {
            let title = " Languages ".to_string();
            let mut lines: Vec<Line> = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  File types touched by tool calls (Read, Edit, Write, etc.)",
                    Style::default().fg(theme::DIM),
                )),
                Line::from(""),
            ];

            let mut ext_by_lang: std::collections::HashMap<&str, Vec<(&String, &usize)>> =
                std::collections::HashMap::new();
            let mut other_exts: Vec<(&String, &usize)> = Vec::new();
            for (ext, count) in &state.stats.extension_usage {
                let lang = crate::aggregator::StatsAggregator::language_for_extension(ext);
                if lang == "Other" {
                    other_exts.push((ext, count));
                } else {
                    ext_by_lang.entry(lang).or_default().push((ext, count));
                }
            }
            for exts in ext_by_lang.values_mut() {
                exts.sort_by(|a, b| b.1.cmp(a.1));
            }
            other_exts.sort_by(|a, b| b.1.cmp(a.1));

            let mut known_langs: Vec<_> = state
                .stats
                .language_usage
                .iter()
                .filter(|(lang, _)| lang.as_str() != "Other")
                .collect();
            known_langs.sort_by(|a, b| b.1.cmp(a.1));

            let total_usage: usize = state.stats.language_usage.values().sum();
            let max_count = known_langs
                .first()
                .map_or(1, |(_, c)| **c)
                .max(other_exts.first().map_or(0, |(_, c)| **c));
            let bar_width = 15usize;
            let name_width = content_width.saturating_sub(40);

            enum LangItem<'a> {
                Known(&'a str, usize),
                Unknown(&'a str, usize),
            }
            let mut items: Vec<LangItem> = Vec::new();
            for (lang, count) in &known_langs {
                items.push(LangItem::Known(lang.as_str(), **count));
            }
            for (ext, count) in &other_exts {
                items.push(LangItem::Unknown(ext.as_str(), **count));
            }
            items.sort_by(|a, b| {
                let ca = match a {
                    LangItem::Known(_, c) | LangItem::Unknown(_, c) => *c,
                };
                let cb = match b {
                    LangItem::Known(_, c) | LangItem::Unknown(_, c) => *c,
                };
                cb.cmp(&ca)
            });

            for (rank, item) in items.iter().enumerate().skip(scroll).take(visible_height) {
                let (display_name, count, is_known) = match item {
                    LangItem::Known(lang, c) => ((*lang).to_string(), *c, true),
                    LangItem::Unknown(ext, c) => (format!(".{ext}"), *c, false),
                };
                let ratio = count as f64 / max_count as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = if is_known {
                    (ratio * 0.7 + 0.3).min(1.0)
                } else {
                    (ratio * 0.4 + 0.2).min(0.8)
                };
                let bar_color = Color::Rgb(
                    (40.0 + 46.0 * intensity) as u8,
                    (80.0 + 85.0 * intensity) as u8,
                    (90.0 + 90.0 * intensity) as u8,
                );
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let pct = if total_usage > 0 {
                    (count as f64 / total_usage as f64 * 100.0) as u32
                } else {
                    0
                };
                let name_label = truncate(&display_name, name_width);
                let name_color = if is_known {
                    theme::LABEL_MUTED
                } else {
                    theme::DIM
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:>3}. ", rank + 1),
                        Style::default().fg(theme::DIM),
                    ),
                    Span::styled(
                        format!("{name_label:<name_width$}"),
                        Style::default().fg(name_color),
                    ),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {count:>5}"),
                        Style::default().fg(theme::TEXT_BRIGHT),
                    ),
                    Span::styled(format!(" {pct:>3}%"), Style::default().fg(theme::DIM)),
                ]));

                if is_known
                    && let LangItem::Known(lang, _) = item
                        && let Some(exts) = ext_by_lang.get(lang) {
                            let indent = "        ";
                            let max_line_width = content_width.saturating_sub(2);
                            let mut current_line = String::from(indent);
                            for (j, (ext, c)) in exts.iter().enumerate() {
                                let part = format!(".{ext}({c})");
                                let needed = if current_line.len() == indent.len() {
                                    current_line.len() + part.len()
                                } else {
                                    current_line.len() + 2 + part.len()
                                };
                                if needed > max_line_width && current_line.len() > indent.len() {
                                    lines.push(Line::from(Span::styled(
                                        current_line.clone(),
                                        Style::default().fg(theme::DIM),
                                    )));
                                    current_line = format!("{indent}{part}");
                                } else {
                                    if j > 0 {
                                        current_line.push_str("  ");
                                    }
                                    current_line.push_str(&part);
                                }
                            }
                            if current_line.len() > indent.len() {
                                lines.push(Line::from(Span::styled(
                                    current_line,
                                    Style::default().fg(theme::DIM),
                                )));
                            }
                        }
            }

            if known_langs.is_empty() && other_exts.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  No language data available",
                    Style::default().fg(theme::DIM),
                )));
            }
            (title, lines)
        }
        5 => {
            use chrono::Datelike;
            let title = " Daily Activity ".to_string();
            let mut lines: Vec<Line> = vec![Line::from("")];
            let daily: Vec<_> = state
                .daily_groups
                .iter()
                .map(|group| {
                    let tokens: u64 = group
                        .sessions
                        .iter()
                        .filter(|s| !s.is_subagent)
                        .map(|s| s.day_input_tokens + s.day_output_tokens)
                        .sum();
                    (group.date, tokens)
                })
                .collect();
            let max_tokens = daily.iter().map(|(_, t)| *t).max().unwrap_or(1);
            let bar_width = 15usize;

            let mut prev_month: Option<u32> = None;
            for (i, (date, tokens)) in daily.iter().enumerate().skip(scroll).take(visible_height) {
                let month = date.month();
                if let Some(pm) = prev_month
                    && pm != month {
                        let separator = "─".repeat(content_width.saturating_sub(2));
                        lines.push(Line::from(Span::styled(
                            format!("  {separator}"),
                            Style::default().fg(theme::SEPARATOR),
                        )));
                    }
                prev_month = Some(month);

                let ratio = *tokens as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = Color::Rgb(
                    (80.0 + 100.0 * intensity) as u8,
                    (160.0 + 58.0 * intensity) as u8,
                    (180.0 + 75.0 * intensity) as u8,
                );
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));

                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>3}. ", i + 1), Style::default().fg(theme::DIM)),
                    Span::styled(format!("{}({})", date, date.format("%a")), Style::default().fg(theme::LABEL_MUTED)),
                    Span::raw(" "),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {:>9}", crate::format_number(*tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ]));
            }
            (title, lines)
        }
        6 => {
            let title = " Hourly avg ".to_string();
            let mut lines: Vec<Line> = vec![Line::from("")];
            let mut hourly_total: std::collections::HashMap<u8, u64> =
                std::collections::HashMap::new();
            for group in &state.daily_groups {
                for session in &group.sessions {
                    if session.is_subagent {
                        continue;
                    }
                    for (hour, tokens) in &session.day_hourly_work_tokens {
                        *hourly_total.entry(*hour).or_insert(0) += tokens;
                    }
                }
            }
            let num_days = state.daily_groups.len().max(1) as u64;

            let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
                .iter()
                .map(|(h, t)| (*h, *t / num_days))
                .collect();

            let max_tokens = hourly_avg.values().max().copied().unwrap_or(1);
            let total_avg: u64 = hourly_avg.values().sum();
            let bar_width = 15usize;

            for hour in (0..24u8).skip(scroll).take(visible_height) {
                let tokens = hourly_avg.get(&hour).copied().unwrap_or(0);
                let ratio = tokens as f64 / max_tokens as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = theme::primary_with_intensity(intensity);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
                let pct = if total_avg > 0 {
                    tokens as f64 / total_avg as f64 * 100.0
                } else {
                    0.0
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {:>2}:00-{:>2}:00 ", hour, hour + 1),
                        Style::default().fg(theme::DIM),
                    ),
                    Span::styled(bar, Style::default().fg(bar_color)),
                    Span::styled(
                        format!(" {:>9}", crate::format_number(tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>4.1}%"), Style::default().fg(theme::DIM)),
                ]));
            }
            (title, lines)
        }
        _ => {
            let title = " Unknown ".to_string();
            let lines = vec![Line::from("  No detail view available")];
            (title, lines)
        }
    };

    let scroll_indicator = if can_scroll_up && can_scroll_down {
        " ▲▼ "
    } else if can_scroll_up {
        " ▲ "
    } else if can_scroll_down {
        " ▼ "
    } else {
        ""
    };

    let position_info = if total_items > 0 {
        format!(
            " {}-{}/{} ",
            scroll + 1,
            (scroll + visible_height).min(total_items),
            total_items
        )
    } else {
        String::new()
    };

    let popup = Paragraph::new(content).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::PRIMARY))
            .title(Span::styled(
                title,
                Style::default().fg(theme::PRIMARY).bold(),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(" j/k: scroll  q: close ", Style::default().fg(theme::DIM)),
                Span::styled(scroll_indicator, Style::default().fg(theme::WARNING)),
                Span::styled(position_info, Style::default().fg(theme::DIM)),
            ])),
    );

    frame.render_widget(popup, popup_area);
}
