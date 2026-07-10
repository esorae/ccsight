use std::io::IsTerminal;

use crate::aggregator::{CostCalculator, DailyGrouper};
use crate::infrastructure::FileDiscovery;

// ANSI escape sequences. Emitted only when stdout is a TTY so pipes (e.g.
// `ccsight --daily | column -t`) stay clean. Cost tier coloring mirrors
// the TUI's `cost_style` (5-tier scale, same boundaries) so the daily $
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

/// CLI mirror of `ui::cost_style`. Boundaries and tier-to-color mapping
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

pub fn show_daily_costs(limit: usize) {
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    if files.is_empty() {
        println!("No session files found");
        return;
    }

    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let calculator = CostCalculator::global();

    // Detect whether stdout is being piped — if so, drop color and the
    // decorative `----` rules, and print raw integers (no K/M/B suffix)
    // so downstream awk / jq / spreadsheet importers don't have to parse
    // a magnitude suffix. The header stays so the columns are still
    // self-labelled.
    let is_tty = std::io::stdout().is_terminal();
    let use_color = is_tty;
    let c = |code: &'static str| -> &'static str { if use_color { code } else { "" } };

    println!(
        "{bold}{:>12} {:>12} {:>12} {:>12} {:>12} {:>10}{reset}",
        "Date",
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
            "-".repeat(74),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }

    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_w: u64 = 0;
    let mut total_cache_r: u64 = 0;
    let mut total_cost: f64 = 0.0;

    for group in daily_groups.iter().rev() {
        let mut day_input: u64 = 0;
        let mut day_output: u64 = 0;
        let mut day_cache_w: u64 = 0;
        let mut day_cache_r: u64 = 0;
        let mut day_cost: f64 = 0.0;

        // Include subagents so `--daily` total matches the TUI Overview /
        // Costs panel. Subagent dispatch is real Anthropic spend tied to the
        // day it ran on.
        for session in &group.sessions {
            for (model, tokens) in &session.day_tokens_by_model {
                day_input += tokens.input_tokens;
                day_output += tokens.output_tokens;
                day_cache_w += tokens.cache_creation_tokens;
                day_cache_r += tokens.cache_read_tokens;

                day_cost += calculator
                    .calculate_cost(tokens, Some(model.as_str()))
                    .unwrap_or(0.0);
            }
        }

        // Skip days with no billable activity. They live in `daily_groups`
        // because the JSONLs contain timestamped non-billing entries (API
        // errors, canceled prompts, system / file-snapshot rows) but they
        // render as noise in a cost summary.
        if day_input == 0 && day_output == 0 && day_cache_w == 0 && day_cache_r == 0 {
            continue;
        }

        total_input += day_input;
        total_output += day_output;
        total_cache_w += day_cache_w;
        total_cache_r += day_cache_r;
        total_cost += day_cost;

        let reset = c(C_RESET);
        // Cost cell is tier-colored (matches TUI Costs panel). Bold lifts
        // the bottom-line number above the surrounding row.
        let cost_col = c(cost_color(day_cost));
        let fmt = |n: u64| {
            if is_tty {
                crate::format_number(n)
            } else {
                n.to_string()
            }
        };
        println!(
            "{cyan}{:>12}{reset} {:>12} {:>12} {dim}{:>12} {:>12}{reset} {bold}{cost_col}{:>10}{reset}",
            group.date.format("%Y-%m-%d"),
            fmt(day_input),
            fmt(day_output),
            fmt(day_cache_w),
            fmt(day_cache_r),
            format!("${:.2}", day_cost),
            cyan = c(C_CYAN),
            dim = c(C_DIM),
            bold = c(C_BOLD),
        );
    }

    if is_tty {
        println!(
            "{dim}{}{reset}",
            "-".repeat(74),
            dim = c(C_DIM),
            reset = c(C_RESET)
        );
    }
    let total_cost_col = c(cost_color(total_cost));
    let fmt = |n: u64| {
        if is_tty {
            crate::format_number(n)
        } else {
            n.to_string()
        }
    };
    println!(
        "{bold}{:>12}{reset} {bold}{:>12} {:>12} {:>12} {:>12}{reset} {bold}{total_cost_col}{:>10}{reset}",
        "Total",
        fmt(total_input),
        fmt(total_output),
        fmt(total_cache_w),
        fmt(total_cache_r),
        format!("${:.2}", total_cost),
        bold = c(C_BOLD),
        reset = c(C_RESET),
    );
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
            for (model, tokens) in &session.day_tokens_by_model {
                entry.0 += tokens.input_tokens;
                entry.1 += tokens.output_tokens;
                entry.2 += tokens.cache_creation_tokens;
                entry.3 += tokens.cache_read_tokens;
                entry.4 += calculator
                    .calculate_cost(tokens, Some(model.as_str()))
                    .unwrap_or(0.0);
            }
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
                "cost_usd": cost,
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
            "cost_usd": total.4,
        },
    })
}

/// `--json` with `--daily` / `--weekly` / `--monthly`: same buckets and
/// labels as the tables, one JSON document on stdout.
pub fn show_costs_json(period: &str, limit: usize) {
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let rows = match period {
        "weekly" => aggregate_buckets(&daily_groups, week_label),
        "monthly" => aggregate_buckets(&daily_groups, month_label),
        _ => aggregate_buckets(&daily_groups, |d| d.format("%Y-%m-%d").to_string()),
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
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    if files.is_empty() {
        println!("No session files found");
        return;
    }
    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let rows = aggregate_buckets(&daily_groups, week_label);
    print_bucket_rows(rows, "Week", 22);
}

/// Group days into calendar months. Label `YYYY-MM`.
pub fn show_monthly_costs(limit: usize) {
    let files = match FileDiscovery::find_jsonl_files_with_limit(limit) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Error finding files: {e}");
            return;
        }
    };
    if files.is_empty() {
        println!("No session files found");
        return;
    }
    let mut cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &mut cache);
    let rows = aggregate_buckets(&daily_groups, month_label);
    print_bucket_rows(rows, "Month", 12);
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
