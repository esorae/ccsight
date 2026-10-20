use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::cost_total_style;
use super::theme;
use crate::AppState;

/// Derived rates shared by Insights preview card and detail popup —
/// single home so the two surfaces never drift. `total_sessions` /
/// `calendar_days` are passed in (callers reuse them).
struct InsightsMetrics {
    cache_hit_rate: f64,
    cache_5m_share: f64,
    tool_success_rate: f64,
    tokens_per_session: u64,
    tokens_per_day: u64,
}

fn insights_metrics(
    state: &AppState,
    total_sessions: usize,
    calendar_days: usize,
) -> InsightsMetrics {
    let cache_read = state.stats.total_tokens.cache_read_tokens;
    let input_tokens = state.stats.total_tokens.input_tokens;
    let cache_hit_rate = if input_tokens + cache_read > 0 {
        cache_read as f64 / (input_tokens + cache_read) as f64 * 100.0
    } else {
        0.0
    };
    // Share of cache writes that landed in the cheaper 5-minute TTL bucket.
    // 5m costs 1.25× base input; 1h costs 2×. Higher = cheaper writes, which
    // matches the "longer bar = better" convention shared by the other rates.
    let cache_5m = state.stats.total_tokens.cache_creation_5m_tokens;
    let cache_1h = state.stats.total_tokens.cache_creation_1h_tokens;
    let cache_5m_share = if cache_5m + cache_1h > 0 {
        cache_5m as f64 / (cache_5m + cache_1h) as f64 * 100.0
    } else {
        0.0
    };

    let total_tool_calls = state.stats.tool_success_count + state.stats.tool_error_count;
    let tool_success_rate = if total_tool_calls > 0 {
        state.stats.tool_success_count as f64 / total_tool_calls as f64 * 100.0
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

    InsightsMetrics {
        cache_hit_rate,
        cache_5m_share,
        tool_success_rate,
        tokens_per_session,
        tokens_per_day,
    }
}

/// Trailing-N-months `(YYYY-MM, cost)` pairs ending at today, oldest first.
/// Months absent from `monthly_costs` are filled with `0.0` so the chart
/// range agrees with the heatmap (which shows a fixed trailing window) and
/// the inline / popup `Monthly` views share the same denominator.
fn trailing_n_months(
    monthly_costs: &std::collections::BTreeMap<String, f64>,
    n: usize,
) -> Vec<(String, f64)> {
    use chrono::Local;
    trailing_n_months_at(monthly_costs, n, Local::now().date_naive())
}

fn trailing_n_months_at(
    monthly_costs: &std::collections::BTreeMap<String, f64>,
    n: usize,
    now: chrono::NaiveDate,
) -> Vec<(String, f64)> {
    use chrono::Datelike;
    let mut result: Vec<(String, f64)> = Vec::with_capacity(n);
    for i in (0..n).rev() {
        let total_months_back = i as i32;
        let mut year = now.year();
        let mut month = now.month() as i32 - total_months_back;
        while month <= 0 {
            month += 12;
            year -= 1;
        }
        let key = format!("{year}-{month:02}");
        let cost = monthly_costs.get(&key).copied().unwrap_or(0.0);
        result.push((key, cost));
    }
    result
}

fn draw_insights_weekly(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    today: chrono::NaiveDate,
    calendar_days: usize,
) {
    use chrono::{Datelike, Weekday};

    let weekdays = [
        (Weekday::Mon, "Mo"),
        (Weekday::Tue, "Tu"),
        (Weekday::Wed, "We"),
        (Weekday::Thu, "Th"),
        (Weekday::Fri, "Fr"),
        (Weekday::Sat, "Sa"),
        (Weekday::Sun, "Su"),
    ];
    let today_weekday = today.weekday();
    let first_date = state.daily_groups.last().map_or(today, |g| g.date);
    let weekday_avg = super::aggregate_weekday_avg(&state.daily_groups, calendar_days, first_date);
    let max_weekly = weekday_avg.values().max().copied().unwrap_or(1);
    let total_weekly: u64 = weekday_avg.values().sum();

    let mut weekly_lines: Vec<Line> = Vec::new();
    for (weekday, label) in &weekdays {
        let avg_tokens = weekday_avg.get(weekday).copied().unwrap_or(0);
        let ratio = avg_tokens as f64 / max_weekly.max(1) as f64;
        let filled = (ratio * WEEKLY_BAR_W as f64).round() as usize;
        let pct = crate::text::format_pct(avg_tokens, total_weekly);
        let bar_color = theme::primary_with_intensity(theme::bar_intensity(ratio));
        let is_today = *weekday == today_weekday;
        weekly_lines.push(Line::from(vec![
            Span::styled(
                format!("{}{label} ", if is_today { "▶" } else { " " }),
                Style::default().fg(if is_today {
                    theme::PRIMARY
                } else {
                    theme::META
                }),
            ),
            super::bar_fill(filled, WEEKLY_BAR_W, bar_color),
            super::bar_empty(filled, WEEKLY_BAR_W),
            Span::styled(
                format!(" {:>5}", crate::format_number(avg_tokens)),
                Style::default().fg(theme::PRIMARY),
            ),
            Span::styled(format!(" {pct:>3}"), Style::default().fg(theme::META)),
        ]));
    }
    // The seven rows sit mid-panel; the row's height belongs to Monthly.
    let top = usize::from(area.height.saturating_sub(2)).saturating_sub(weekly_lines.len()) / 2;
    weekly_lines.splice(0..0, std::iter::repeat_n(Line::from(""), top));

    // Use `total_tokens / calendar_days` so this footer agrees with the
    // Insights metrics `tokens/day`. Summing seven integer-divided per-weekday
    // averages drifts because each weekday occurrence count is an int and
    // the per-weekday quotient floors away that fractional remainder.
    let avg_daily_tokens = state.stats.total_tokens.work_tokens() / calendar_days as u64;
    let footer = vec![
        Span::styled(" avg: ", Style::default().fg(theme::LABEL)),
        Span::styled(
            format!("{}/day ", crate::format_number(avg_daily_tokens)),
            Style::default().fg(theme::TOKENS),
        ),
    ];

    let weekly_block = Paragraph::new(weekly_lines).centered().block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if state.insights_panel == 1 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Span::styled(
                " Weekly avg ",
                Style::default().fg(theme::PRIMARY),
            ))
            .title_bottom(Line::from(footer)),
    );
    frame.render_widget(weekly_block, area);
}

/// One week's column in the trend: the `MM-DD` label and a two-bar pair.
const WEEK_COL_W: usize = 7;

/// The trailing `n` weeks as columns: cost and token bars, then the week's
/// Monday, cost, tokens and change against the week before. The current week
/// is still under way, so it reads `so far` instead of a change.
fn week_trend_lines(
    state: &AppState,
    today: chrono::NaiveDate,
    n: usize,
    height: usize,
    indent: &'static str,
) -> Vec<Line<'static>> {
    let weeks =
        crate::aggregator::trailing_weeks(&state.daily_groups, &state.daily_costs, today, n);
    let heights = share_heights(
        &weeks.iter().map(|w| w.cost).collect::<Vec<_>>(),
        &weeks.iter().map(|w| w.tokens).collect::<Vec<_>>(),
    );
    let bar_height = height.saturating_sub(4).max(1);
    let col = WEEK_COL_W;
    let mut lines = Vec::new();
    for row in (0..bar_height).rev() {
        let mut spans = vec![Span::raw(indent)];
        for &(cost_h, token_h) in &heights {
            spans.extend(month_bar_pair(cost_h, token_h, row, bar_height, col));
        }
        lines.push(Line::from(spans));
    }
    let mut labels = vec![Span::raw(indent)];
    let mut costs = vec![Span::raw(indent)];
    let mut tokens = vec![Span::raw(indent)];
    let mut changes = vec![Span::raw(indent)];
    for (i, w) in weeks.iter().enumerate() {
        let end = w.start + chrono::Duration::days(7);
        let unpriced = super::any_unpriced(&state.daily_groups, |g, _| {
            g.date >= w.start && g.date < end
        });
        labels.push(Span::styled(
            format!("{:^col$}", w.start.format("%m-%d").to_string()),
            Style::default().fg(theme::TIME),
        ));
        costs.push(Span::styled(
            format!("{:^col$}", super::format_cost_marked(w.cost, unpriced, 0)),
            cost_total_style(unpriced),
        ));
        tokens.push(Span::styled(
            format!("{:^col$}", crate::format_number(w.tokens)),
            Style::default().fg(theme::TOKENS),
        ));
        let change = if i + 1 == weeks.len() {
            "so far".to_string()
        } else if i == 0 {
            "-".to_string()
        } else {
            vs_average(w.cost, weeks[i - 1].cost)
        };
        changes.push(Span::styled(
            format!("{change:^col$}"),
            Style::default().fg(theme::META),
        ));
    }
    lines.extend([labels, costs, tokens, changes].map(Line::from));
    lines
}

/// Weekday (Monday first) by clock-hour grid of average work tokens, two
/// columns per hour, shaded against the busiest cell.
fn weekday_hour_lines(state: &AppState, today: chrono::NaiveDate) -> Vec<Line<'static>> {
    use super::dashboard::{BLOCK_RAMP, SPARK_CHARS};
    const LABELS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
    let grid = crate::aggregator::weekday_hour_avg(&state.daily_groups, today);
    let max = grid.iter().flatten().copied().max().unwrap_or(0).max(1);
    let mut axis = " ".repeat(48);
    for (hour, label) in [(0, "0"), (6, "6"), (12, "12"), (18, "18")] {
        axis.replace_range(hour * 2..hour * 2 + label.len(), label);
    }
    let mut lines = vec![Line::from(vec![
        Span::raw("     "),
        Span::styled(axis, Style::default().fg(theme::TIME)),
    ])];
    for (label, row) in LABELS.iter().zip(grid) {
        let mut spans = vec![Span::styled(
            format!("  {label} "),
            Style::default().fg(theme::META),
        )];
        for v in row {
            if v == 0 {
                spans.push(Span::styled(
                    format!("{} ", SPARK_CHARS[0]),
                    Style::default().fg(theme::SEPARATOR),
                ));
                continue;
            }
            let ratio = v as f64 / max as f64;
            let top = BLOCK_RAMP.len() - 1;
            let c = BLOCK_RAMP[((ratio * top as f64).ceil() as usize).min(top)];
            spans.push(Span::styled(
                format!("{c}{c}"),
                Style::default().fg(theme::primary_with_intensity(theme::bar_intensity(ratio))),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Row `row` (0 = bottom) of a month's column in a `height`-row bar chart.
/// The top cell takes a partial block, and any non-zero month keeps a stub
/// on row 0, so a small month never vanishes next to the tallest one.
fn month_bar_cell(ratio: f64, row: usize, height: usize) -> &'static str {
    let frac = ratio * height as f64 - row as f64;
    if frac >= 1.0 {
        "██"
    } else if frac >= 0.75 {
        "▆▆"
    } else if frac >= 0.5 {
        "▄▄"
    } else if frac >= 0.25 {
        "▂▂"
    } else if frac > 0.0 && row == 0 {
        "▁▁"
    } else {
        "  "
    }
}

/// One column's cost bar and token bar side by side, centred in `width`;
/// heights come from [`share_heights`].
fn month_bar_pair(
    cost_ratio: f64,
    token_ratio: f64,
    row: usize,
    height: usize,
    width: usize,
) -> [Span<'static>; 5] {
    const PAIR: usize = 5;
    let left = width.saturating_sub(PAIR) / 2;
    let right = width.saturating_sub(PAIR + left);
    [
        Span::raw(" ".repeat(left)),
        Span::styled(
            month_bar_cell(cost_ratio, row, height),
            Style::default().fg(theme::COST_TOTAL),
        ),
        Span::raw(" "),
        Span::styled(
            month_bar_cell(token_ratio, row, height),
            Style::default().fg(theme::TOKENS),
        ),
        Span::raw(" ".repeat(right)),
    ]
}

/// Bar heights for paired cost / token columns: each column's share of its
/// series' total, on one scale whose top is the largest share in either
/// series. Equal bars mean cost and tokens moved together; a taller cost bar
/// marks a column whose tokens came dear. Own-max scaling would not compare.
fn share_heights(costs: &[f64], tokens: &[u64]) -> Vec<(f64, f64)> {
    let cost_total: f64 = costs.iter().sum();
    let token_total = tokens.iter().sum::<u64>() as f64;
    let share = |v: f64, total: f64| if total > 0.0 { v / total } else { 0.0 };
    let shares: Vec<(f64, f64)> = costs
        .iter()
        .zip(tokens)
        .map(|(c, t)| (share(*c, cost_total), share(*t as f64, token_total)))
        .collect();
    let top = shares
        .iter()
        .flat_map(|(c, t)| [*c, *t])
        .fold(0.0f64, f64::max);
    if top <= 0.0 {
        return vec![(0.0, 0.0); shares.len()];
    }
    shares
        .into_iter()
        .map(|(c, t)| (c / top, t / top))
        .collect()
}

/// Names the two bars of [`month_bar_pair`] in their colors.
fn month_legend() -> [Span<'static>; 4] {
    [
        Span::styled("██", Style::default().fg(theme::COST_TOTAL)),
        Span::styled(" cost  ", Style::default().fg(theme::LABEL)),
        Span::styled("██", Style::default().fg(theme::TOKENS)),
        Span::styled(" tokens ", Style::default().fg(theme::LABEL)),
    ]
}

/// `YYYY-MM` → `YY-MM`: the year stays, so a year boundary is unambiguous.
pub(super) fn short_month(month: &str) -> String {
    month.split_once('-').map_or_else(
        || "??".to_string(),
        |(y, m)| format!("{}-{m}", y.get(2..).unwrap_or(y)),
    )
}

/// A month's cost centred in `width`, marked when one of its models is unpriced.
fn month_cost_span(state: &AppState, month: &str, cost: f64, width: usize) -> Span<'static> {
    let unpriced = super::month_unpriced(&state.daily_groups, month);
    Span::styled(
        format!("{:^width$}", super::format_cost_marked(cost, unpriced, 0)),
        cost_total_style(unpriced),
    )
}

/// Cost and tokens per month over the trailing twelve, idle months included,
/// so the panel, the detail popup and each month's `vs avg` share one figure
/// however much older history the popup can scroll to.
fn trailing_year_average(
    monthly_costs: &std::collections::BTreeMap<String, f64>,
    monthly_tokens: &std::collections::BTreeMap<String, u64>,
) -> (f64, u64) {
    let window = trailing_n_months(monthly_costs, 12);
    if window.is_empty() {
        return (0.0, 0);
    }
    let cost: f64 = window.iter().map(|(_, c)| *c).sum();
    let tokens: u64 = window
        .iter()
        .map(|(m, _)| monthly_tokens.get(m).copied().unwrap_or(0))
        .sum();
    (cost / window.len() as f64, tokens / window.len() as u64)
}

/// A month's work tokens centred in `width`.
fn month_tokens_span(
    monthly_tokens: &std::collections::BTreeMap<String, u64>,
    month: &str,
    width: usize,
) -> Span<'static> {
    let tokens = monthly_tokens.get(month).copied().unwrap_or(0);
    Span::styled(
        format!("{:^width$}", crate::format_number(tokens)),
        Style::default().fg(theme::TOKENS),
    )
}

/// A month's `vs avg` cell. The current month is still under way, so it
/// reads `so far` rather than a share of a full month's average — the same
/// rule the Weekly trend applies to the current week.
fn month_vs_average(month: &str, cost: f64, avg: f64) -> String {
    let current = chrono::Local::now()
        .date_naive()
        .format("%Y-%m")
        .to_string();
    if month == current {
        "so far".to_string()
    } else {
        vs_average(cost, avg)
    }
}

/// Signed percentage against the trailing average. Callers color it neutrally:
/// a busy month is more activity, which is an overspend only under API billing.
fn vs_average(cost: f64, avg: f64) -> String {
    if avg > 0.0 {
        let pct = ((cost - avg) / avg * 100.0) as i32;
        if pct >= 0 {
            format!("+{pct}%")
        } else {
            format!("{pct}%")
        }
    } else {
        "-".to_string()
    }
}

/// `this mo: $X · forecast: $Y` for the current month, or nothing before its
/// first cost. The forecast is left off on the month's last day, where it
/// equals the running figure.
fn this_month_spans(
    state: &AppState,
    monthly_costs: &std::collections::BTreeMap<String, f64>,
) -> Vec<Span<'static>> {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let key = today.format("%Y-%m").to_string();
    let Some(&current) = monthly_costs.get(&key) else {
        return Vec::new();
    };
    let days_elapsed = today.day() as f64;
    let days_in_month = crate::aggregator::days_in_month(today) as f64;
    let unpriced = super::month_unpriced(&state.daily_groups, &key);
    let mut spans = vec![
        Span::styled("this mo: ", Style::default().fg(theme::LABEL)),
        Span::styled(
            super::format_cost_marked(current, unpriced, 0),
            cost_total_style(unpriced),
        ),
    ];
    if days_elapsed < days_in_month {
        let forecast = current / days_elapsed * days_in_month;
        spans.push(Span::styled(
            " · forecast: ",
            Style::default().fg(theme::LABEL),
        ));
        spans.push(Span::styled(
            super::format_cost_marked(forecast, unpriced, 0),
            cost_total_style(unpriced),
        ));
    }
    spans
}

fn draw_insights_monthly(frame: &mut Frame, area: Rect, state: &AppState) {
    let monthly_costs = super::aggregate_monthly_costs(&state.daily_costs);
    // Trailing 12 months including zero-cost months so the range mirrors the
    // dashboard heatmap's trailing-year window. Avg divides by 12 (the
    // window size, not just months that happen to have data).
    let trailing_window = trailing_n_months(&monthly_costs, 12);
    let monthly_tokens = super::aggregate_monthly_tokens(&state.daily_groups);
    let (avg_monthly, avg_monthly_tokens) = trailing_year_average(&monthly_costs, &monthly_tokens);
    let col_width = 7usize;
    let max_months = ((area.width.saturating_sub(3)) as usize / col_width).max(1);
    // Show the most recent `max_months` of the trailing window so a narrow
    // panel keeps the right edge anchored at "this month".
    let months: Vec<(String, f64)> = trailing_window
        .iter()
        .rev()
        .take(max_months)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let months_view: Vec<(&String, &f64)> = months.iter().map(|(k, v)| (k, v)).collect();

    let mut monthly_lines: Vec<Line> = Vec::new();
    // The bars take what the four text rows (month, cost, tokens, vs avg) leave.
    let bar_height = (area.height.saturating_sub(2) as usize)
        .saturating_sub(4)
        .max(2);

    let heights = share_heights(
        &months_view.iter().map(|(_, c)| **c).collect::<Vec<_>>(),
        &months_view
            .iter()
            .map(|(m, _)| monthly_tokens.get(*m).copied().unwrap_or(0))
            .collect::<Vec<_>>(),
    );
    for row in (0..bar_height).rev() {
        let mut row_spans: Vec<Span> = vec![Span::raw(" ")];
        for &(cost_h, token_h) in &heights {
            row_spans.extend(month_bar_pair(cost_h, token_h, row, bar_height, col_width));
        }
        monthly_lines.push(Line::from(row_spans));
    }

    let mut label_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut cost_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut tok_spans: Vec<Span> = vec![Span::raw(" ")];
    let mut diff_spans: Vec<Span> = vec![Span::raw(" ")];
    for (month, cost) in &months_view {
        label_spans.push(Span::styled(
            format!("{:^col_width$}", short_month(month)),
            Style::default().fg(theme::TIME),
        ));
        cost_spans.push(month_cost_span(state, month, **cost, col_width));
        tok_spans.push(month_tokens_span(&monthly_tokens, month, col_width));
        diff_spans.push(Span::styled(
            format!(
                "{:^col_width$}",
                month_vs_average(month, **cost, avg_monthly)
            ),
            Style::default().fg(theme::META),
        ));
    }
    monthly_lines.push(Line::from(label_spans));
    monthly_lines.push(Line::from(cost_spans));
    monthly_lines.push(Line::from(tok_spans));
    monthly_lines.push(Line::from(diff_spans));

    let forecast_spans = this_month_spans(state, &monthly_costs);

    let mut bottom_spans = vec![
        Span::styled(" avg: ", Style::default().fg(theme::LABEL)),
        Span::styled(
            format!(
                "{}/mo",
                super::format_cost_marked(avg_monthly, !state.models_without_pricing.is_empty(), 0)
            ),
            cost_total_style(!state.models_without_pricing.is_empty()),
        ),
        Span::styled(
            format!(" {}/mo", crate::format_number(avg_monthly_tokens)),
            Style::default().fg(theme::TOKENS),
        ),
    ];
    if !forecast_spans.is_empty() {
        bottom_spans.push(Span::styled(" | ", Style::default().fg(theme::LABEL)));
        bottom_spans.extend(forecast_spans);
    }
    bottom_spans.push(Span::raw(" "));

    let monthly_block = Paragraph::new(monthly_lines).centered().block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if state.insights_panel == 2 {
                theme::PRIMARY
            } else {
                theme::BORDER
            }))
            .title(Line::from(
                [Span::styled(
                    " Monthly ",
                    Style::default().fg(theme::PRIMARY),
                )]
                .into_iter()
                .chain(month_legend())
                .collect::<Vec<_>>(),
            ))
            .title_bottom(Line::from(bottom_spans)),
    );
    frame.render_widget(monthly_block, area);
}

fn draw_insights_metrics(
    frame: &mut Frame,
    area: Rect,
    state: &AppState,
    total_sessions: usize,
    calendar_days: usize,
    card_height: u16,
    extra: Vec<Line<'static>>,
) {
    // Unified metrics block
    let metrics_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.insights_panel == 0 {
            theme::PRIMARY
        } else {
            theme::BORDER
        }))
        .title(Span::styled(
            " Metrics ",
            Style::default().fg(theme::PRIMARY),
        ));
    let inner = metrics_block.inner(area);
    frame.render_widget(metrics_block, area);
    let card_rows = card_height.saturating_sub(2);
    let [metrics_inner, trend_area] =
        Layout::vertical([Constraint::Length(card_rows), Constraint::Fill(1)]).areas(inner);
    frame.render_widget(Paragraph::new(extra), trend_area);

    let avg_cost_per_day = state.total_cost / calendar_days as f64;

    let InsightsMetrics {
        cache_hit_rate,
        cache_5m_share,
        tool_success_rate,
        tokens_per_session,
        tokens_per_day,
    } = insights_metrics(state, total_sessions, calendar_days);
    // Raw cache-write split is also shown verbatim on the card below.
    let cache_5m = state.stats.total_tokens.cache_creation_5m_tokens;
    let cache_1h = state.stats.total_tokens.cache_creation_1h_tokens;

    // Row layout: 2 metric cards rows + MCP activation + Adoption [+ Pricing gap]
    // Pricing gap row is rendered only when at least one model lacks pricing (see
    // `metrics_height` above). Extra `Length(1)` below is harmless if unused.
    let row_constraints: Vec<Constraint> = (0..card_rows).map(|_| Constraint::Length(1)).collect();
    let row_chunks = Layout::vertical(row_constraints).split(metrics_inner);
    let row1_chunks = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .split(row_chunks[0]);
    let row2_chunks = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(row_chunks[1]);

    // Each card names its figure in words; a card too narrow for that falls
    // back to the short label.
    let row1_items: [(String, &str, &str, ratatui::style::Color); 3] = [
        (
            format!("{cache_hit_rate:.1}%"),
            "cache hit",
            "cache",
            theme::SUCCESS,
        ),
        (
            format!("{tool_success_rate:.1}%"),
            "tool calls succeeded",
            "ok",
            if tool_success_rate >= 90.0 {
                theme::SUCCESS
            } else {
                theme::WARNING
            },
        ),
        (
            format!("${:.2}", avg_cost_per_day.max(0.0)),
            "per day",
            "/day",
            theme::VALUE,
        ),
    ];
    let row2_items: [(String, &str, &str, ratatui::style::Color); 4] = [
        (
            crate::format_number(tokens_per_day),
            "tokens per day",
            "tok/day",
            theme::PRIMARY,
        ),
        (
            crate::format_number(tokens_per_session),
            "tokens per session",
            "tok/ses",
            theme::VALUE,
        ),
        (
            // Distinct tools, skills, commands, subagents and MCP tools
            // invoked in this view: a count of kinds, not of calls.
            format!("{}", state.stats.tool_usage.len()),
            "different tools used",
            "tools",
            theme::VALUE,
        ),
        (
            format!("{total_sessions}"),
            "sessions",
            "sessions",
            theme::PRIMARY,
        ),
    ];
    let card_spans = |value: &str, long: &str, short: &str, color, width: u16| {
        let fits = value.chars().count() + 1 + long.chars().count() <= usize::from(width);
        vec![
            Span::styled(value.to_string(), Style::default().fg(color).bold()),
            Span::styled(
                format!(" {}", if fits { long } else { short }),
                Style::default().fg(theme::LABEL),
            ),
        ]
    };

    for (i, (value, long, short, color)) in row1_items.iter().enumerate() {
        let width = row1_chunks[i].width;
        let mut spans = card_spans(value, long, short, *color, width);
        // The cache card adds how its writes split by lifetime, so "how much
        // hits" and "how long it lasts" read as one unit. Skipped when nothing
        // was written to cache, or the card has no room for it.
        if i == 0 && (cache_5m + cache_1h) > 0 {
            let split = format!(" (5-min writes {cache_5m_share:.0}%)");
            if super::spans_width(&spans) + split.chars().count() <= usize::from(width) {
                spans.push(Span::styled(split, Style::default().fg(theme::META)));
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).centered(), row1_chunks[i]);
    }

    for (i, (value, long, short, color)) in row2_items.iter().enumerate() {
        let spans = card_spans(value, long, short, *color, row2_chunks[i].width);
        frame.render_widget(Paragraph::new(Line::from(spans)).centered(), row2_chunks[i]);
    }

    // Ecosystem row — `N/M MCP active · K stale  ·  Used: MCP X · Skills Y · Subagents Z`.
    // Per-server stale list with day-since-use lives in the Metrics detail popup (`i`).
    // The Used counts are absolute (not %) because the three categories have different
    // invocation semantics (1 Skill = context load; 1 Subagent = child session; 1 MCP
    // call = external request) so cross-category percentages would mislead.
    let stale_at = state.stale_reference();
    let configured_count = state.mcp_status.iter().filter(|s| s.configured).count();
    let active_count = state
        .mcp_status
        .iter()
        .filter(|s| s.configured && !s.is_underutilized(stale_at, 30))
        .count();
    let stale_count = state
        .mcp_status
        .iter()
        .filter(|s| s.is_underutilized(stale_at, 30))
        .count();
    let row_width = row_chunks[2].width as usize;
    let counts = EcoRowCounts {
        active: active_count,
        configured: configured_count,
        stale: stale_count,
        mcp: state.stats.sessions_using_mcp,
        skills: state.stats.sessions_using_skills,
        commands: state.stats.sessions_using_commands,
        subagents: state.stats.sessions_using_subagents,
    };
    let eco_spans = [EcoWording::Explained, EcoWording::Long]
        .into_iter()
        .map(|w| eco_row(&counts, w))
        .find(|spans| super::spans_width(spans) <= row_width)
        .unwrap_or_else(|| eco_row(&counts, EcoWording::Short));
    frame.render_widget(
        Paragraph::new(Line::from(eco_spans)).centered(),
        row_chunks[2],
    );

    // Pricing gap row (conditional) — shown only when one or more models used
    // in the view lack a pricing entry. Surfaces the silent-$0 condition that
    // would otherwise hide inside the Models detail popup.
    if !state.models_without_pricing.is_empty()
        && let Some(gap_row) = row_chunks.get(3)
    {
        let count = state.models_without_pricing.len();
        let mut names: Vec<&String> = state.models_without_pricing.iter().collect();
        names.sort();
        let preview: String = names
            .iter()
            .take(3)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if names.len() > 3 {
            format!(" +{} more", names.len() - 3)
        } else {
            String::new()
        };
        let gap_spans = vec![
            Span::styled(
                "⚠ Pricing gap  ",
                Style::default().fg(theme::WARNING).bold(),
            ),
            Span::styled(
                format!("{count} models"),
                Style::default().fg(theme::WARNING),
            ),
            Span::styled(" · ", Style::default().fg(theme::LABEL)),
            Span::styled(preview, Style::default().fg(theme::LABEL)),
            Span::styled(suffix, Style::default().fg(theme::LABEL)),
        ];
        frame.render_widget(Paragraph::new(Line::from(gap_spans)).centered(), *gap_row);
    }
}

/// Counts behind the Insights Ecosystem row.
pub(super) struct EcoRowCounts {
    pub active: usize,
    pub configured: usize,
    pub stale: usize,
    pub mcp: usize,
    pub skills: usize,
    pub commands: usize,
    pub subagents: usize,
}

/// How much an ecosystem row spells out: `Explained` adds what "used" and
/// "stale" are judged by, `Short` abbreviates every label.
#[derive(Clone, Copy, Debug, PartialEq)]
enum EcoWording {
    Explained,
    Long,
    Short,
}

/// Ecosystem summary row. The short form drops label words but keeps every
/// number, so a narrow terminal shows the Subagent count instead of clipping
/// it. Composed apart from drawing so the caller measures these spans, not a
/// second copy of the labels. Numbers are session counts — the Dashboard
/// Ecosystem panel counts calls under similar labels.
fn eco_row(c: &EcoRowCounts, wording: EcoWording) -> Vec<Span<'static>> {
    let long = wording != EcoWording::Short;
    let explained = wording == EcoWording::Explained;
    let label_style = Style::default().fg(theme::LABEL);
    let mut spans = vec![
        Span::styled(
            format!("{}/{}", c.active, c.configured),
            Style::default().fg(theme::PRIMARY).bold(),
        ),
        Span::styled(
            if explained {
                " MCP servers used in 30d"
            } else if long {
                " MCP used"
            } else {
                " MCP"
            },
            label_style,
        ),
    ];
    if c.stale > 0 {
        spans.push(Span::styled(" · ", label_style));
        spans.push(Span::styled(
            if explained {
                format!("{} stale (unused 30d+)", c.stale)
            } else if long {
                format!("{} stale", c.stale)
            } else {
                format!("{}⚠", c.stale)
            },
            Style::default().fg(theme::WARNING),
        ));
    }
    spans.push(Span::styled(
        if long {
            "    Sessions using  "
        } else {
            "  ses: "
        },
        label_style,
    ));
    // Order matches the popup tabs: MCP → Skills → Commands → Subagents.
    let cats = [
        (if long { "MCP " } else { "M" }, c.mcp, theme::CAT_MCP),
        (
            if long { "Skills " } else { "Sk" },
            c.skills,
            theme::CAT_SKILLS,
        ),
        (
            if long { "Commands " } else { "Cm" },
            c.commands,
            theme::CAT_COMMANDS,
        ),
        (
            if long { "Subagents " } else { "Sb" },
            c.subagents,
            theme::CAT_SUBAGENTS,
        ),
    ];
    for (i, (label, n, color)) in cats.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", label_style));
        }
        spans.push(Span::styled(*label, Style::default().fg(*color)));
        spans.push(Span::styled(
            format!("{n}"),
            Style::default().fg(*color).bold(),
        ));
    }
    spans
}

/// Panels on the Insights tab, in `insights_panel` order: Metrics, Weekly,
/// Monthly. The detail popup's label table is sized by it.
pub(crate) const INSIGHTS_PANELS: usize = 3;

/// Cells in a Weekly row's bar.
const WEEKLY_BAR_W: usize = 8;
/// The Weekly panel's width: a row (`▶Fr `, bar, average, share), a margin
/// column each side, borders.
pub(super) const WEEKLY_W: u16 = (4 + WEEKLY_BAR_W + 6 + 4) as u16 + 4;

/// The Weekly / Monthly row's least height: Weekly's seven rows with a blank
/// above and below, inside borders.
const BOTTOM_ROW_H: u16 = 7 + 2 + 2;

pub(super) fn draw_insights(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // Card rows: 2 KPI rows + 1 ecosystem row, plus 1 when any model lacks
    // pricing.
    let card_height: u16 = if state.models_without_pricing.is_empty() {
        5
    } else {
        6
    };
    // Metrics is sized to its content, so rows its steps cannot use go to
    // Weekly / Monthly instead of a blank band inside Metrics.
    let today = chrono::Local::now().date_naive();
    let room = area.height.saturating_sub(BOTTOM_ROW_H + 1 + card_height);
    let extra = metrics_extra_lines(
        state,
        today,
        area.width.saturating_sub(2),
        usize::from(room),
    );
    let metrics_height = card_height + extra.len() as u16;
    let chunks = Layout::vertical([
        Constraint::Length(metrics_height), // Cards [+ Pricing gap], then trends / work / totals
        Constraint::Min(BOTTOM_ROW_H),      // Weekly + Monthly, taking any rows left
        Constraint::Length(1),              // Help
    ])
    .split(area);

    let total_sessions = state.stats.total_sessions_count;
    let calendar_days = state.calendar_days(today);

    draw_insights_metrics(
        frame,
        chunks[0],
        state,
        total_sessions,
        calendar_days,
        card_height,
        extra,
    );

    // Bottom section: Weekly and Monthly side by side
    let bottom_chunks =
        Layout::horizontal([Constraint::Length(WEEKLY_W), Constraint::Fill(1)]).split(chunks[1]);

    state.layout.insights_panel_areas = vec![
        chunks[0],        // 0: Metrics
        bottom_chunks[0], // 1: Weekly
        bottom_chunks[1], // 2: Monthly
    ];

    // Monthly trend (compact)
    draw_insights_monthly(frame, bottom_chunks[1], state);

    // Weekly activity (all-time average by weekday)
    draw_insights_weekly(frame, bottom_chunks[0], state, today, calendar_days);

    // No ↑↓ entry: the Insights main view has nothing to scroll — reading
    // depth lives in the detail popup (Enter), which has full scroll keys.
    let help_line = Paragraph::new(super::tab_help_bar(
        state,
        &[
            ("?", "help"),
            ("q", "quit"),
            ("←→", "panel"),
            ("Enter", "detail"),
            ("/", "search"),
            ("m", "pins"),
        ],
    ));
    frame.render_widget(help_line, chunks[2]);
}

/// Columns an MCP row keeps after the name: its indent, the gap and the
/// longest detail (`N calls, yesterday`).
const MCP_ROW_RESERVED: usize = 3 + 1 + 22;

/// A server name in a column sized to the popup, cut with `…` so it never
/// runs into the figures after it nor pushes them off a narrow popup.
fn mcp_name_cell(name: &str, popup_w: usize) -> String {
    let w = popup_w.saturating_sub(MCP_ROW_RESERVED).clamp(12, 28);
    let cut = super::truncate_with_ellipsis(name, w);
    let pad = w.saturating_sub(unicode_width::UnicodeWidthStr::width(cut.as_str()));
    format!("{cut}{} ", " ".repeat(pad))
}

fn insights_detail_metrics(
    state: &AppState,
    calendar_days: usize,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let total_sessions = state.stats.total_sessions_count;
    // Distinct subagent sessions (dedup by file_path), paired with the distinct
    // user total above — a plain count would be subagent session-days.
    let subagent_sessions = state
        .daily_groups
        .iter()
        .flat_map(|g| g.sessions.iter())
        .filter(|s| s.is_subagent)
        .map(|s| &s.file_path)
        .collect::<std::collections::HashSet<_>>()
        .len();
    // Tokens + cost attributable to subagents: the totals include them,
    // so their share is named next to the totals it sits inside.
    let (subagent_work_tokens, subagent_cost) = subagent_spend(state);
    let active_days = state.active_days().max(1);

    let InsightsMetrics {
        cache_hit_rate,
        cache_5m_share,
        tool_success_rate,
        tokens_per_session,
        tokens_per_day,
    } = insights_metrics(state, total_sessions, calendar_days);
    // Raw input / cache-read totals are also shown verbatim in this popup.
    let cache_read = state.stats.total_tokens.cache_read_tokens;
    let input_tokens = state.stats.total_tokens.input_tokens;

    let avg_cost_per_day = state.total_cost / calendar_days as f64;
    let mut lines: Vec<Line> = Vec::new();

    let w = inner_width.saturating_sub(2);
    let sep = Line::from(Span::styled(
        "─".repeat(w),
        Style::default().fg(theme::BORDER),
    ));
    let bar_w = (w / 3).min(16);

    // Cost & sessions overview. The subagent suffix mirrors the Sessions
    // row: the cost includes subagents while the session count leaves them out.
    let subagent_cost_suffix = if subagent_cost > 0.0 {
        format!(" (incl. {} subagent)", super::format_cost(subagent_cost, 2))
    } else {
        String::new()
    };
    lines.push(Line::from(vec![
        Span::styled(" Total Cost  ", Style::default().fg(theme::LABEL)),
        Span::styled(
            super::format_cost_marked(
                state.total_cost,
                !state.models_without_pricing.is_empty(),
                2,
            ),
            cost_total_style(!state.models_without_pricing.is_empty()),
        ),
        Span::styled(subagent_cost_suffix, Style::default().fg(theme::META)),
        Span::styled(
            format!("  ({calendar_days} days, {active_days} active)"),
            Style::default().fg(theme::META),
        ),
    ]));

    if !state.models_without_pricing.is_empty() {
        let mut names: Vec<&str> = state
            .models_without_pricing
            .iter()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        lines.push(Line::from(vec![
            Span::styled(
                " ⚠ Pricing gap  ",
                Style::default().fg(theme::WARNING).bold(),
            ),
            Span::styled(names.join(", "), Style::default().fg(theme::LABEL)),
        ]));
    }

    let total_duration_mins: i64 = state
        .daily_groups
        .iter()
        .flat_map(crate::aggregator::DailyGroup::user_sessions)
        .map(|s| s.day_duration_mins().max(1))
        .sum();
    let avg_duration_mins = if total_sessions > 0 {
        total_duration_mins / total_sessions as i64
    } else {
        0
    };
    let avg_dur_str = crate::text::format_duration_mins(avg_duration_mins);
    // The headline count excludes subagents (subagent activity is
    // attributed to the parent session in every other metric); the
    // dim suffix surfaces the subagent slice so readers don't wonder
    // whether the number is inflated by nested invocations.
    let subagent_session_suffix = if subagent_sessions > 0 {
        format!(" excl. subagents (+{subagent_sessions} sub)")
    } else {
        String::new()
    };
    lines.push(Line::from(vec![
        Span::styled(" Sessions    ", Style::default().fg(theme::LABEL)),
        Span::styled(
            format!("{total_sessions}"),
            Style::default().fg(theme::SUCCESS).bold(),
        ),
        Span::styled(subagent_session_suffix, Style::default().fg(theme::META)),
        Span::styled(
            format!(
                "  ${:.2}/day  {}/day  {}/ses  {avg_dur_str}/ses",
                avg_cost_per_day.max(0.0),
                crate::format_number(tokens_per_day),
                crate::format_number(tokens_per_session)
            ),
            Style::default().fg(theme::META),
        ),
    ]));
    lines.push(super::churn_line(
        " Edits       ",
        state.stats.line_churn,
        Style::default().fg(theme::META),
    ));
    // Without this the reader cannot tell a per-calendar-day average from a
    // per-active-day one, and the two differ by the idle-day share. Silent
    // when every day was active, since then there is nothing to disambiguate.
    if active_days < calendar_days {
        lines.push(Line::from(Span::styled(
            format!(
                " /day figures divide by all {calendar_days} days, not the {active_days} active ones."
            ),
            Style::default().fg(theme::LABEL),
        )));
    }
    lines.push(sep.clone());

    // Rates with descriptive labels
    let rates: [(f64, &str, ratatui::style::Color); 3] = [
        (cache_hit_rate, "Cache Hit Rate   ", theme::SUCCESS),
        // Formula label, not prose: numerator and denominator are
        // both explicit so neither "which TTL" nor "vs reads or only
        // writes" can be misread. 5m is the cheaper bucket so higher
        // % reads as "better" (matches the other rates in this row).
        (cache_5m_share, "Cache 5m/(5m+1h) ", theme::ACCENT),
        (
            tool_success_rate,
            "Tool Success Rate",
            if tool_success_rate >= 90.0 {
                theme::SUCCESS
            } else {
                theme::WARNING
            },
        ),
    ];
    for (rate, label, color) in &rates {
        let filled = (*rate / 100.0 * bar_w as f64).round() as usize;
        lines.push(Line::from(vec![
            Span::styled(format!(" {label} "), Style::default().fg(theme::LABEL)),
            Span::styled(format!("{rate:>5.1}%"), Style::default().fg(*color).bold()),
            Span::raw(" "),
            super::bar_fill(filled, bar_w, *color),
            super::bar_empty(filled, bar_w),
        ]));
    }
    lines.push(sep.clone());

    // Tokens (2 lines, key-value style)
    let output = state.stats.total_tokens.output_tokens;
    let cache_write = state.stats.total_tokens.cache_creation_tokens;
    lines.push(Line::from(vec![
        Span::styled(" Input  ", Style::default().fg(theme::LABEL)),
        Span::styled(
            format!("{:<10}", crate::format_number(input_tokens)),
            Style::default().fg(theme::TEXT_BRIGHT),
        ),
        Span::styled("  Output  ", Style::default().fg(theme::LABEL)),
        Span::styled(
            crate::format_number(output),
            Style::default().fg(theme::TEXT_BRIGHT),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" CacheR ", Style::default().fg(theme::LABEL)),
        Span::styled(
            format!("{:<10}", crate::format_number(cache_read)),
            Style::default().fg(theme::TEXT_BRIGHT),
        ),
        Span::styled("  CacheW  ", Style::default().fg(theme::LABEL)),
        Span::styled(
            crate::format_number(cache_write),
            Style::default().fg(theme::TEXT_BRIGHT),
        ),
    ]));
    // How much of the Input+Output above ran in subagents. Skipped when
    // there are no subagent invocations at all.
    let work_tokens = state.stats.total_tokens.work_tokens();
    if subagent_work_tokens > 0 {
        let sub_pct = crate::text::format_pct(subagent_work_tokens, work_tokens);
        lines.push(Line::from(vec![
            Span::styled(" of which subagent ", Style::default().fg(theme::LABEL)),
            Span::styled(
                format!(
                    "{} ({sub_pct} of work)",
                    crate::format_number(subagent_work_tokens)
                ),
                Style::default().fg(theme::META),
            ),
        ]));
    }
    lines.push(sep.clone());

    // The panel's charts; a popup too narrow for them falls back to one
    // sparkline per metric.
    let today = chrono::Local::now().date_naive();
    if trend_charts_fit(w) {
        lines.extend(metrics_trend_charts(
            state,
            today,
            u16::try_from(w).unwrap_or(u16::MAX),
            DETAIL_TREND_ROWS,
        ));
    } else {
        let trend_days = w.saturating_sub(46).clamp(7, 30);
        let trends = metric_trends(state, today, trend_days);
        for t in &trends {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {:<12} ", t.label),
                    Style::default().fg(theme::LABEL),
                ),
                Span::styled(t.spark.clone(), Style::default().fg(theme::PRIMARY)),
                Span::styled(
                    format!("  7d {}", t.recent),
                    Style::default().fg(theme::PRIMARY),
                ),
                Span::styled(
                    format!("  30d {}  ", t.baseline),
                    Style::default().fg(theme::META),
                ),
                Span::styled(t.delta.clone(), Style::default().fg(theme::META)),
            ]));
        }
    }
    lines.push(sep.clone());

    // Models (blue gradient, matching dashboard detail). Percent
    // labelled `$%` so it doesn't get confused with the Dashboard
    // Models panel's token-share `tok%` column.
    if !state.model_costs.is_empty() {
        let name_w = 12;
        let bar_max = w.saturating_sub(name_w + 20);
        let total_cost: f64 = state.model_costs.iter().map(|(_, c)| *c).sum();
        let max_cost = state
            .model_costs
            .iter()
            .map(|(_, c)| *c)
            .fold(0.0f64, f64::max)
            .max(0.01);
        for (model, cost) in state.model_costs.iter().take(5) {
            let ratio = *cost / max_cost;
            let pct_label = crate::text::format_pct(
                (*cost * 1_000_000.0) as u64,
                (total_cost * 1_000_000.0) as u64,
            );
            let filled = (ratio * bar_max as f64).round() as usize;
            let intensity = theme::bar_intensity(ratio);
            let bar_color = theme::ramp_color(theme::RAMP_BLUE, intensity);
            let name: String = model.chars().take(name_w).collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {name:<name_w$}"),
                    Style::default().fg(super::model_color(model)),
                ),
                super::bar_fill(filled, bar_max, bar_color),
                super::bar_empty(filled, bar_max),
                Span::styled(
                    format!(
                        " {:>6}",
                        super::format_cost_marked(
                            *cost,
                            state.models_without_pricing.contains(model),
                            0
                        )
                    ),
                    cost_total_style(state.models_without_pricing.contains(model)),
                ),
                Span::styled(format!(" {pct_label:>4}"), Style::default().fg(theme::META)),
            ]));
        }
        lines.push(sep.clone());
    }

    // Projects (purple gradient, matching dashboard detail)
    if !state.stats.project_stats.is_empty() {
        let name_w = w.saturating_sub(22).min(24);
        let bar_max = 12;
        let mut projects: Vec<_> = state.stats.project_stats.iter().collect();
        projects.sort_by_key(|p| std::cmp::Reverse(p.1.work_tokens));
        let max_tokens = projects.first().map_or(1, |(_, s)| s.work_tokens);
        for (name, ps) in projects.iter().take(5) {
            let short = state.project_label(name);
            let display: String = short.chars().take(name_w).collect();
            let ratio = ps.work_tokens as f64 / max_tokens as f64;
            let filled = (ratio * bar_max as f64).round() as usize;
            let intensity = theme::bar_intensity(ratio);
            let bar_color = theme::ramp_color(theme::RAMP_PURPLE, intensity);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {display:<name_w$}"),
                    Style::default().fg(theme::PROJECT),
                ),
                super::bar_fill(filled, bar_max, bar_color),
                super::bar_empty(filled, bar_max),
                Span::styled(
                    format!(" {:>4} ses", ps.sessions),
                    Style::default().fg(theme::META),
                ),
                Span::styled(
                    format!(" {}", crate::format_number(ps.work_tokens)),
                    Style::default().fg(theme::TOKENS),
                ),
            ]));
        }
        lines.push(sep.clone());
    }

    // Tools (Top N per section: Built-in / Skill / Agent / MCP)
    if !state.stats.tool_usage.is_empty() {
        use crate::aggregator::{ToolCategory, classify_tool, format_tool_short};

        let tools: Vec<_> = state.stats.tool_usage.iter().collect();
        let (mut builtin, mut skill, mut agent, mut mcp, mut command): (
            Vec<&(&String, &usize)>,
            Vec<&(&String, &usize)>,
            Vec<&(&String, &usize)>,
            Vec<&(&String, &usize)>,
            Vec<&(&String, &usize)>,
        ) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for t in tools.iter() {
            match classify_tool(t.0) {
                ToolCategory::BuiltIn => builtin.push(t),
                ToolCategory::Skill { .. } => skill.push(t),
                ToolCategory::Agent { .. } => agent.push(t),
                ToolCategory::Mcp { .. } => mcp.push(t),
                ToolCategory::Command { .. } => command.push(t),
            }
        }
        // Stable tiebreak by name so equal-count rows don't shuffle on redraw
        // (HashMap iteration order is non-deterministic across rebuilds).
        builtin.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        skill.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        agent.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        mcp.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        command.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

        // Bar widths normalize against the per-category top entry, so
        // the `%` column has to use the **same** denominator (category
        // total). Sharing a grand-total denominator across categories
        // would let the bar render full while the `%` rounds to zero
        // for categories that are small relative to the dominant one.
        let builtin_total: usize = builtin.iter().map(|(_, c)| **c).sum();
        let mcp_total: usize = mcp.iter().map(|(_, c)| **c).sum();
        let skill_total: usize = skill.iter().map(|(_, c)| **c).sum();
        let agent_total: usize = agent.iter().map(|(_, c)| **c).sum();
        let command_total: usize = command.iter().map(|(_, c)| **c).sum();
        let name_w = 18;
        let bar_max = w.saturating_sub(name_w + 12);

        let render = |tool_name: String,
                      count: usize,
                      max: usize,
                      category_total: usize,
                      color: ratatui::style::Color,
                      lines: &mut Vec<Line>| {
            let ratio = count as f64 / max as f64;
            let pct = crate::text::format_pct(count as u64, category_total as u64);
            let filled = (ratio * bar_max as f64).round() as usize;
            let intensity = theme::bar_intensity(ratio);
            let bar_color = theme::ramp_color(theme::RAMP_OLIVE, intensity);
            // One column short of the cell so a cut name never touches its bar.
            let name = super::truncate_with_ellipsis(&tool_name, name_w - 1);
            lines.push(Line::from(vec![
                Span::styled(format!(" {name:<name_w$}"), Style::default().fg(color)),
                super::bar_fill(filled, bar_max, bar_color),
                super::bar_empty(filled, bar_max),
                Span::styled(format!(" {count:>5}"), Style::default().fg(theme::VALUE)),
                Span::styled(format!(" {pct:>3}"), Style::default().fg(theme::META)),
            ]));
        };

        let has_meta = !skill.is_empty() || !agent.is_empty() || !mcp.is_empty();
        // Header makes the segmentation explicit so the % column is
        // readable: each row's percentage is against its own category
        // total, not against the grand total across categories.
        lines.push(Line::from(vec![Span::styled(
            format!(
                " Top tools · {} different used (% per category)",
                state.stats.tool_usage.len()
            ),
            Style::default().fg(theme::LABEL),
        )]));
        if !has_meta {
            // Fallback: top 6 built-in
            let max_tool = builtin.first().map_or(1, |(_, c)| **c).max(1);
            for entry in builtin.iter().take(6) {
                let (tool, count) = entry;
                render(
                    (*tool).clone(),
                    **count,
                    max_tool,
                    builtin_total,
                    theme::CAT_BUILTIN,
                    &mut lines,
                );
            }
        } else {
            // Order matches Tools popup tabs. Built-in and MCP stay
            // separate here (popup merges them) — built-in calls are
            // 5–10× MCP, so a merged top-N would hide MCP rows.
            let max_builtin = builtin.first().map_or(1, |(_, c)| **c).max(1);
            let max_mcp = mcp.first().map_or(1, |(_, c)| **c).max(1);
            let max_skill = skill.first().map_or(1, |(_, c)| **c).max(1);
            let max_agent = agent.first().map_or(1, |(_, c)| **c).max(1);
            let max_command = command.first().map_or(1, |(_, c)| **c).max(1);
            for entry in builtin.iter().take(3) {
                let (tool, count) = entry;
                render(
                    (*tool).clone(),
                    **count,
                    max_builtin,
                    builtin_total,
                    theme::CAT_BUILTIN,
                    &mut lines,
                );
            }
            for entry in mcp.iter().take(3) {
                let (tool, count) = entry;
                render(
                    format_tool_short(tool),
                    **count,
                    max_mcp,
                    mcp_total,
                    theme::CAT_MCP,
                    &mut lines,
                );
            }
            for entry in skill.iter().take(3) {
                let (tool, count) = entry;
                render(
                    format_tool_short(tool),
                    **count,
                    max_skill,
                    skill_total,
                    theme::CAT_SKILLS,
                    &mut lines,
                );
            }
            for entry in command.iter().take(3) {
                let (tool, count) = entry;
                render(
                    format_tool_short(tool),
                    **count,
                    max_command,
                    command_total,
                    theme::CAT_COMMANDS,
                    &mut lines,
                );
            }
            for entry in agent.iter().take(3) {
                let (tool, count) = entry;
                render(
                    format_tool_short(tool),
                    **count,
                    max_agent,
                    agent_total,
                    theme::CAT_SUBAGENTS,
                    &mut lines,
                );
            }
        }
        lines.push(sep.clone());
    }

    // Per-category usage — absolute counts only (cross-category % is
    // incommensurable across these categories). Each row = sessions that
    // used the category + the top entry.
    {
        let total = state.stats.total_sessions_count;
        let skills = state.stats.sessions_using_skills;
        let subagents = state.stats.sessions_using_subagents;
        let mcp_used = state.stats.sessions_using_mcp;
        let commands_used = state.stats.sessions_using_commands;
        lines.push(Line::from(vec![
            Span::styled(" Usage by category  ", Style::default().fg(theme::LABEL)),
            Span::styled(
                format!("of {total} sessions"),
                Style::default().fg(theme::LABEL),
            ),
        ]));
        // Order matches popup tabs: MCP → Skills → Commands → Subagents.
        let rows: [(&str, usize, ratatui::style::Color); 4] = [
            ("MCP tools", mcp_used, theme::CAT_MCP),
            ("Skills", skills, theme::CAT_SKILLS),
            ("Commands", commands_used, theme::CAT_COMMANDS),
            ("Subagents", subagents, theme::CAT_SUBAGENTS),
        ];
        for (name, used, color) in rows {
            let prefix = match name {
                "Skills" => "skill:",
                "Commands" => "command:",
                "Subagents" => "agent:",
                _ => "mcp__",
            };
            let top = state
                .stats
                .tool_sessions
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .max_by_key(|(_, c)| **c);
            let top_text = top.map_or(String::new(), |(k, c)| {
                let display = crate::aggregator::format_tool_short(k);
                let label = display
                    .strip_prefix("skill:")
                    .or_else(|| display.strip_prefix("agent:"))
                    .or_else(|| display.strip_prefix("command:"))
                    .map_or_else(|| display.clone(), str::to_string);
                format!(" · top: {label} ({c} ses)")
            });
            lines.push(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(format!("{name:<11}"), Style::default().fg(color)),
                Span::styled(format!("{used:>4} ses"), Style::default().fg(color).bold()),
                Span::styled(top_text, Style::default().fg(theme::LABEL)),
            ]));
        }
        lines.push(sep.clone());
    }

    // MCP Activation (configured vs active, underutilized list)
    if !state.mcp_status.is_empty() {
        let now = chrono::Utc::now();
        let stale_at = state.stale_reference();
        let configured_count = state.mcp_status.iter().filter(|s| s.configured).count();
        let mut active: Vec<_> = state
            .mcp_status
            .iter()
            .filter(|s| s.configured && !s.is_underutilized(stale_at, 30))
            .collect();
        // Tiebreak on server name so equal-call servers don't reshuffle on redraw.
        active.sort_by(|a, b| {
            b.total_calls
                .cmp(&a.total_calls)
                .then_with(|| a.name.cmp(&b.name))
        });
        let mut underutilized: Vec<_> = state
            .mcp_status
            .iter()
            .filter(|s| s.is_underutilized(stale_at, 30))
            .collect();
        underutilized.sort_by(|a, b| {
            let ad = a.days_since_last_use(now).unwrap_or(i64::MAX);
            let bd = b.days_since_last_use(now).unwrap_or(i64::MAX);
            bd.cmp(&ad)
        });

        lines.push(Line::from(vec![
            Span::styled(" MCP Activation  ", Style::default().fg(theme::LABEL)),
            Span::styled(
                format!("{}/{configured_count}", active.len()),
                Style::default().fg(theme::PRIMARY).bold(),
            ),
            Span::styled(" active", Style::default().fg(theme::LABEL)),
            Span::styled(
                format!(" · {} stale (unused 30d+)", underutilized.len()),
                Style::default().fg(if underutilized.is_empty() {
                    theme::LABEL
                } else {
                    theme::WARNING
                }),
            ),
        ]));
        for s in &active {
            let detail = match s.days_since_last_use(now) {
                None => format!("{} calls", s.total_calls),
                Some(0) => format!("{} calls, today", s.total_calls),
                Some(1) => format!("{} calls, yesterday", s.total_calls),
                Some(d) => format!("{} calls, {d}d ago", s.total_calls),
            };
            lines.push(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    mcp_name_cell(&s.name, w),
                    Style::default().fg(theme::SUCCESS),
                ),
                Span::styled(detail, Style::default().fg(theme::LABEL)),
            ]));
        }
        for s in &underutilized {
            let detail = match s.days_since_last_use(now) {
                None => "never used".to_string(),
                Some(0) => "last used today".to_string(),
                Some(1) => "last used yesterday".to_string(),
                Some(d) => format!("last used {d}d ago"),
            };
            lines.push(Line::from(vec![
                Span::styled("   ", Style::default()),
                Span::styled(
                    mcp_name_cell(&s.name, w),
                    Style::default().fg(theme::WARNING),
                ),
                Span::styled(detail, Style::default().fg(theme::LABEL)),
            ]));
        }
        lines.push(sep.clone());
    }

    // Languages (teal, matching dashboard languages panel)
    if !state.stats.language_usage.is_empty() {
        let mut langs: Vec<_> = state.stats.language_usage.iter().collect();
        langs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let max_lang = langs.first().map_or(1, |(_, c)| **c).max(1);
        let total_lang: usize = langs.iter().map(|(_, c)| **c).sum();
        let name_w = 12;
        let bar_max = w.saturating_sub(name_w + 12);
        for (lang, count) in langs.iter().take(6) {
            let ratio = **count as f64 / max_lang as f64;
            let pct = crate::text::format_pct(**count as u64, total_lang as u64);
            let filled = (ratio * bar_max as f64).round() as usize;
            let intensity = theme::bar_intensity(ratio);
            let bar_color = theme::ramp_color(theme::RAMP_DEEP_TEAL, intensity);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        " {name:<name_w$}",
                        name = lang.chars().take(name_w).collect::<String>()
                    ),
                    Style::default().fg(theme::VALUE),
                ),
                super::bar_fill(filled, bar_max, bar_color),
                super::bar_empty(filled, bar_max),
                Span::styled(format!(" {count:>5}"), Style::default().fg(theme::VALUE)),
                Span::styled(format!(" {pct:>3}"), Style::default().fg(theme::META)),
            ]));
        }
    }
    lines
}

pub(super) fn draw_insights_detail_popup(frame: &mut Frame, area: Rect, state: &mut AppState) {
    // Shared size with the Session / Dashboard detail popups (see
    // `super::detail_popup_area`) so the three present identically.
    let popup_area = super::detail_popup_area(area);
    let popup_width = popup_area.width;
    let popup_height = popup_area.height;

    state.layout.active_popup_area = Some(popup_area);
    frame.render_widget(Clear, popup_area);

    let today = chrono::Local::now().date_naive();
    let calendar_days = state.calendar_days(today);

    // Title-cased to match the Dashboard popup convention (`Daily Costs`,
    // `Languages`, `Hourly Average`, etc.) — lowercase abbreviations were
    // an inconsistency that made these popups look like internal/debug labels.
    let panel_labels: [&str; INSIGHTS_PANELS] = ["Metrics", "Weekly", "Monthly"];
    let current_panel = state.insights_panel.min(panel_labels.len() - 1);
    let panel_label = panel_labels[current_panel];

    let inner_width = popup_width.saturating_sub(4) as usize;

    let mut lines: Vec<Line> = Vec::new();

    match current_panel {
        0 => lines = insights_detail_metrics(state, calendar_days, inner_width),
        1 => {
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

            let today_date = chrono::Local::now().date_naive();
            let today_weekday = today_date.weekday();
            let first_date = state.daily_groups.last().map_or(today_date, |g| g.date);
            let weekday_avg =
                super::aggregate_weekday_avg(&state.daily_groups, calendar_days, first_date);
            let (max_day, max_avg) = weekday_avg
                .iter()
                .max_by_key(|(_, v)| **v)
                .map_or((Weekday::Mon, 0u64), |(k, v)| (*k, *v));
            let max_weekly = max_avg.max(1);
            let total_weekly: u64 = weekday_avg.values().sum();
            let bar_width = inner_width.saturating_sub(28);

            for (wd, label) in &weekdays {
                let avg = weekday_avg.get(wd).copied().unwrap_or(0);
                let ratio = avg as f64 / max_weekly as f64;
                let filled = (ratio * bar_width as f64).round() as usize;
                let pct = crate::text::format_pct(avg, total_weekly);
                let is_today = *wd == today_weekday;
                let marker = if is_today { "▶" } else { " " };
                let intensity = theme::bar_intensity(ratio);
                let bar_color = theme::primary_with_intensity(intensity);

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {marker}{label:<9} "),
                        Style::default().fg(if is_today {
                            theme::PRIMARY
                        } else {
                            theme::META
                        }),
                    ),
                    super::bar_fill(filled, bar_width, bar_color),
                    super::bar_empty(filled, bar_width),
                    Span::styled(
                        format!(" {:>5}/day", crate::format_number(avg)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                    Span::styled(format!(" {pct:>3}"), Style::default().fg(theme::META)),
                ]));
            }

            lines.push(Line::from(""));
            let avg_daily_tokens = state.stats.total_tokens.work_tokens() / calendar_days as u64;
            if max_avg > 0 {
                let max_label = weekdays
                    .iter()
                    .find(|(wd, _)| *wd == max_day)
                    .map_or("?", |(_, l)| *l);
                lines.push(Line::from(vec![
                    Span::styled(" Most active: ", Style::default().fg(theme::LABEL)),
                    Span::styled(max_label, Style::default().fg(theme::SUCCESS).bold()),
                    Span::styled("  avg: ", Style::default().fg(theme::LABEL)),
                    Span::styled(
                        format!("{}/day", crate::format_number(avg_daily_tokens)),
                        Style::default().fg(theme::PRIMARY),
                    ),
                ]));
            } else {
                lines.push(Line::from(Span::styled(
                    " No activity recorded",
                    Style::default().fg(theme::PLACEHOLDER),
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(
                [Span::styled(
                    " Last 12 weeks  ",
                    Style::default().fg(theme::HEADER),
                )]
                .into_iter()
                .chain(month_legend())
                .collect::<Vec<_>>(),
            ));
            lines.extend(week_trend_lines(state, today_date, 12, 9, "  "));

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                " By hour (avg per day)",
                Style::default().fg(theme::HEADER),
            )));
            lines.extend(weekday_hour_lines(state, today_date));
        }
        2 => {
            let monthly_costs = super::aggregate_monthly_costs(&state.daily_costs);
            let monthly_tokens = super::aggregate_monthly_tokens(&state.daily_groups);

            // Trailing window covers max(data months, 12) so the popup keeps
            // the inline panel's 12-month minimum (with zero months filled
            // when data is sparse) and still surfaces older history when the
            // user has more than 12 months of data.
            let window_size = monthly_costs.len().max(12);
            let trailing_window = trailing_n_months(&monthly_costs, window_size);
            let all_months: Vec<(&String, &f64)> =
                trailing_window.iter().rev().map(|(k, v)| (k, v)).collect();
            let total_months = all_months.len();
            // Stretch the column width when there are few enough months to fit
            // the whole history without scrolling. Default 8 leaves a ragged
            // gap on the right when total fits in the popup; this picks the
            // largest column width (capped at 10) that lets every month show.
            let avail = inner_width.saturating_sub(2);
            // Pick the widest column that still fits every month — falls back
            // to the default 8 only when the popup is too narrow to fit them
            // all. Floor of 6 keeps the labels (`YY-MM`) readable.
            let col_width = if total_months > 0 {
                (avail / total_months.max(1)).clamp(6, 10)
            } else {
                8
            };
            let visible_months = (avail / col_width).max(1);
            let max_scroll = total_months.saturating_sub(visible_months);
            if state.insights_detail_scroll() > max_scroll {
                state.set_insights_detail_scroll(max_scroll);
            }
            let skip = total_months
                .saturating_sub(visible_months)
                .saturating_sub(state.insights_detail_scroll());
            let months: Vec<_> = all_months
                .into_iter()
                .rev()
                .skip(skip)
                .take(visible_months)
                .collect();
            let (avg_monthly, avg_monthly_tokens) =
                trailing_year_average(&monthly_costs, &monthly_tokens);

            let bar_height = 6usize;
            let heights = share_heights(
                &months.iter().map(|(_, c)| **c).collect::<Vec<_>>(),
                &months
                    .iter()
                    .map(|(m, _)| monthly_tokens.get(*m).copied().unwrap_or(0))
                    .collect::<Vec<_>>(),
            );

            lines.push(Line::from(
                [Span::raw(" ")]
                    .into_iter()
                    .chain(month_legend())
                    .collect::<Vec<_>>(),
            ));
            for row in (0..bar_height).rev() {
                let mut row_spans: Vec<Span> = vec![Span::raw("  ")];
                for &(cost_h, token_h) in &heights {
                    row_spans.extend(month_bar_pair(cost_h, token_h, row, bar_height, col_width));
                }
                lines.push(Line::from(row_spans));
            }

            let mut label_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut cost_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut tok_spans: Vec<Span> = vec![Span::raw("  ")];
            let mut diff_spans: Vec<Span> = vec![Span::raw("  ")];
            for (month, cost) in &months {
                label_spans.push(Span::styled(
                    format!("{:^col_width$}", short_month(month)),
                    Style::default().fg(theme::TIME),
                ));
                cost_spans.push(month_cost_span(state, month, **cost, col_width));
                tok_spans.push(month_tokens_span(&monthly_tokens, month, col_width));

                diff_spans.push(Span::styled(
                    format!(
                        "{:^col_width$}",
                        month_vs_average(month, **cost, avg_monthly)
                    ),
                    Style::default().fg(theme::META),
                ));
            }
            lines.push(Line::from(label_spans));
            lines.push(Line::from(cost_spans));
            lines.push(Line::from(tok_spans));
            lines.push(Line::from(diff_spans));

            lines.push(Line::from(""));
            let mut summary_spans = vec![
                Span::styled("  avg: ", Style::default().fg(theme::LABEL)),
                Span::styled(
                    format!(
                        "{}/mo",
                        super::format_cost_marked(
                            avg_monthly,
                            !state.models_without_pricing.is_empty(),
                            0
                        )
                    ),
                    cost_total_style(!state.models_without_pricing.is_empty()),
                ),
                Span::styled(
                    format!("  {}/mo", crate::format_number(avg_monthly_tokens)),
                    Style::default().fg(theme::PRIMARY),
                ),
            ];
            let this_month = this_month_spans(state, &monthly_costs);
            if !this_month.is_empty() {
                summary_spans.push(Span::styled(" | ", Style::default().fg(theme::LABEL)));
                summary_spans.extend(this_month);
            }
            summary_spans.push(Span::styled(" | ", Style::default().fg(theme::LABEL)));
            summary_spans.push(Span::styled("total: ", Style::default().fg(theme::LABEL)));
            // precision=0 matches the neighbouring avg / this mo / forecast
            // figures on the same line and avoids clipping the last digit
            // against the popup border.
            summary_spans.push(Span::styled(
                super::format_cost_marked(
                    state.total_cost,
                    !state.models_without_pricing.is_empty(),
                    0,
                ),
                cost_total_style(!state.models_without_pricing.is_empty()),
            ));
            lines.push(Line::from(summary_spans));
            if total_months > visible_months {
                lines.push(Line::from(vec![
                    Span::styled("  ↑↓: scroll months  ", Style::default().fg(theme::LABEL)),
                    Span::styled(
                        format!(
                            "{}-{} of {}",
                            skip + 1,
                            (skip + visible_months).min(total_months),
                            total_months
                        ),
                        Style::default().fg(theme::META),
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
    state.set_insights_detail_scroll(state.insights_detail_scroll().min(max_scroll));

    let popup = Paragraph::new(lines)
        .scroll((state.insights_detail_scroll() as u16, 0))
        .block(
            super::popup_block(&format!(" {panel_label} ")).title_bottom(Line::from(vec![
                Span::styled(
                    " ←→: switch  ↑↓: scroll  Enter/i/q: close ",
                    Style::default().fg(theme::LABEL),
                ),
                Span::styled(
                    format!("[{}/{INSIGHTS_PANELS}] {} ", current_panel + 1, panel_label),
                    Style::default().fg(theme::PRIMARY),
                ),
            ])),
        );

    frame.render_widget(popup, popup_area);
}

/// Bar rows per trend chart, tallest first: the Metrics block shrinks a
/// chart before it drops one.
const TREND_CHART_ROWS: [usize; 8] = [8, 7, 6, 5, 4, 3, 2, 1];
/// The lowest chart height at which the totals still keep their rows.
const TREND_ROWS_WITH_TOTALS: usize = 3;

/// The Metrics panel's rows below the cards, fitted to `rows`: the 30-day
/// trend charts, the work line, then the popup's totals. Least essential out
/// first — chart height down to three rows, the totals, then the rest of the
/// chart height, then the charts.
fn metrics_extra_lines(
    state: &AppState,
    today: chrono::NaiveDate,
    width: u16,
    rows: usize,
) -> Vec<Line<'static>> {
    let charts_fit = trend_charts_fit(usize::from(width));
    let work = metrics_work_lines(state);
    let totals = metrics_total_lines(state, width);
    // Legend, then per trend: header, `r` bar rows, date axis.
    let chart_h = |r: usize| 1 + 3 * (r + 2);
    let mut plans: Vec<(Option<usize>, bool)> = Vec::new();
    if charts_fit {
        // The totals stay while a chart keeps a readable height.
        plans.extend(
            TREND_CHART_ROWS
                .iter()
                .filter(|&&r| r >= TREND_ROWS_WITH_TOTALS)
                .map(|&r| (Some(r), true)),
        );
        plans.extend(TREND_CHART_ROWS.iter().map(|&r| (Some(r), false)));
    }
    plans.extend([(None, true), (None, false)]);
    let Some((chart, with_totals)) = plans.into_iter().find(|(chart, with_totals)| {
        chart.map_or(0, chart_h) + work.len() + if *with_totals { totals.len() } else { 0 } <= rows
    }) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(r) = chart {
        lines.extend(metrics_trend_charts(state, today, width, r));
    }
    lines.extend(work);
    if with_totals {
        lines.extend(totals);
    }
    lines
}

const TREND_DAYS: usize = 30;
/// Bar rows per chart in the detail popup: tall enough to read, short
/// enough that the three charts don't bury the sections below.
const DETAIL_TREND_ROWS: usize = 4;

fn trend_charts_fit(width: usize) -> bool {
    width >= TREND_INDENT + TREND_AXIS_W + TREND_DAYS * 2
}
const TREND_INDENT: usize = 3;
/// Days at the right of a chart drawn bright: the 7-day average's window.
const TREND_RECENT_DAYS: usize = 7;

/// Each trend's header (7d / 30d averages, change) over a day-per-column bar
/// chart of `rows` scaled to its own peak, each row's value on a left axis
/// and dates below. The last week is drawn bright so the 7-day
/// average's window shows.
fn metrics_trend_charts(
    state: &AppState,
    today: chrono::NaiveDate,
    width: u16,
    rows: usize,
) -> Vec<Line<'static>> {
    // Columns widen with the panel so the chart spans it.
    let col_w = (usize::from(width).saturating_sub(TREND_INDENT * 2 + TREND_AXIS_W) / TREND_DAYS)
        .clamp(2, 4);
    let indent = " ".repeat(TREND_INDENT);
    let mut lines = vec![Line::from(Span::styled(
        format!("{indent}Last 30 days · bright = last 7 days · \u{b7} = no activity"),
        Style::default().fg(theme::LABEL),
    ))];
    for t in metric_trends(state, today, TREND_DAYS) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{indent}{:<20}", t.name),
                Style::default().fg(theme::HEADER),
            ),
            Span::styled("last 7 days avg ", Style::default().fg(theme::LABEL)),
            Span::styled(t.recent, Style::default().fg(theme::PRIMARY)),
            Span::styled(" · 30 days avg ", Style::default().fg(theme::LABEL)),
            Span::styled(t.baseline, Style::default().fg(theme::META)),
            Span::styled(" · ", Style::default().fg(theme::LABEL)),
            Span::styled(t.delta, Style::default().fg(theme::META)),
        ]));
        let peak = t.values.iter().flatten().copied().fold(0.0f64, f64::max);
        let recent_from = t.values.len().saturating_sub(TREND_RECENT_DAYS);
        for row in (0..rows).rev() {
            // Each row is labelled with the value at its top edge.
            let tick = (t.fmt)(peak * (row + 1) as f64 / rows as f64);
            let mut spans = vec![Span::styled(
                format!("{indent}{tick:>w$} ┤ ", w = TREND_AXIS_W - 3),
                Style::default().fg(theme::LABEL),
            )];
            for (i, v) in t.values.iter().enumerate() {
                let ratio = v.filter(|_| peak > 0.0).map_or(0.0, |v| v / peak);
                let cell = month_bar_cell(ratio, row, rows);
                let (text, color) = if ratio <= 0.0 && row == 0 {
                    ("· ", theme::SEPARATOR)
                } else if i >= recent_from {
                    (cell, theme::PRIMARY)
                } else {
                    (cell, theme::primary_with_intensity(0.45))
                };
                spans.push(Span::styled(
                    format!("{text:<col_w$}"),
                    Style::default().fg(color),
                ));
            }
            lines.push(Line::from(spans));
        }
        lines.push(trend_date_axis(today, col_w, &indent));
    }
    lines
}

/// Columns left of a trend chart's bars: the axis tick and `┤ `.
const TREND_AXIS_W: usize = 9;

/// Dates under a trend chart: every ten days from the window's start, and
/// `today` under the last column.
fn trend_date_axis(today: chrono::NaiveDate, col_w: usize, indent: &str) -> Line<'static> {
    let mut axis: Vec<char> = " ".repeat(TREND_DAYS * col_w + 6).chars().collect();
    let mut put = |col: usize, text: &str| {
        for (k, ch) in text.chars().enumerate() {
            if let Some(cell) = axis.get_mut(col * col_w + k) {
                *cell = ch;
            }
        }
    };
    for i in (0..TREND_DAYS - 1).step_by(10) {
        let date = today - chrono::Duration::days((TREND_DAYS - 1 - i) as i64);
        put(i, &date.format("%m-%d").to_string());
    }
    put(TREND_DAYS - 1, "today");
    Line::from(Span::styled(
        format!(
            "{indent}{}{}",
            " ".repeat(TREND_AXIS_W),
            axis.into_iter().collect::<String>().trim_end()
        ),
        Style::default().fg(theme::TIME),
    ))
}

/// One line: lines edited and the share of work run by subagents.
fn metrics_work_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut work = super::churn_line(
        "   Lines edited  ",
        state.stats.line_churn,
        Style::default().fg(theme::LABEL),
    );
    let (sub, _) = subagent_spend(state);
    let total = state.stats.total_tokens.work_tokens();
    if sub > 0 {
        let pct = crate::text::format_pct(sub, total);
        work.spans.extend([
            Span::styled(
                "        Subagents' share of work tokens  ",
                Style::default().fg(theme::LABEL),
            ),
            Span::styled(pct, Style::default().fg(theme::TOKENS)),
            Span::styled(
                format!(
                    "  ({} of {} in+out)",
                    crate::format_number(sub),
                    crate::format_number(total)
                ),
                Style::default().fg(theme::LABEL),
            ),
        ]);
    }
    vec![work]
}

/// The detail popup's totals in one line each: top built-in tools,
/// cost by model, token kinds. Items past the width are dropped whole.
fn metrics_total_lines(state: &AppState, width: u16) -> Vec<Line<'static>> {
    use crate::aggregator::{ToolCategory, classify_tool};
    let fit = |label: &'static str, items: Vec<(String, String)>| {
        let mut spans = vec![Span::styled(
            format!("{}{label:<34}", " ".repeat(TREND_INDENT)),
            Style::default().fg(theme::LABEL),
        )];
        for (i, (name, value)) in items.into_iter().enumerate() {
            let piece = [
                Span::styled(
                    if i == 0 { "" } else { "  ·  " },
                    Style::default().fg(theme::SEPARATOR),
                ),
                Span::styled(format!("{name} "), Style::default().fg(theme::VALUE)),
                Span::styled(value, Style::default().fg(theme::META)),
            ];
            if super::spans_width(&spans) + super::spans_width(&piece) > usize::from(width) {
                break;
            }
            spans.extend(piece);
        }
        Line::from(spans)
    };
    let mut builtin: Vec<(&String, &usize)> = state
        .stats
        .tool_usage
        .iter()
        .filter(|(name, _)| matches!(classify_tool(name), ToolCategory::BuiltIn))
        .collect();
    builtin.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let builtin_total: usize = builtin.iter().map(|(_, c)| **c).sum();
    let tools = builtin
        .iter()
        .take(5)
        .map(|(name, count)| {
            (
                (*name).clone(),
                crate::text::format_pct(**count as u64, builtin_total as u64),
            )
        })
        .collect();
    let cost_total: f64 = state.model_costs.iter().map(|(_, c)| *c).sum();
    let models = state
        .model_costs
        .iter()
        .take(5)
        .map(|(model, cost)| {
            (
                model.clone(),
                crate::text::format_pct((*cost * 1e6) as u64, (cost_total * 1e6) as u64),
            )
        })
        .collect();
    let t = &state.stats.total_tokens;
    let tokens = [
        ("input", t.input_tokens),
        ("output", t.output_tokens),
        ("cache read", t.cache_read_tokens),
        ("cache write", t.cache_creation_tokens),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), crate::format_number(v)))
    .collect();
    vec![
        fit("Built-in tools (share of calls)", tools),
        fit("Cost by model (share of cost)", models),
        fit("Tokens by kind", tokens),
    ]
}

/// One trend row: the last `days` as a sparkline, the 7-day and whole-window
/// averages, and the change between them.
pub(crate) struct MetricTrend {
    pub label: &'static str,
    /// The label spelled out, for the Metrics panel's chart headers.
    pub name: &'static str,
    /// Formats a value of this metric the way its averages read.
    pub fmt: fn(f64) -> String,
    /// Oldest first, one per day; `None` where the day has no value.
    pub values: Vec<Option<f64>>,
    pub spark: String,
    pub recent: String,
    pub baseline: String,
    pub delta: String,
}

/// `$/day`, `Tokens/ses` and `Sessions/day` over the last `days`, shared by
/// the Metrics panel and its detail popup so both quote one figure. Cache hit
/// is left out: it sits near saturation, so its sparkline says nothing.
pub(crate) fn metric_trends(
    state: &AppState,
    today: chrono::NaiveDate,
    days: usize,
) -> [MetricTrend; 3] {
    fn build(
        state: &AppState,
        today: chrono::NaiveDate,
        days: usize,
        label: &'static str,
        name: &'static str,
        missing: MissingDay,
        sampler: fn(&crate::aggregator::DailyGroup) -> DailyTrendValue,
        fmt: fn(f64) -> String,
    ) -> MetricTrend {
        let series = metric_per_day(state, today, days, missing, sampler);
        let (recent, baseline) = summarise_series(&series);
        let spark = super::dashboard::render_spark_line(today, days, |d| {
            series
                .iter()
                .find(|(dd, _)| *dd == d)
                .and_then(|(_, v)| *v)
                .unwrap_or(0.0)
        });
        MetricTrend {
            label,
            name,
            fmt,
            values: series.iter().map(|(_, v)| *v).collect(),
            spark,
            recent: recent.map_or_else(|| "—".to_string(), fmt),
            baseline: baseline.map_or_else(|| "—".to_string(), fmt),
            delta: fmt_delta_pct(recent, baseline),
        }
    }
    [
        build(
            state,
            today,
            days,
            "$/day",
            "Cost per day",
            MissingDay::Zero,
            |g| DailyTrendValue {
                num: g.cost(crate::aggregator::CostCalculator::global()),
                den: 1.0,
            },
            |v| super::format_cost(v, 0),
        ),
        build(
            state,
            today,
            days,
            "Tokens/ses",
            "Tokens per session",
            MissingDay::Skip,
            tokens_per_session_value,
            |v| crate::format_number(v as u64),
        ),
        build(
            state,
            today,
            days,
            "Sessions/day",
            "Sessions per day",
            MissingDay::Zero,
            sessions_per_day_value,
            |v| format!("{v:.1}"),
        ),
    ]
}

/// Work tokens and cost run by subagents across the view.
fn subagent_spend(state: &AppState) -> (u64, f64) {
    let calculator = crate::aggregator::CostCalculator::global();
    state
        .daily_groups
        .iter()
        .flat_map(|g| g.sessions.iter())
        .filter(|s| s.is_subagent)
        .fold((0, 0.0), |(t, c), s| {
            (t + s.work_tokens(), c + s.cost(calculator))
        })
}

/// Per-day metric value, used by trends to build sparklines and the
/// trailing-window averages shown alongside them.
pub(crate) struct DailyTrendValue {
    /// Numerator (e.g., cache_read tokens).
    pub num: f64,
    /// Denominator (e.g., input + cache_read tokens). Skipped when 0.
    pub den: f64,
}

/// How a calendar day with no activity enters a trend series. A per-day rate
/// (`$/day`, `Sessions/day`) must count it as zero, or the trailing average
/// silently becomes a per-active-day figure and disagrees with every other
/// per-day number. A per-session rate must skip it — a day with no sessions
/// has no ratio to contribute.
#[derive(Clone, Copy)]
pub(crate) enum MissingDay {
    Zero,
    Skip,
}

pub(crate) fn metric_per_day(
    state: &AppState,
    today: chrono::NaiveDate,
    days: usize,
    missing: MissingDay,
    mut sample: impl FnMut(&crate::aggregator::DailyGroup) -> DailyTrendValue,
) -> Vec<(chrono::NaiveDate, Option<f64>)> {
    let by_date: std::collections::HashMap<chrono::NaiveDate, &crate::aggregator::DailyGroup> =
        state.daily_groups.iter().map(|g| (g.date, g)).collect();
    (0..days)
        .rev()
        .map(|offset| {
            let date = today - chrono::Duration::days(offset as i64);
            // A zero denominator is "not computable", never zero — it stays
            // `None` regardless of how absent days are treated.
            let value = match by_date.get(&date) {
                Some(g) => {
                    let v = sample(g);
                    (v.den > 0.0).then(|| v.num / v.den)
                }
                None => match missing {
                    MissingDay::Zero => Some(0.0),
                    MissingDay::Skip => None,
                },
            };
            (date, value)
        })
        .collect()
}

pub(crate) fn tokens_per_session_value(group: &crate::aggregator::DailyGroup) -> DailyTrendValue {
    // Subagent tokens over user sessions, as the headline figure divides.
    DailyTrendValue {
        num: group.work_tokens() as f64,
        den: group.user_sessions().count() as f64,
    }
}

pub(crate) fn sessions_per_day_value(group: &crate::aggregator::DailyGroup) -> DailyTrendValue {
    let sess = group.sessions.iter().filter(|s| !s.is_subagent).count();
    // Den=1 so the sample IS the session count; metric_per_day will return
    // `Some(sess)` only when `den > 0`, which is always true here.
    DailyTrendValue {
        num: sess as f64,
        den: 1.0,
    }
}

/// Returns (recent_avg, baseline_avg) where recent = last 7 samples and
/// baseline = all 30 samples. Both averages drop missing days (None) so
/// gaps don't drag values toward zero.
pub(crate) fn summarise_series(
    samples: &[(chrono::NaiveDate, Option<f64>)],
) -> (Option<f64>, Option<f64>) {
    fn avg(values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            None
        } else {
            Some(values.iter().sum::<f64>() / values.len() as f64)
        }
    }
    let recent: Vec<f64> = samples
        .iter()
        .rev()
        .take(7)
        .filter_map(|(_, v)| *v)
        .collect();
    let baseline: Vec<f64> = samples.iter().filter_map(|(_, v)| *v).collect();
    (avg(&recent), avg(&baseline))
}

/// Render the recent-vs-baseline delta as a percent change. Uniform across
/// every metric in the popup so absolute units (%, tokens, sess) don't leak
/// into the delta column.
pub(crate) fn fmt_delta_pct(recent: Option<f64>, baseline: Option<f64>) -> String {
    match (recent, baseline) {
        (Some(r), Some(b)) if b.abs() > f64::EPSILON => {
            let pct = ((r - b) / b) * 100.0;
            if pct.abs() < 5.0 {
                "≈".to_string()
            } else if pct > 0.0 {
                format!("↑+{pct:.0}%")
            } else {
                format!("↓{pct:.0}%")
            }
        }
        _ => "—".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::trailing_n_months_at;
    use super::{month_bar_cell, month_bar_pair, month_vs_average, share_heights, vs_average};

    // The month under way reads `so far`, not a share of a full month.
    #[test]
    fn the_current_month_reads_so_far() {
        let now = chrono::Local::now()
            .date_naive()
            .format("%Y-%m")
            .to_string();
        assert_eq!(month_vs_average(&now, 5.0, 100.0), "so far");
        assert_eq!(month_vs_average("2000-01", 150.0, 100.0), "+50%");
    }

    // The trend divides as the headline does: subagent-inclusive tokens over
    // user sessions only.
    #[test]
    fn tokens_per_session_counts_subagent_tokens_over_user_sessions() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let user = make_session_with_tokens("p", 100, 0, "claude-sonnet-4");
        let mut sub = make_session_with_tokens("p", 50, 0, "claude-sonnet-4");
        sub.is_subagent = true;
        let v = super::tokens_per_session_value(&make_daily_group(day, vec![user, sub]));
        assert_eq!((v.num, v.den), (150.0, 1.0));
    }

    // Shares on one scale: a month whose cost share exceeds its token share
    // stands taller in cost, whatever the two series' absolute sizes.
    #[test]
    fn paired_bars_compare_shares_on_one_scale() {
        // Costs split 50/50, tokens 80/20 over two months.
        let h = share_heights(&[5000.0, 5000.0], &[80, 20]);
        assert!(
            (h[0].1 - 1.0).abs() < 1e-9,
            "the largest share fills the bar"
        );
        assert!((h[0].0 - 0.625).abs() < 1e-9 && (h[1].0 - 0.625).abs() < 1e-9);
        assert!(h[1].0 > h[1].1, "the dear month's cost bar is taller");
        assert_eq!(share_heights(&[0.0], &[0]), vec![(0.0, 0.0)]);
    }
    use crate::aggregator::days_in_month;

    // The pair is centred in the column so it sits under the `YY-MM` label,
    // cost on the left, tokens on the right, each on its own scale.
    #[test]
    fn a_month_pair_centres_cost_then_tokens_in_its_column() {
        let text: String = month_bar_pair(1.0, 0.0, 0, 4, 7)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, " ██    ");
        let text: String = month_bar_pair(0.0, 1.0, 3, 4, 7)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, "    ██ ");
    }

    #[test]
    fn a_small_month_keeps_a_stub_on_the_bottom_row() {
        assert_eq!(month_bar_cell(0.01, 0, 6), "▁▁");
        assert_eq!(month_bar_cell(0.01, 1, 6), "  ");
        assert_eq!(month_bar_cell(0.0, 0, 6), "  ");
        assert_eq!(month_bar_cell(1.0, 5, 6), "██");
    }

    #[test]
    fn days_in_month_handles_december_and_leap_february() {
        let d = |y, m| chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap(); // lint-ok: date-literal
        assert_eq!(days_in_month(d(2025, 12)), 31);
        assert_eq!(days_in_month(d(2024, 2)), 29);
        assert_eq!(days_in_month(d(2025, 2)), 28);
    }

    #[test]
    fn vs_average_is_signed_and_dashes_without_an_average() {
        assert_eq!(vs_average(150.0, 100.0), "+50%");
        assert_eq!(vs_average(50.0, 100.0), "-50%");
        assert_eq!(vs_average(5.0, 0.0), "-");
    }
    use super::{EcoRowCounts, EcoWording, eco_row};
    use crate::ui::spans_width;

    fn counts(stale: usize, n: usize) -> EcoRowCounts {
        EcoRowCounts {
            active: n,
            configured: n,
            stale,
            mcp: n,
            skills: n,
            commands: n,
            subagents: n,
        }
    }

    // The long form's width depends on the digit counts beside its labels, so
    // the caller measures the composed spans. Rendering cannot check this:
    // ratatui clips at draw time, and the centered row loses its right edge
    // first — the Subagent count.
    #[test]
    fn eco_row_picks_a_form_that_fits() {
        for stale in [0usize, 1, 42, 1_234] {
            for n in [0usize, 1, 9, 10, 999, 123_456] {
                let c = counts(stale, n);
                let short_w = spans_width(&eco_row(&c, EcoWording::Short));
                for row_width in 0..=240usize {
                    let long = eco_row(&c, EcoWording::Long);
                    let chosen = if spans_width(&long) > row_width {
                        eco_row(&c, EcoWording::Short)
                    } else {
                        long
                    };
                    let chosen_w = spans_width(&chosen);
                    assert!(
                        chosen_w <= row_width || chosen_w == short_w,
                        "stale={stale} n={n} row_width={row_width} → {chosen_w}"
                    );
                }
            }
        }
    }

    // The `Sessions` row's four rate figures grow with the data, so the shared
    // popup width is only correct as long as this holds. ratatui clips at draw
    // time, so a render test cannot see the overflow — measure the composition.
    // Whatever the Metrics panel shows, its detail popup shows too, as the
    // same figure: the panel is a preview of the popup.
    #[test]
    fn the_metrics_detail_carries_every_figure_the_panel_previews() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let today = chrono::Local::now().date_naive();
        let mut sub = make_session_with_tokens("proj", 1_000, 0, "claude-opus-5");
        sub.is_subagent = true;
        sub.file_path = std::path::PathBuf::from("/s/sub.jsonl");
        let main = make_session_with_tokens("proj", 2_000, 0, "claude-opus-5");
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            today,
            vec![main, sub],
        )]);
        state
            .models_without_pricing
            .insert("unpriced-model".to_string());
        state.stats.tool_usage.insert("Bash".to_string(), 1);
        state.mcp_status = vec![crate::infrastructure::McpServerStatus {
            name: "a-server-name-longer-than-its-column".to_string(),
            configured: true,
            last_used: None,
            total_calls: 0,
        }];
        let text = |lines: Vec<ratatui::text::Line<'static>>| {
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let panel = text(super::metrics_work_lines(&state));
        let detail = text(super::insights_detail_metrics(&state, 10, 100));

        let tools = format!("{} different used", state.stats.tool_usage.len());
        assert!(detail.contains(&tools), "{detail}");
        assert!(detail.contains("1 stale (unused 30d+)"), "{detail}");
        assert!(detail.contains("Pricing gap") && detail.contains("unpriced-model"));
        assert!(
            detail.contains("Cost per day") && detail.contains(" ┤ "),
            "{detail}"
        );
        // A narrow popup shortens the name column instead of clipping the detail.
        for line in super::insights_detail_metrics(&state, 10, 44) {
            let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if row.contains("never used") {
                assert!(spans_width(&line.spans) <= 44, "{row}");
            }
        }
        assert!(
            detail.contains("a-server-name-longer-than-i… never used"),
            "{detail}"
        );
        let share = crate::text::format_pct(1_000, state.stats.total_tokens.work_tokens());
        assert!(panel.contains(&format!("work tokens  {share}")), "{panel}");
        assert!(detail.contains(&format!("({share} of work)")), "{detail}");
    }

    #[test]
    fn insights_detail_metrics_fit_the_popup_width() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        // Every field that lengthens the row at once: 4-digit session and
        // subagent counts, a 6-digit daily cost, and an hours-long average
        // duration. `format_number` abbreviates the token figures, so raising
        // those alone leaves the row shorter than real data makes it.
        let mut sessions = Vec::new();
        for i in 0..6000u32 {
            let mut sess = make_session_with_tokens("proj", 900_000, 800_000, "claude-opus-5");
            sess.is_subagent = i % 4 != 0;
            sess.file_path = std::path::PathBuf::from(format!("/s/{i}.jsonl"));
            sess.day_first_timestamp = chrono::Utc::now() - chrono::Duration::minutes(1234);
            sess.day_last_timestamp = chrono::Utc::now();
            sessions.push(sess);
        }
        let mut state = crate::test_helpers::helpers::make_test_app_state(vec![make_daily_group(
            date, sessions,
        )]);
        state.total_cost = 987_654.32;
        // `format_number` abbreviates, so the widest token figure is a
        // 3-significant-digit form like `1.23K`. Pick totals that land both
        // rate columns there rather than on a 1-char `0`.
        state.stats.total_tokens.input_tokens = 2_460_000;
        state.stats.total_tokens.output_tokens = 2_460_000;

        let popup = super::super::detail_popup_area(ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 140,
            height: 45,
        });
        let inner = popup.width.saturating_sub(2) as usize;
        for line in super::insights_detail_metrics(&state, 4000, inner) {
            let w = spans_width(&line.spans);
            assert!(
                w <= inner,
                "line is {w} wide, popup inner is {inner}: {line:?}"
            );
        }
    }

    // Strictly narrower, not merely no wider: an equal short form makes the
    // caller's fallback a no-op that `eco_row_picks_a_form_that_fits` would
    // still accept, since nothing narrower exists to compare against.
    #[test]
    fn eco_row_short_form_is_strictly_narrower_than_long() {
        for stale in [0usize, 1, 1_234] {
            for n in [0usize, 9, 123_456] {
                let c = counts(stale, n);
                assert!(
                    spans_width(&eco_row(&c, EcoWording::Short))
                        < spans_width(&eco_row(&c, EcoWording::Long)),
                    "stale={stale} n={n}"
                );
            }
        }
    }

    // The short form is the floor: no narrower variant exists, so it has to fit
    // a conventional 80-column terminal at every count magnitude.
    #[test]
    fn eco_row_short_form_fits_eighty_columns() {
        for stale in [0usize, 1, 42, 1_234] {
            for n in [0usize, 9, 999, 123_456] {
                let w = spans_width(&eco_row(&counts(stale, n), EcoWording::Short));
                assert!(w <= 80, "stale={stale} n={n} → {w}");
            }
        }
    }

    // Every count has to survive the narrow form: dropping one is the failure
    // the short form exists to prevent.
    #[test]
    fn eco_row_keeps_every_count_in_both_forms() {
        let c = EcoRowCounts {
            active: 3,
            configured: 7,
            stale: 2,
            mcp: 11,
            skills: 13,
            commands: 17,
            subagents: 19,
        };
        for wording in [EcoWording::Explained, EcoWording::Long, EcoWording::Short] {
            let text: String = eco_row(&c, wording)
                .iter()
                .map(|sp| sp.content.to_string())
                .collect();
            for n in ["3", "7", "2", "11", "13", "17", "19"] {
                assert!(text.contains(n), "{wording:?} missing {n} in {text:?}");
            }
        }
    }
    use chrono::NaiveDate;
    use std::collections::BTreeMap;

    #[test]
    fn trailing_window_crosses_year_boundary() {
        let costs: BTreeMap<String, f64> = [
            ("2025-12".to_string(), 100.0),
            ("2026-01".to_string(), 200.0),
        ]
        .into_iter()
        .collect();
        let now = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(); // lint-ok: date-literal
        let window = trailing_n_months_at(&costs, 12, now);

        assert_eq!(window.len(), 12);
        assert_eq!(window.first().unwrap().0, "2025-02");
        assert_eq!(window.last().unwrap().0, "2026-01");
        let dec_2025 = window.iter().find(|(k, _)| k == "2025-12").unwrap();
        assert_eq!(dec_2025.1, 100.0);
        let jan_2026 = window.iter().find(|(k, _)| k == "2026-01").unwrap();
        assert_eq!(jan_2026.1, 200.0);
    }

    #[test]
    fn trailing_window_fills_zero_for_missing_months() {
        let costs: BTreeMap<String, f64> = [("2026-03".to_string(), 50.0)].into_iter().collect();
        let now = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(); // lint-ok: date-literal
        let window = trailing_n_months_at(&costs, 12, now);

        assert_eq!(window.len(), 12);
        let zeros = window.iter().filter(|(_, c)| *c == 0.0).count();
        assert_eq!(zeros, 11);
        let with_data = window.iter().find(|(_, c)| *c > 0.0).unwrap();
        assert_eq!(with_data.0, "2026-03");
        assert_eq!(with_data.1, 50.0);
    }
}
