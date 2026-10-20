use std::io::IsTerminal;

use crate::aggregator::{CostCalculator, DailyGrouper};
use crate::infrastructure::FileDiscovery;

// ANSI escape sequences. Emitted only when stdout is a TTY so pipes (e.g.
// `ccsight --daily | column -t`) stay clean. Cost tier coloring mirrors
// the TUI's `cost_color` tier scale (5-tier scale, same boundaries) so the daily $
// reads the same in CLI and TUI.
const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_DIM: &str = "\x1b[2m";
const C_CYAN: &str = "\x1b[36m";
const C_GREEN: &str = "\x1b[32m"; // SUCCESS — lowest cost tier
const C_YELLOW: &str = "\x1b[33m"; // WARNING
const C_BR_RED: &str = "\x1b[91m"; // ERROR
const C_RED: &str = "\x1b[31m"; // DANGER
const C_BR_MAGENTA: &str = "\x1b[95m"; // CRITICAL — highest tier

/// CLI mirror of the TUI's `cost_color` tier scale. Boundaries and tier-to-color mapping
/// must stay in lockstep with the TUI — when the TUI rebalances tiers,
/// update both. Tiers from low to high spend: SUCCESS / WARNING / ERROR
/// / DANGER / CRITICAL.
fn cost_color(amount: f64) -> &'static str {
    let c = amount.max(0.0);
    if c > 300.0 {
        C_BR_MAGENTA
    } else if c > 100.0 {
        C_RED
    } else if c > 60.0 {
        C_BR_RED
    } else if c > 20.0 {
        C_YELLOW
    } else {
        C_GREEN
    }
}

/// The day groups over `limit` files, or `None` after saying why there are
/// none. `report_empty` prints the no-files notice a table wants; `--json`
/// prints its empty document instead.
fn load_groups(limit: usize, report_empty: bool) -> Option<Vec<crate::aggregator::DailyGroup>> {
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return None;
        }
    };
    if report_empty && files.is_empty() {
        println!("No session files found");
        return None;
    }
    let mut cache = crate::infrastructure::Cache::load().ok();
    Some(DailyGrouper::group_by_date_with_shared_cache(
        &files, &mut cache,
    ))
}

fn day_label(d: chrono::NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

pub fn show_daily_costs(limit: usize) {
    let Some(groups) = load_groups(limit, true) else {
        return;
    };
    print_bucket_rows(aggregate_buckets(&groups, day_label), "Date", 12);
}

/// Aggregate `daily_groups` into buckets keyed by a label string (e.g. ISO
/// week or year-month). Returns the labelled rows in insertion order so the
/// caller's date sort propagates through.
fn aggregate_buckets<F>(
    daily_groups: &[crate::aggregator::DailyGroup],
    label: F,
) -> Vec<(String, u64, u64, u64, u64, f64)>
where
    F: Fn(chrono::NaiveDate) -> String,
{
    use std::collections::BTreeMap;
    let calculator = CostCalculator::global();
    // Use BTreeMap keyed on the label; since labels are derived from
    // dates and dates are processed newest-first, we sort labels asc
    // later for chronological output (matches --daily).
    let mut acc: BTreeMap<String, (u64, u64, u64, u64, f64)> = BTreeMap::new();
    for group in daily_groups {
        let key = label(group.date);
        let entry = acc.entry(key).or_insert((0, 0, 0, 0, 0.0));
        for session in &group.sessions {
            let t = session.day_token_total();
            entry.0 += t.input_tokens;
            entry.1 += t.output_tokens;
            entry.2 += t.cache_creation_tokens;
            entry.3 += t.cache_read_tokens;
            entry.4 += session.cost(calculator);
        }
    }
    acc.into_iter()
        .map(|(k, (a, b, c, d, e))| (k, a, b, c, d, e))
        .collect()
}

fn print_bucket_rows(
    rows: Vec<(String, u64, u64, u64, u64, f64)>,
    label_header: &str,
    label_w: usize,
) {
    let is_tty = std::io::stdout().is_terminal();
    let use_color = is_tty;
    let c = |code: &'static str| -> &'static str { if use_color { code } else { "" } };
    let fmt = |n: u64| {
        if is_tty {
            crate::format_number(n)
        } else {
            n.to_string()
        }
    };
    // Total table width = label + 5 numeric columns + 5 separators.
    let total_w = label_w + 12 * 4 + 10 + 5;

    println!(
        "{bold}{:>label_w$} {:>12} {:>12} {:>12} {:>12} {:>10}{reset}",
        label_header,
        "Input",
        "Output",
        "CacheW",
        "CacheR",
        "Cost",
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(total_w),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }

    let mut total = (0u64, 0u64, 0u64, 0u64, 0.0f64);
    for (label, input, output, cw, cr, cost) in &rows {
        if *input == 0 && *output == 0 && *cw == 0 && *cr == 0 {
            continue;
        }
        total.0 += input;
        total.1 += output;
        total.2 += cw;
        total.3 += cr;
        total.4 += cost;
        let cost_col = c(cost_color(*cost));
        let reset = c(C_RESET);
        println!(
            "{cyan}{:>label_w$}{reset} {:>12} {:>12} {dim}{:>12} {:>12}{reset} {bold}{cost_col}{:>10}{reset}",
            label,
            fmt(*input),
            fmt(*output),
            fmt(*cw),
            fmt(*cr),
            format!("${:.2}", cost),
            cyan = c(C_CYAN),
            dim = c(C_DIM),
            bold = c(C_BOLD),
        );
    }
    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(total_w),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }
    let total_cost_col = c(cost_color(total.4));
    println!(
        "{bold}{:>label_w$}{reset} {bold}{:>12} {:>12} {:>12} {:>12}{reset} {bold}{total_cost_col}{:>10}{reset}",
        "Total",
        fmt(total.0),
        fmt(total.1),
        fmt(total.2),
        fmt(total.3),
        format!("${:.2}", total.4),
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
}

/// Pure JSON assembly for `--json`: zero rows are skipped and the total is
/// summed over kept rows only, matching the table renderers exactly so the
/// two output shapes can never disagree.
fn buckets_to_json(period: &str, rows: &[(String, u64, u64, u64, u64, f64)]) -> serde_json::Value {
    let mut total = (0u64, 0u64, 0u64, 0u64, 0.0f64);
    let json_rows: Vec<serde_json::Value> = rows
        .iter()
        .filter(|(_, input, output, cw, cr, _)| *input != 0 || *output != 0 || *cw != 0 || *cr != 0)
        .map(|(label, input, output, cw, cr, cost)| {
            total.0 += input;
            total.1 += output;
            total.2 += cw;
            total.3 += cr;
            total.4 += cost;
            serde_json::json!({
                "label": label,
                "input_tokens": input,
                "output_tokens": output,
                "cache_write_tokens": cw,
                "cache_read_tokens": cr,
                "cost_usd": crate::text::round_cents(*cost),
            })
        })
        .collect();
    serde_json::json!({
        "period": period,
        "rows": json_rows,
        "total": {
            "input_tokens": total.0,
            "output_tokens": total.1,
            "cache_write_tokens": total.2,
            "cache_read_tokens": total.3,
            "cost_usd": crate::text::round_cents(total.4),
        },
    })
}

/// `--json` with `--daily` / `--weekly` / `--monthly`: same buckets and
/// labels as the tables, one JSON document on stdout.
pub fn show_costs_json(period: &str, limit: usize) {
    let Some(daily_groups) = load_groups(limit, false) else {
        return;
    };
    let rows = match period {
        "weekly" => aggregate_buckets(&daily_groups, week_label),
        "monthly" => aggregate_buckets(&daily_groups, month_label),
        _ => aggregate_buckets(&daily_groups, day_label),
    };
    println!("{}", buckets_to_json(period, &rows));
}

/// ISO 8601-1:2019 §5.5.4 time-interval notation for a Mon-Sun week: two
/// endpoints joined by `/`, the end abbreviated to whatever differs from
/// the start. Shared by the weekly table and `--json` so labels agree.
fn week_label(d: chrono::NaiveDate) -> String {
    use chrono::{Datelike, Duration};
    let monday = d - Duration::days(d.weekday().num_days_from_monday() as i64);
    let sunday = monday + Duration::days(6);
    let end_fmt = if monday.year() == sunday.year() {
        if monday.month() == sunday.month() {
            "%d"
        } else {
            "%m-%d"
        }
    } else {
        "%Y-%m-%d"
    };
    format!("{}/{}", monday.format("%Y-%m-%d"), sunday.format(end_fmt))
}

fn month_label(d: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    format!("{}-{:02}", d.year(), d.month())
}

/// Group days into ISO weeks (Mon-Sun). The label spells out the date
/// range so users don't have to mentally convert `2026-W22` into "what
/// dates did that cover" — format: `Wnn  mm-dd–mm-dd` (en dash range
/// inside the same year, matching the Daily detail popup's weekly view).
pub fn show_weekly_costs(limit: usize) {
    let Some(groups) = load_groups(limit, true) else {
        return;
    };
    print_bucket_rows(aggregate_buckets(&groups, week_label), "Week", 22);
}

/// Group days into calendar months. Label `YYYY-MM`.
pub fn show_monthly_costs(limit: usize) {
    let Some(groups) = load_groups(limit, true) else {
        return;
    };
    print_bucket_rows(aggregate_buckets(&groups, month_label), "Month", 12);
}

#[cfg(test)]
mod tests {
    //! Fixture arithmetic: two non-zero rows (10+5 input, 20+1 output,
    //! 30 cache-write, 40 cache-read, $1.5+$0.25) plus one all-zero row
    //! that must be skipped by both the table and the JSON path.
    use super::*;

    fn rows() -> Vec<(String, u64, u64, u64, u64, f64)> {
        // Labels are opaque to the JSON assembly; day-N stands in for dates.
        vec![
            ("day-1".to_string(), 10, 20, 30, 40, 1.5),
            ("day-2".to_string(), 0, 0, 0, 0, 0.0),
            ("day-3".to_string(), 5, 1, 0, 0, 0.25),
        ]
    }

    #[test]
    fn json_skips_zero_rows_and_totals_kept_rows() {
        let v = buckets_to_json("daily", &rows());
        assert_eq!(v["period"], "daily");
        let out_rows = v["rows"].as_array().unwrap();
        assert_eq!(out_rows.len(), 2);
        assert_eq!(out_rows[0]["label"], "day-1");
        assert_eq!(out_rows[1]["label"], "day-3");
        assert_eq!(v["total"]["input_tokens"], 15);
        assert_eq!(v["total"]["output_tokens"], 21);
        assert_eq!(v["total"]["cache_write_tokens"], 30);
        assert_eq!(v["total"]["cache_read_tokens"], 40);
        assert!((v["total"]["cost_usd"].as_f64().unwrap() - 1.75).abs() < 1e-9);
    }

    // Cents, like the MCP tools: a raw float sum prints long binary tails.
    #[test]
    fn json_costs_are_rounded_to_cents() {
        let v = buckets_to_json(
            "daily",
            &[
                ("day-1".to_string(), 1, 1, 0, 0, 1.234_567),
                ("day-2".to_string(), 1, 1, 0, 0, 0.1),
            ],
        );
        assert_eq!(v["rows"][0]["cost_usd"], 1.23);
        assert_eq!(v["total"]["cost_usd"], 1.33);
    }

    #[test]
    fn json_row_carries_full_schema() {
        let v = buckets_to_json("weekly", &rows());
        let row = &v["rows"][0];
        for key in [
            "label",
            "input_tokens",
            "output_tokens",
            "cache_write_tokens",
            "cache_read_tokens",
            "cost_usd",
        ] {
            assert!(row.get(key).is_some(), "missing key: {key}");
        }
    }

    #[test]
    fn week_label_matches_iso_interval_notation() {
        // Mid-week date collapses to its Mon-Sun span; same month → day-only end.
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(); // lint-ok: date-literal
        assert_eq!(week_label(d), "2026-01-05/11"); // lint-ok: date-literal
        // Month boundary keeps month in the end label.
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 30).unwrap(); // lint-ok: date-literal
        assert_eq!(week_label(d), "2026-01-26/02-01"); // lint-ok: date-literal
    }

    #[test]
    fn month_label_is_year_dash_month() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 3, 9).unwrap(); // lint-ok: date-literal
        assert_eq!(month_label(d), "2026-03");
    }
}
