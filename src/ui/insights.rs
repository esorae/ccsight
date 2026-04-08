use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::AppState;
use super::theme;
use super::{cost_style, weekday_occurrence_count};

pub(super) fn draw_insights(frame: &mut Frame, area: Rect, state: &mut AppState) {
    use chrono::{Datelike, Local, Timelike, Weekday};

    let chunks = Layout::vertical([
        Constraint::Length(4), // Unified metrics (2 rows)
        Constraint::Fill(2),   // Today vs Avg (main, scales with terminal)
        Constraint::Min(9),    // Bottom section (weekly + monthly)
        Constraint::Length(1), // Help
    ])
    .split(area);

    let selected_panel = state.insights_panel;

    // Unified metrics block
    let metrics_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if selected_panel == 0 {
            theme::PRIMARY
        } else {
            theme::BORDER
        }))
        .title(Span::styled(
            " Metrics ",
            Style::default().fg(theme::PRIMARY),
        ));
    let metrics_inner = metrics_block.inner(chunks[0]);
    frame.render_widget(metrics_block, chunks[0]);

    // Calculate all metrics
    let total_sessions: usize = state
        .daily_groups
        .iter()
        .map(|g| g.sessions.iter().filter(|s| !s.is_subagent).count())
        .sum();
    let today = chrono::Local::now().date_naive();
    let first_date = state.daily_groups.iter().map(|g| g.date).min();
    let calendar_days = match first_date {
        Some(first) => (today - first).num_days() as usize + 1,
        _ => 1,
    };
    let avg_cost_per_day = state.total_cost / calendar_days as f64;

    let cache_read = state.stats.total_tokens.cache_read_tokens;
    let input_tokens = state.stats.total_tokens.input_tokens;
    let cache_hit_rate = if input_tokens + cache_read > 0 {
        cache_read as f64 / (input_tokens + cache_read) as f64 * 100.0
    } else {
        0.0
    };

    let total_tool_calls = state.stats.tool_success_count + state.stats.tool_error_count;
    let tool_success_rate = if total_tool_calls > 0 {
        state.stats.tool_success_count as f64 / total_tool_calls as f64 * 100.0
    } else {
        0.0
    };

    let completion_rate = if state.stats.total_sessions_count > 0 {
        state.stats.sessions_with_summary as f64 / state.stats.total_sessions_count as f64 * 100.0
    } else {
        0.0
    };

    let total_work_tokens = state.stats.total_tokens.work_tokens();
    let tokens_per_session = if total_sessions > 0 {
        total_work_tokens / total_sessions as u64
    } else {
        0
    };
    let tokens_per_day = total_work_tokens / calendar_days as u64;

    // 2 rows layout
    let row_chunks =
        Layout::vertical([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).split(metrics_inner);
    let row1_chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(row_chunks[0]);
    let row2_chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(row_chunks[1]);

    let row1_items: [(String, &str, ratatui::style::Color); 4] = [
        (format!("{cache_hit_rate:.1}%"), "cache", theme::SUCCESS),
        (
            format!("{tool_success_rate:.1}%"),
            "success",
            if tool_success_rate >= 90.0 {
                theme::SUCCESS
            } else {
                theme::WARNING
            },
        ),
        (
            format!("{completion_rate:.0}%"),
            "summary",
            if completion_rate >= 80.0 {
                theme::SUCCESS
            } else {
                theme::WARNING
            },
        ),
        (
            format!("${:.1}/day", avg_cost_per_day.max(0.0)),
            "cost",
            theme::SECONDARY,
        ),
    ];

    let row2_items: [(String, &str, ratatui::style::Color); 4] = [
        (
            format!("{}/day", crate::format_number(tokens_per_day)),
            "tokens",
            theme::PRIMARY,
        ),
        (
            format!("{}/ses", crate::format_number(tokens_per_session)),
            "density",
            theme::SECONDARY,
        ),
        (
            format!("{}", state.stats.tool_usage.len()),
            "tools",
            theme::SECONDARY,
        ),
        (format!("{total_sessions}"), "sessions", theme::PRIMARY),
    ];

    for (i, (value, label, color)) in row1_items.iter().enumerate() {
        let card = Paragraph::new(Line::from(vec![
            Span::styled(value.as_str(), Style::default().fg(*color).bold()),
            Span::styled(format!(" {label}"), Style::default().fg(theme::DIM)),
        ]))
        .centered();
        frame.render_widget(card, row1_chunks[i]);
    }

    for (i, (value, label, color)) in row2_items.iter().enumerate() {
        let card = Paragraph::new(Line::from(vec![
            Span::styled(value.as_str(), Style::default().fg(*color).bold()),
            Span::styled(format!(" {label}"), Style::default().fg(theme::DIM)),
        ]))
        .centered();
        frame.render_widget(card, row2_chunks[i]);
    }

    // Today vs Average - Cumulative graph
    let today = Local::now().date_naive();
    let current_hour = Local::now().hour() as u8;

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
    let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
        .iter()
        .map(|(h, t)| (*h, *t / calendar_days as u64))
        .collect();

    let mut today_hourly: std::collections::HashMap<u8, u64> = std::collections::HashMap::new();
    if let Some(today_group) = state.daily_groups.iter().find(|g| g.date == today) {
        for session in today_group.sessions.iter().filter(|s| !s.is_subagent) {
            for (hour, tokens) in &session.day_hourly_work_tokens {
                *today_hourly.entry(*hour).or_insert(0) += tokens;
            }
        }
    }

    // Calculate cumulative values
    let mut today_cumulative = [0u64; 24];
    let mut avg_cumulative = [0u64; 24];
    let mut running_today = 0u64;
    let mut running_avg = 0u64;
    for hour in 0..24u8 {
        running_today += today_hourly.get(&hour).copied().unwrap_or(0);
        running_avg += hourly_avg.get(&hour).copied().unwrap_or(0);
        today_cumulative[hour as usize] = running_today;
        avg_cumulative[hour as usize] = running_avg;
    }

    let today_total = running_today;
    let avg_total = running_avg;
    let max_cumulative = today_total.max(avg_total).max(1);

    let graph_height = chunks[1].height.saturating_sub(4).max(1) as usize;
    let graph_width = chunks[1].width.saturating_sub(12).max(1) as usize;

    let mut graph_lines: Vec<Line> = Vec::new();

    // Y-axis labels and graph rows - line chart style
    let is_top_row = |r: usize| r == graph_height - 1;
    for row in (0..graph_height).rev() {
        let threshold_low = row as f64 / graph_height as f64 * max_cumulative as f64;
        let threshold_high = (row as f64 + 1.0) / graph_height as f64 * max_cumulative as f64;

        let y_label = if row == graph_height - 1 {
            crate::format_number(max_cumulative)
        } else if row == graph_height / 2 {
            crate::format_number(max_cumulative / 2)
        } else if row == 0 {
            "0".to_string()
        } else {
            String::new()
        };

        let mut row_spans: Vec<Span> = Vec::new();
        for col in 0..graph_width {
            let hour = (col * 24 / graph_width).min(23) as u8;
            let is_future = hour > current_hour;
            let today_val = today_cumulative[hour as usize] as f64;
            let avg_val = avg_cumulative[hour as usize] as f64;

            let today_in_row = !is_future
                && today_val >= threshold_low
                && (today_val < threshold_high || is_top_row(row));
            let avg_in_row =
                avg_val >= threshold_low && (avg_val < threshold_high || is_top_row(row));
            let today_below = !is_future && today_val >= threshold_high && !is_top_row(row);
            let avg_below = avg_val >= threshold_high && !is_top_row(row);

            let (ch, color) = if today_in_row && avg_in_row {
                ('●', theme::WARNING)
            } else if today_in_row {
                ('●', theme::SUCCESS)
            } else if avg_in_row {
                ('○', theme::LABEL_MUTED)
            } else if today_below && avg_below {
                ('│', theme::SEPARATOR)
            } else if today_below {
                ('│', theme::HEATMAP_LOW)
            } else if avg_below {
                ('┆', theme::FAINT)
            } else {
                (' ', theme::DIM)
            };
            row_spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        }

        let mut line_spans = vec![
            Span::styled(
                format!("{y_label:>5} "),
                Style::default().fg(theme::LABEL_MUTED),
            ),
            Span::raw("│"),
        ];
        line_spans.extend(row_spans);
        graph_lines.push(Line::from(line_spans));
    }

    // X-axis
    let mut x_axis = String::new();
    x_axis.push_str("      └");
    for _ in 0..graph_width {
        x_axis.push('─');
    }
    graph_lines.push(Line::from(Span::styled(
        x_axis,
        Style::default().fg(theme::DIM),
    )));

    // Hour labels
    let mut hour_labels = "       ".to_string();
    let step = graph_width / 6;
    for i in 0..=6 {
        let hour = i * 4;
        let pos = i * step;
        while hour_labels.len() < 7 + pos {
            hour_labels.push(' ');
        }
        hour_labels.push_str(&format!("{hour:<4}"));
    }
    graph_lines.push(Line::from(Span::styled(
        hour_labels,
        Style::default().fg(theme::LABEL_MUTED),
    )));

    // Current time marker with progress
    let current_pos = (current_hour as usize * graph_width / 24) + 7;
    let day_progress = ((current_hour as f32 + 1.0) / 24.0 * 100.0) as u8;
    let mut marker_spans: Vec<Span> = Vec::new();
    marker_spans.push(Span::raw(" ".repeat(current_pos)));
    marker_spans.push(Span::styled(
        "▲",
        Style::default()
            .fg(theme::PRIMARY)
            .add_modifier(Modifier::BOLD),
    ));
    marker_spans.push(Span::styled(
        format!(" now {current_hour}:00 "),
        Style::default()
            .fg(theme::PRIMARY)
            .add_modifier(Modifier::BOLD),
    ));
    marker_spans.push(Span::styled(
        format!("[{day_progress}%]"),
        Style::default().fg(theme::SUCCESS),
    ));
    graph_lines.push(Line::from(marker_spans));

    let diff_pct = if avg_total > 0 {
        (today_total as f64 / avg_total as f64 * 100.0) as i32
    } else {
        0
    };
    let today_cost = state
        .daily_costs
        .iter()
        .find(|(d, _)| *d == today)
        .map_or(0.0, |(_, c)| *c);

    let today_block = Paragraph::new(graph_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if selected_panel == 1 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Today vs Average (cumulative) ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(" ●", Style::default().fg(theme::SUCCESS)),
                Span::styled("Today", Style::default().fg(theme::SUCCESS)),
                Span::styled("  ○", Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled("Avg", Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled(
                    format!("  │ {}", crate::format_number(today_total)),
                    Style::default()
                        .fg(theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" / {} ", crate::format_number(avg_total)),
                    Style::default().fg(theme::TEXT_BRIGHT),
                ),
                Span::styled(
                    format!("({diff_pct}%) "),
                    Style::default().fg(if diff_pct > 100 {
                        theme::WARNING
                    } else {
                        theme::SUCCESS
                    }),
                ),
                Span::styled(
                    format!(" ${:.2} ", today_cost.max(0.0)),
                    cost_style(today_cost),
                ),
            ])),
    );
    frame.render_widget(today_block, chunks[1]);

    // Bottom section: Weekly and Monthly side by side
    let bottom_chunks =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[2]);

    state.insights_panel_areas = vec![
        chunks[0],        // 0: Metrics
        chunks[1],        // 1: Today vs Average
        bottom_chunks[0], // 2: Weekly
        bottom_chunks[1], // 3: Monthly
    ];

    // Monthly trend (compact)
    let mut monthly_costs: std::collections::BTreeMap<String, f64> =
        std::collections::BTreeMap::new();
    for (date, cost) in &state.daily_costs {
        let month_key = format!("{}-{:02}", date.year(), date.month());
        *monthly_costs.entry(month_key).or_insert(0.0) += cost;
    }
    let col_width = 7usize;
    let max_months = ((bottom_chunks[1].width.saturating_sub(3)) as usize / col_width).max(1);
    let months: Vec<_> = monthly_costs
        .iter()
        .rev()
        .take(max_months)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let max_monthly = months
        .iter()
        .map(|(_, c)| **c)
        .fold(0.0f64, f64::max)
        .max(1.0);

    let mut monthly_lines: Vec<Line> = Vec::new();
    let bar_height = 4usize;

    for row in (0..bar_height).rev() {
        let threshold = (row as f64 + 0.5) / bar_height as f64;
        let mut row_spans: Vec<Span> = vec![Span::raw(" ")];
        for (_, cost) in &months {
            let ratio = **cost / max_monthly;
            let intensity = (ratio * 0.7 + 0.3).min(1.0);
            let color = theme::primary_with_intensity(intensity);
            let bar = if ratio >= threshold { "██" } else { "  " };
            row_spans.push(Span::styled(
                format!("{bar:^col_width$}"),
                Style::default().fg(color),
            ));
        }
        monthly_lines.push(Line::from(row_spans));
    }

    let avg_monthly = if months.is_empty() {
        0.0
    } else {
        months.iter().map(|(_, c)| **c).sum::<f64>() / months.len() as f64
    };

    let mut label_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut cost_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut diff_spans: Vec<Span> = vec![Span::raw(" ")];
    for (month, cost) in &months {
        let short_month = month.split('-').next_back().unwrap_or("??");
        label_spans.push(Span::styled(
            format!("{short_month:^col_width$}"),
            Style::default().fg(theme::LABEL_MUTED),
        ));
        cost_spans.push(Span::styled(
            format!(
                "{:^width$}",
                format!("${:.0}", cost.max(0.0)),
                width = col_width
            ),
            Style::default().fg(theme::WARM),
        ));

        let diff_str = if avg_monthly > 0.0 {
            let pct = ((**cost - avg_monthly) / avg_monthly * 100.0) as i32;
            if pct >= 0 {
                format!("+{pct}%")
            } else {
                format!("{pct}%")
            }
        } else {
            "-".to_string()
        };
        let diff_color = if **cost > avg_monthly {
            theme::WARNING
        } else {
            theme::SUCCESS
        };
        diff_spans.push(Span::styled(
            format!("{diff_str:^col_width$}"),
            Style::default().fg(diff_color),
        ));
    }
    monthly_lines.push(Line::from(label_spans));
    monthly_lines.push(Line::from(cost_spans));
    monthly_lines.push(Line::from(diff_spans));

    let forecast_spans = {
        let now = Local::now();
        let current_month_key = format!("{}-{:02}", now.year(), now.month());
        let days_elapsed = now.day() as f64;
        let days_in_month = if now.month() == 12 {
            chrono::NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
        } else {
            chrono::NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
        }
        .and_then(|d| d.pred_opt())
        .map_or(30.0, |d| d.day() as f64);

        if let Some(current_cost) = monthly_costs.get(&current_month_key) {
            if days_elapsed > 0.0 {
                let forecast = current_cost / days_elapsed * days_in_month;
                vec![
                    Span::styled("this mo: ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("${:.0} est", forecast.max(0.0)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ]
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    };

    let mut bottom_spans = vec![
        Span::styled(" avg: ", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("${:.0}/mo", avg_monthly.max(0.0)),
            Style::default().fg(theme::PRIMARY),
        ),
    ];
    if !forecast_spans.is_empty() {
        bottom_spans.push(Span::styled(" | ", Style::default().fg(theme::DIM)));
        bottom_spans.extend(forecast_spans);
    }
    bottom_spans.push(Span::raw(" "));

    let monthly_block = Paragraph::new(monthly_lines).centered().block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if selected_panel == 3 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Monthly ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(bottom_spans)),
    );
    frame.render_widget(monthly_block, bottom_chunks[1]);

    // Weekly activity (all-time average by weekday)
    let weekdays = [
        (Weekday::Mon, "Mo"),
        (Weekday::Tue, "Tu"),
        (Weekday::Wed, "We"),
        (Weekday::Thu, "Th"),
        (Weekday::Fri, "Fr"),
        (Weekday::Sat, "Sa"),
        (Weekday::Sun, "Su"),
    ];
    let mut weekday_work: std::collections::HashMap<Weekday, u64> =
        std::collections::HashMap::new();
    for group in &state.daily_groups {
        let weekday = group.date.weekday();
        let tokens: u64 = group
            .sessions
            .iter()
            .filter(|s| !s.is_subagent)
            .map(|s| s.day_input_tokens + s.day_output_tokens)
            .sum();
        *weekday_work.entry(weekday).or_insert(0) += tokens;
    }
    let today_weekday = today.weekday();

    let mut weekday_avg: std::collections::HashMap<Weekday, u64> =
        std::collections::HashMap::new();
    let first_date = state
        .daily_groups
        .last()
        .map_or(today, |g| g.date);
    for (wd, _) in &weekdays {
        let count = weekday_occurrence_count(calendar_days, first_date, *wd);
        let tokens = weekday_work.get(wd).copied().unwrap_or(0);
        weekday_avg.insert(*wd, tokens / count as u64);
    }
    let max_weekly = weekday_avg.values().max().copied().unwrap_or(1);
    let total_weekly: u64 = weekday_avg.values().sum();

    let mut weekly_lines: Vec<Line> = Vec::new();
    let bar_width = 8usize;

    for (weekday, label) in &weekdays {
        let avg_tokens = weekday_avg.get(weekday).copied().unwrap_or(0);
        let ratio = avg_tokens as f64 / max_weekly as f64;
        let filled = (ratio * bar_width as f64).round() as usize;
        let pct = if total_weekly > 0 {
            (avg_tokens as f64 / total_weekly as f64 * 100.0) as u32
        } else {
            0
        };
        let intensity = (ratio * 0.7 + 0.3).min(1.0);
        let bar_color = theme::primary_with_intensity(intensity);
        let marker = if *weekday == today_weekday {
            "▶"
        } else {
            " "
        };

        weekly_lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{label} "),
                Style::default().fg(if *weekday == today_weekday {
                    theme::PRIMARY
                } else {
                    theme::LABEL_MUTED
                }),
            ),
            Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
            Span::styled(
                "░".repeat(bar_width - filled),
                Style::default().fg(theme::SEPARATOR),
            ),
            Span::styled(
                format!(" {:>5}", crate::format_number(avg_tokens)),
                Style::default().fg(theme::PRIMARY),
            ),
            Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
        ]));
    }

    let avg_daily_tokens = total_weekly / 7;
    let weekly_block = Paragraph::new(weekly_lines).centered().block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if selected_panel == 2 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Weekly avg ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(vec![
                Span::styled(" avg: ", Style::default().fg(theme::DIM)),
                Span::styled(
                    format!("{}/day ", crate::format_number(avg_daily_tokens)),
                    Style::default().fg(theme::PRIMARY),
                ),
            ])),
    );
    frame.render_widget(weekly_block, bottom_chunks[0]);

    let help_spans = vec![
        Span::styled(" ?", Style::default().fg(theme::PRIMARY)),
        Span::styled(":help ", Style::default().fg(theme::DIM)),
        Span::styled("q", Style::default().fg(theme::PRIMARY)),
        Span::styled(":quit ", Style::default().fg(theme::DIM)),
        Span::styled("←→", Style::default().fg(theme::PRIMARY)),
        Span::styled(":panel ", Style::default().fg(theme::DIM)),
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


pub(super) fn draw_insights_detail_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    let popup_width = 75.min(area.width.saturating_sub(4));
    let popup_height = area.height.saturating_sub(4).min(40);

    let popup_area = Rect {
        x: area.x + (area.width.saturating_sub(popup_width)) / 2,
        y: area.y + (area.height.saturating_sub(popup_height)) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let total_sessions: usize = state
        .daily_groups
        .iter()
        .map(|g| g.sessions.iter().filter(|s| !s.is_subagent).count())
        .sum();
    let today = chrono::Local::now().date_naive();
    let first_date = state.daily_groups.iter().map(|g| g.date).min();
    let calendar_days = match first_date {
        Some(first) => (today - first).num_days() as usize + 1,
        _ => 1,
    };
    let active_days = state.daily_groups.len().max(1);

    let cache_read = state.stats.total_tokens.cache_read_tokens;
    let input_tokens = state.stats.total_tokens.input_tokens;
    let cache_hit_rate = if input_tokens + cache_read > 0 {
        cache_read as f64 / (input_tokens + cache_read) as f64 * 100.0
    } else {
        0.0
    };

    let total_tool_calls = state.stats.tool_success_count + state.stats.tool_error_count;
    let tool_success_rate = if total_tool_calls > 0 {
        state.stats.tool_success_count as f64 / total_tool_calls as f64 * 100.0
    } else {
        0.0
    };

    let completion_rate = if state.stats.total_sessions_count > 0 {
        state.stats.sessions_with_summary as f64 / state.stats.total_sessions_count as f64 * 100.0
    } else {
        0.0
    };

    let avg_cost_per_day = state.total_cost / calendar_days as f64;

    let total_work_tokens = state.stats.total_tokens.work_tokens();
    let tokens_per_session = if total_sessions > 0 {
        total_work_tokens / total_sessions as u64
    } else {
        0
    };
    let tokens_per_day = total_work_tokens / calendar_days as u64;


    let panel_labels = ["metrics", "today vs avg", "weekly", "monthly"];
    let current_panel = state.insights_panel.min(3);
    let panel_label = panel_labels[current_panel];

    let inner_width = popup_width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = vec![Line::from("")];

    match current_panel {
        0 => {
            let w = inner_width.saturating_sub(2);
            let sep = Line::from(Span::styled("─".repeat(w), Style::default().fg(theme::BORDER)));
            let bar_w = (w / 3).min(16);

            // Cost & sessions overview
            lines.push(Line::from(vec![
                Span::styled(" Total Cost  ", Style::default().fg(theme::DIM)),
                Span::styled(super::format_cost(state.total_cost, 2), cost_style(state.total_cost)),
                Span::styled(
                    format!("  ({calendar_days} days, {active_days} active)"),
                    Style::default().fg(theme::DIM),
                ),
            ]));
            let total_duration_mins: i64 = state.daily_groups.iter()
                .flat_map(|g| g.sessions.iter().filter(|s| !s.is_subagent))
                .map(|s| (s.day_last_timestamp - s.day_first_timestamp).num_minutes().max(1))
                .sum();
            let avg_duration_mins = if total_sessions > 0 { total_duration_mins / total_sessions as i64 } else { 0 };
            let avg_dur_str = if avg_duration_mins >= 60 {
                format!("{}h{}m", avg_duration_mins / 60, avg_duration_mins % 60)
            } else {
                format!("{avg_duration_mins}m")
            };
            lines.push(Line::from(vec![
                Span::styled(" Sessions    ", Style::default().fg(theme::DIM)),
                Span::styled(format!("{total_sessions}"), Style::default().fg(theme::SUCCESS).bold()),
                Span::styled(
                    format!("  ${:.1}/day  {}/day  {}/ses  {avg_dur_str}/ses",
                        avg_cost_per_day.max(0.0),
                        crate::format_number(tokens_per_day),
                        crate::format_number(tokens_per_session)),
                    Style::default().fg(theme::DIM),
                ),
            ]));
            lines.push(sep.clone());

            // Rates with descriptive labels
            let rates: [(f64, &str, ratatui::style::Color); 3] = [
                (cache_hit_rate, "Cache Hit Rate   ", theme::SUCCESS),
                (tool_success_rate, "Tool Success Rate",
                    if tool_success_rate >= 90.0 { theme::SUCCESS } else { theme::WARNING }),
                (completion_rate, "Has Summary      ",
                    if completion_rate >= 80.0 { theme::SUCCESS } else { theme::WARNING }),
            ];
            for (rate, label, color) in &rates {
                let filled = (*rate / 100.0 * bar_w as f64).round() as usize;
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label} "), Style::default().fg(theme::DIM)),
                    Span::styled(format!("{rate:>5.1}%"), Style::default().fg(*color).bold()),
                    Span::raw(" "),
                    Span::styled("█".repeat(filled.min(bar_w)), Style::default().fg(*color)),
                    Span::styled("░".repeat(bar_w.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                ]));
            }
            lines.push(sep.clone());

            // Tokens (2 lines, key-value style)
            let output = state.stats.total_tokens.output_tokens;
            let cache_write = state.stats.total_tokens.cache_creation_tokens;
            lines.push(Line::from(vec![
                Span::styled(" Input  ", Style::default().fg(theme::DIM)),
                Span::styled(format!("{:<10}", crate::format_number(input_tokens)), Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled("  Output  ", Style::default().fg(theme::DIM)),
                Span::styled(crate::format_number(output), Style::default().fg(theme::TEXT_BRIGHT)),
            ]));
            lines.push(Line::from(vec![
                Span::styled(" CacheR ", Style::default().fg(theme::DIM)),
                Span::styled(format!("{:<10}", crate::format_number(cache_read)), Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled("  CacheW  ", Style::default().fg(theme::DIM)),
                Span::styled(crate::format_number(cache_write), Style::default().fg(theme::TEXT_BRIGHT)),
            ]));
            lines.push(sep.clone());

            // Models (blue gradient, matching dashboard detail)
            if !state.model_costs.is_empty() {
                let name_w = 12;
                let bar_max = w.saturating_sub(name_w + 18);
                let total_cost: f64 = state.model_costs.iter().map(|(_, c)| *c).sum();
                let max_cost = state.model_costs.iter().map(|(_, c)| *c).fold(0.0f64, f64::max).max(0.01);
                for (model, cost) in state.model_costs.iter().take(5) {
                    let ratio = *cost / max_cost;
                    let pct = if total_cost > 0.0 { (*cost / total_cost * 100.0) as u32 } else { 0 };
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (100.0 + 118.0 * intensity) as u8,
                        (140.0 + 78.0 * intensity) as u8,
                        (200.0 + 55.0 * intensity) as u8,
                    );
                    let name: String = model.chars().take(name_w).collect();
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {name:<name_w$}"), Style::default().fg(theme::PRIMARY)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {:>6}", super::format_cost(*cost, 0)), Style::default().fg(theme::WARM)),
                        Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // Projects (purple gradient, matching dashboard detail)
            if !state.stats.project_stats.is_empty() {
                let name_w = w.saturating_sub(22).min(24);
                let bar_max = 12;
                let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
                projects.sort_by(|a, b| b.1.work_tokens.cmp(&a.1.work_tokens));
                let max_tokens = projects.first().map_or(1, |(_, s)| s.work_tokens);
                for (name, ps) in projects.iter().take(5) {
                    let short = super::shorten_project(name);
                    let display: String = short.chars().take(name_w).collect();
                    let ratio = ps.work_tokens as f64 / max_tokens as f64;
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (140.0 + 78.0 * intensity) as u8,
                        (100.0 + 68.0 * intensity) as u8,
                        (180.0 + 75.0 * intensity) as u8,
                    );
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {display:<name_w$}"), Style::default().fg(theme::SECONDARY)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {:>4}s", ps.sessions), Style::default().fg(theme::DIM)),
                        Span::styled(format!(" {}", crate::format_number(ps.work_tokens)), Style::default().fg(theme::LABEL_MUTED)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // Tools (green gradient, matching dashboard detail)
            if !state.stats.tool_usage.is_empty() {
                let mut tools: Vec<_> = state.stats.tool_usage.iter().collect();
                tools.sort_by(|a, b| b.1.cmp(a.1));
                let max_tool = tools.first().map_or(1, |(_, c)| **c).max(1);
                let total_tool: usize = tools.iter().map(|(_, c)| **c).sum();
                let name_w = 12;
                let bar_max = w.saturating_sub(name_w + 12);
                for (tool, count) in tools.iter().take(6) {
                    let ratio = **count as f64 / max_tool as f64;
                    let pct = if total_tool > 0 { (**count as f64 / total_tool as f64 * 100.0) as u32 } else { 0 };
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (150.0 + 68.0 * intensity) as u8,
                        (180.0 + 38.0 * intensity) as u8,
                        (100.0 + 55.0 * intensity) as u8,
                    );
                    let name: String = tool.chars().take(name_w).collect();
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {name:<name_w$}"), Style::default().fg(theme::SUCCESS)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {count:>5}"), Style::default().fg(theme::LABEL_MUTED)),
                        Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                    ]));
                }
                lines.push(sep.clone());
            }

            // Languages (teal, matching dashboard languages panel)
            if !state.stats.language_usage.is_empty() {
                let mut langs: Vec<_> = state.stats.language_usage.iter().collect();
                langs.sort_by(|a, b| b.1.cmp(a.1));
                let max_lang = langs.first().map_or(1, |(_, c)| **c).max(1);
                let total_lang: usize = langs.iter().map(|(_, c)| **c).sum();
                let name_w = 12;
                let bar_max = w.saturating_sub(name_w + 12);
                for (lang, count) in langs.iter().take(6) {
                    let ratio = **count as f64 / max_lang as f64;
                    let pct = if total_lang > 0 { (**count as f64 / total_lang as f64 * 100.0) as u32 } else { 0 };
                    let filled = (ratio * bar_max as f64).round() as usize;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let bar_color = ratatui::style::Color::Rgb(
                        (40.0 + 46.0 * intensity) as u8,
                        (80.0 + 85.0 * intensity) as u8,
                        (90.0 + 90.0 * intensity) as u8,
                    );
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {name:<name_w$}", name = lang.chars().take(name_w).collect::<String>()), Style::default().fg(theme::LABEL_MUTED)),
                        Span::styled("█".repeat(filled.min(bar_max)), Style::default().fg(bar_color)),
                        Span::styled("░".repeat(bar_max.saturating_sub(filled)), Style::default().fg(theme::SEPARATOR)),
                        Span::styled(format!(" {count:>5}"), Style::default().fg(theme::LABEL_MUTED)),
                        Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                    ]));
                }
            }
        }
        1 => {
            use chrono::{Local, Timelike};
            let today = Local::now().date_naive();
            let current_hour = Local::now().hour() as u8;

            let mut hourly_total: std::collections::HashMap<u8, u64> =
                std::collections::HashMap::new();
            for group in &state.daily_groups {
                for session in group.sessions.iter().filter(|s| !s.is_subagent) {
                    for (hour, tokens) in &session.day_hourly_work_tokens {
                        *hourly_total.entry(*hour).or_insert(0) += tokens;
                    }
                }
            }
            let hourly_avg: std::collections::HashMap<u8, u64> = hourly_total
                .iter()
                .map(|(h, t)| (*h, *t / calendar_days as u64))
                .collect();

            let mut today_hourly: std::collections::HashMap<u8, u64> =
                std::collections::HashMap::new();
            if let Some(today_group) = state.daily_groups.iter().find(|g| g.date == today) {
                for session in today_group.sessions.iter().filter(|s| !s.is_subagent) {
                    for (hour, tokens) in &session.day_hourly_work_tokens {
                        *today_hourly.entry(*hour).or_insert(0) += tokens;
                    }
                }
            }

            let mut today_total = 0u64;
            let mut avg_total = 0u64;
            for hour in 0..=current_hour {
                today_total += today_hourly.get(&hour).copied().unwrap_or(0);
                avg_total += hourly_avg.get(&hour).copied().unwrap_or(0);
            }

            let full_day_avg: u64 = hourly_avg.values().sum();
            let today_cost = state
                .daily_costs
                .iter()
                .find(|(d, _)| *d == today)
                .map_or(0.0, |(_, c)| *c);

            let diff_pct = if avg_total > 0 {
                (today_total as f64 / avg_total as f64 * 100.0) as i32
            } else {
                0
            };

            let mut today_cumulative = [0u64; 24];
            let mut avg_cumulative = [0u64; 24];
            let mut running_today = 0u64;
            let mut running_avg = 0u64;
            for hour in 0..24u8 {
                running_today += today_hourly.get(&hour).copied().unwrap_or(0);
                running_avg += hourly_avg.get(&hour).copied().unwrap_or(0);
                today_cumulative[hour as usize] = running_today;
                avg_cumulative[hour as usize] = running_avg;
            }
            let max_cumulative = running_today.max(running_avg).max(1);

            let graph_height = 8usize;
            let graph_width = inner_width.saturating_sub(10);

            let is_top_row = |r: usize| r == graph_height - 1;
            for row in (0..graph_height).rev() {
                let threshold_low = row as f64 / graph_height as f64 * max_cumulative as f64;
                let threshold_high =
                    (row as f64 + 1.0) / graph_height as f64 * max_cumulative as f64;

                let y_label = if row == graph_height - 1 {
                    crate::format_number(max_cumulative)
                } else if row == graph_height / 2 {
                    crate::format_number(max_cumulative / 2)
                } else if row == 0 {
                    "0".to_string()
                } else {
                    String::new()
                };

                let mut row_spans: Vec<Span> = Vec::new();
                for col in 0..graph_width {
                    let hour = (col * 24 / graph_width).min(23) as u8;
                    let is_future = hour > current_hour;
                    let today_val = today_cumulative[hour as usize] as f64;
                    let avg_val = avg_cumulative[hour as usize] as f64;

                    let today_in_row = !is_future
                        && today_val >= threshold_low
                        && (today_val < threshold_high || is_top_row(row));
                    let avg_in_row =
                        avg_val >= threshold_low && (avg_val < threshold_high || is_top_row(row));
                    let today_below = !is_future && today_val >= threshold_high && !is_top_row(row);
                    let avg_below = avg_val >= threshold_high && !is_top_row(row);

                    let (ch, color) = if today_in_row && avg_in_row {
                        ('●', theme::WARNING)
                    } else if today_in_row {
                        ('●', theme::SUCCESS)
                    } else if avg_in_row {
                        ('○', theme::LABEL_MUTED)
                    } else if today_below && avg_below {
                        ('│', theme::SEPARATOR)
                    } else if today_below {
                        ('│', theme::HEATMAP_LOW)
                    } else if avg_below {
                        ('┆', theme::FAINT)
                    } else {
                        (' ', theme::DIM)
                    };
                    row_spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
                }

                let mut line_spans = vec![
                    Span::styled(
                        format!(" {y_label:>5} "),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                    Span::raw("│"),
                ];
                line_spans.extend(row_spans);
                lines.push(Line::from(line_spans));
            }

            let mut x_axis = String::new();
            x_axis.push_str("       └");
            for _ in 0..graph_width {
                x_axis.push('─');
            }
            lines.push(Line::from(Span::styled(
                x_axis,
                Style::default().fg(theme::DIM),
            )));

            let mut hour_labels = "        ".to_string();
            let step = graph_width / 6;
            for i in 0..=6 {
                let h = i * 4;
                let pos = i * step;
                while hour_labels.len() < 8 + pos {
                    hour_labels.push(' ');
                }
                hour_labels.push_str(&format!("{h:<4}"));
            }
            lines.push(Line::from(Span::styled(
                hour_labels,
                Style::default().fg(theme::LABEL_MUTED),
            )));

            let diff_color = if diff_pct > 100 {
                theme::WARNING
            } else {
                theme::SUCCESS
            };
            lines.push(Line::from(vec![
                Span::styled(" ●", Style::default().fg(theme::SUCCESS)),
                Span::styled(
                    format!("{} ", crate::format_number(today_total)),
                    Style::default().fg(theme::SUCCESS).bold(),
                ),
                Span::styled(" ○", Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled(
                    format!("{} ", crate::format_number(avg_total)),
                    Style::default().fg(theme::TEXT_BRIGHT),
                ),
                Span::styled(format!("{diff_pct}%"), Style::default().fg(diff_color)),
                Span::styled(
                    format!(
                        "  avg: {}/day ${:.2}",
                        crate::format_number(full_day_avg),
                        today_cost.max(0.0)
                    ),
                    Style::default().fg(theme::DIM),
                ),
            ]));
        }
        2 => {
            use chrono::{Datelike, Weekday};
            let weekdays = [
                (Weekday::Mon, "Monday"),
                (Weekday::Tue, "Tuesday"),
                (Weekday::Wed, "Wednesday"),
                (Weekday::Thu, "Thursday"),
                (Weekday::Fri, "Friday"),
                (Weekday::Sat, "Saturday"),
                (Weekday::Sun, "Sunday"),
            ];

            let mut weekday_work: std::collections::HashMap<Weekday, u64> =
                std::collections::HashMap::new();
            for group in &state.daily_groups {
                let weekday = group.date.weekday();
                let tokens: u64 = group
                    .sessions
                    .iter()
                    .filter(|s| !s.is_subagent)
                    .map(|s| s.day_input_tokens + s.day_output_tokens)
                    .sum();
                *weekday_work.entry(weekday).or_insert(0) += tokens;
            }

            let today_weekday = chrono::Local::now().date_naive().weekday();
            let today_date = chrono::Local::now().date_naive();
            let first_date = state
                .daily_groups
                .last()
                .map_or(today_date, |g| g.date);
            let mut weekday_avg: std::collections::HashMap<Weekday, u64> =
                std::collections::HashMap::new();
            let mut max_day = Weekday::Mon;
            let mut max_avg = 0u64;
            for (wd, _) in &weekdays {
                let count = weekday_occurrence_count(calendar_days, first_date, *wd);
                let tokens = weekday_work.get(wd).copied().unwrap_or(0);
                let avg = tokens / count as u64;
                weekday_avg.insert(*wd, avg);
                if avg > max_avg {
                    max_avg = avg;
                    max_day = *wd;
                }
            }

            let max_weekly = weekday_avg.values().max().copied().unwrap_or(1);
            let total_weekly: u64 = weekday_avg.values().sum();
            let bar_width = inner_width.saturating_sub(28);

            for (wd, label) in &weekdays {
                let avg = weekday_avg.get(wd).copied().unwrap_or(0);
                let ratio = avg as f64 / max_weekly as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let pct = if total_weekly > 0 {
                    (avg as f64 / total_weekly as f64 * 100.0) as u32
                } else {
                    0
                };
                let is_today = *wd == today_weekday;
                let marker = if is_today { "▶" } else { " " };
                let intensity = (ratio * 0.7 + 0.3).min(1.0);
                let bar_color = theme::primary_with_intensity(intensity);

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {marker}{label:<9} "),
                        Style::default().fg(if is_today {
                            theme::PRIMARY
                        } else {
                            theme::LABEL_MUTED
                        }),
                    ),
                    Span::styled(
                        "█".repeat(filled.min(bar_width)),
                        Style::default().fg(bar_color),
                    ),
                    Span::styled(
                        "░".repeat(bar_width.saturating_sub(filled)),
                        Style::default().fg(theme::SEPARATOR),
                    ),
                    Span::styled(
                        format!(" {:>5}/d", crate::format_number(avg)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>2}%"), Style::default().fg(theme::DIM)),
                ]));
            }

            lines.push(Line::from(""));
            let avg_daily_tokens = total_weekly / 7;
            if max_avg > 0 {
                let max_label = weekdays
                    .iter()
                    .find(|(wd, _)| *wd == max_day)
                    .map_or("?", |(_, l)| *l);
                lines.push(Line::from(vec![
                    Span::styled(" Most active: ", Style::default().fg(theme::DIM)),
                    Span::styled(max_label, Style::default().fg(theme::SUCCESS).bold()),
                    Span::styled("  avg: ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("{}/day", crate::format_number(avg_daily_tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ]));
            } else {
                lines.push(Line::from(Span::styled(
                    " No activity recorded",
                    Style::default().fg(theme::DIM),
                )));
            }
        }
        3 => {
            use chrono::Datelike;
            let mut monthly_costs: std::collections::BTreeMap<String, f64> =
                std::collections::BTreeMap::new();
            for (date, cost) in &state.daily_costs {
                let month_key = format!("{}-{:02}", date.year(), date.month());
                *monthly_costs.entry(month_key).or_insert(0.0) += cost;
            }

            let col_width = 8usize;
            let visible_months = (inner_width.saturating_sub(2) / col_width).max(1);
            let all_months: Vec<_> = monthly_costs.iter().rev().collect();
            let total_months = all_months.len();
            let max_scroll = total_months.saturating_sub(visible_months);
            if state.insights_detail_scroll > max_scroll {
                state.insights_detail_scroll = max_scroll;
            }
            let skip = total_months
                .saturating_sub(visible_months)
                .saturating_sub(state.insights_detail_scroll);
            let months: Vec<_> = all_months
                .into_iter()
                .rev()
                .skip(skip)
                .take(visible_months)
                .collect();
            let avg_monthly = if months.is_empty() {
                0.0
            } else {
                months.iter().map(|(_, c)| **c).sum::<f64>() / months.len() as f64
            };
            let max_monthly = months
                .iter()
                .map(|(_, c)| **c)
                .fold(0.0f64, f64::max)
                .max(1.0);

            let bar_height = 6usize;

            for row in (0..bar_height).rev() {
                let threshold = (row as f64 + 0.5) / bar_height as f64;
                let mut row_spans: Vec<Span> = vec![Span::raw("  ")];
                for (_, cost) in &months {
                    let ratio = **cost / max_monthly;
                    let intensity = (ratio * 0.7 + 0.3).min(1.0);
                    let color = theme::primary_with_intensity(intensity);
                    let bar = if ratio >= threshold { "██" } else { "  " };
                    row_spans.push(Span::styled(
                        format!("{bar:^col_width$}"),
                        Style::default().fg(color),
                    ));
                }
                lines.push(Line::from(row_spans));
            }

            let mut label_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut cost_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut diff_spans: Vec<Span> = vec![Span::raw("  ")];
            for (month, cost) in &months {
                let short_month = month.split('-').next_back().unwrap_or("??");
                label_spans.push(Span::styled(
                    format!("{short_month:^col_width$}"),
                    Style::default().fg(theme::LABEL_MUTED),
                ));
                cost_spans.push(Span::styled(
                    format!(
                        "{:^width$}",
                        format!("${:.0}", cost.max(0.0)),
                        width = col_width
                    ),
                    Style::default().fg(theme::WARM),
                ));

                let diff_str = if avg_monthly > 0.0 {
                    let pct = ((**cost - avg_monthly) / avg_monthly * 100.0) as i32;
                    if pct >= 0 {
                        format!("+{pct}%")
                    } else {
                        format!("{pct}%")
                    }
                } else {
                    "-".to_string()
                };
                let diff_color = if **cost > avg_monthly {
                    theme::WARNING
                } else {
                    theme::SUCCESS
                };
                diff_spans.push(Span::styled(
                    format!("{diff_str:^col_width$}"),
                    Style::default().fg(diff_color),
                ));
            }
            lines.push(Line::from(label_spans));
            lines.push(Line::from(cost_spans));
            lines.push(Line::from(diff_spans));

            lines.push(Line::from(""));
            let mut summary_spans = vec![
                Span::styled("  avg: ", Style::default().fg(theme::DIM)),
                Span::styled(
                    format!("${:.0}/mo", avg_monthly.max(0.0)),
                    Style::default().fg(theme::PRIMARY),
                ),
            ];
            {
                let now = chrono::Local::now();
                let current_month_key = format!("{}-{:02}", now.year(), now.month());
                let days_elapsed = now.day() as f64;
                let days_in_month = if now.month() == 12 {
                    chrono::NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
                } else {
                    chrono::NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
                }
                .and_then(|d| d.pred_opt())
                .map_or(30.0, |d| d.day() as f64);

                if let Some(current_cost) = monthly_costs.get(&current_month_key)
                    && days_elapsed > 0.0 {
                        let forecast = current_cost / days_elapsed * days_in_month;
                        summary_spans.push(Span::styled(" | ", Style::default().fg(theme::DIM)));
                        summary_spans.push(Span::styled("this mo: ", Style::default().fg(theme::DIM)));
                        summary_spans.push(Span::styled(
                            format!("${:.0} est", forecast.max(0.0)),
                            Style::default().fg(theme::PRIMARY),
                        ));
                    }
            }
            summary_spans.push(Span::styled(" | ", Style::default().fg(theme::DIM)));
            summary_spans.push(Span::styled("total: ", Style::default().fg(theme::DIM)));
            summary_spans.push(Span::styled(
                super::format_cost(state.total_cost, 2),
                Style::default().fg(theme::PRIMARY),
            ));
            lines.push(Line::from(summary_spans));
            if total_months > visible_months {
                lines.push(Line::from(vec![
                    Span::styled("  j/k: scroll months  ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!(
                            "{}-{} of {}",
                            skip + 1,
                            (skip + visible_months).min(total_months),
                            total_months
                        ),
                        Style::default().fg(theme::LABEL_MUTED),
                    ),
                ]));
            }
        }
        _ => {}
    }

    let visible_height = popup_height.saturating_sub(2) as usize;
    if lines.len() < visible_height {
        let pad = (visible_height - lines.len()) / 2;
        let mut padded = vec![Line::from(""); pad];
        padded.append(&mut lines);
        lines = padded;
    }
    let max_scroll = lines.len().saturating_sub(visible_height);
    state.insights_detail_scroll = state.insights_detail_scroll.min(max_scroll);

    let popup = Paragraph::new(lines)
        .scroll((state.insights_detail_scroll as u16, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::PRIMARY))
                .title(Span::styled(
                    format!(" {panel_label} "),
                    Style::default().fg(theme::PRIMARY).bold(),
                ))
                .title_bottom(Line::from(vec![
                    Span::styled(" h/l:switch  i/q:close ", Style::default().fg(theme::DIM)),
                    Span::styled(
                        format!("[{}/4] {} ", current_panel + 1, panel_label),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ])),
        );

    frame.render_widget(popup, popup_area);
}
