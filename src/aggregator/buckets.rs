//! Time-bucket aggregations (monthly cost / monthly tokens / weekday avg)
//! shared by both compact Insights panels and their detail popups.

use std::collections::{BTreeMap, HashMap};

use chrono::{Datelike, NaiveDate, Weekday};

use crate::aggregator::{DailyGroup, SessionInfo};

/// Aggregate per-month cost (key = "YYYY-MM") from `state.daily_costs`.
/// Used by both the compact Insights panel and its detail popup.
pub(crate) fn aggregate_monthly_costs(daily_costs: &[(NaiveDate, f64)]) -> BTreeMap<String, f64> {
    let mut map = BTreeMap::new();
    for (date, cost) in daily_costs {
        let key = format!("{}-{:02}", date.year(), date.month());
        *map.entry(key).or_insert(0.0) += cost;
    }
    map
}

/// Aggregate per-month work-token totals (key = "YYYY-MM"), subagents included
/// like the month's cost beside it.
pub(crate) fn aggregate_monthly_tokens(daily_groups: &[DailyGroup]) -> BTreeMap<String, u64> {
    let mut map = BTreeMap::new();
    for group in daily_groups {
        let key = format!("{}-{:02}", group.date.year(), group.date.month());
        let tokens: u64 = group.sessions.iter().map(SessionInfo::work_tokens).sum();
        *map.entry(key).or_insert(0) += tokens;
    }
    map
}

/// Per-weekday work-token average (token sum / occurrences-of-that-weekday),
/// subagents included. Returned map always has every `Weekday` key.
pub(crate) fn aggregate_weekday_avg(
    daily_groups: &[DailyGroup],
    calendar_days: usize,
    first_date: NaiveDate,
) -> HashMap<Weekday, u64> {
    const ALL_WEEKDAYS: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    // Include subagent sessions to match the Costs panel and Insights
    // `tokens/day` accounting, so all `K/day` values agree.
    let mut work: HashMap<Weekday, u64> = HashMap::new();
    for group in daily_groups {
        let tokens: u64 = group.sessions.iter().map(SessionInfo::work_tokens).sum();
        *work.entry(group.date.weekday()).or_insert(0) += tokens;
    }
    let mut avg = HashMap::with_capacity(7);
    for wd in ALL_WEEKDAYS {
        let count = weekday_occurrence_count(calendar_days, first_date, wd);
        avg.insert(wd, work.get(&wd).copied().unwrap_or(0) / count as u64);
    }
    avg
}

/// How many times a given weekday appears across `calendar_days` starting at
/// `first_date`. Used by [`aggregate_weekday_avg`] (and the Insights weekly
/// detail's "Most active" caption) to convert raw weekday tokens into an
/// average per occurrence. Always returns at least 1 to avoid div-by-zero.
pub(crate) fn weekday_occurrence_count(
    calendar_days: usize,
    first_date: NaiveDate,
    weekday: Weekday,
) -> u32 {
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

pub(crate) fn days_in_month(date: NaiveDate) -> u32 {
    let (y, m) = (date.year(), date.month());
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    next.and_then(|d| d.pred_opt()).map_or(30, |d| d.day())
}

/// One Monday-started week: its cost and work tokens, subagents included.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WeekBucket {
    pub start: NaiveDate,
    pub cost: f64,
    pub tokens: u64,
}

/// The `n` weeks ending with `today`'s, oldest first, idle weeks included so
/// the columns are evenly spaced in time.
pub(crate) fn trailing_weeks(
    daily_groups: &[DailyGroup],
    daily_costs: &[(NaiveDate, f64)],
    today: NaiveDate,
    n: usize,
) -> Vec<WeekBucket> {
    let this_week = week_start(today);
    let mut weeks: Vec<WeekBucket> = (0..n as i64)
        .rev()
        .map(|k| WeekBucket {
            start: this_week - chrono::Duration::weeks(k),
            cost: 0.0,
            tokens: 0,
        })
        .collect();
    let slot = |date: NaiveDate| {
        let back = (this_week - week_start(date)).num_weeks();
        (0..n as i64).contains(&back).then(|| n - 1 - back as usize)
    };
    for (date, cost) in daily_costs {
        if let Some(i) = slot(*date) {
            weeks[i].cost += cost;
        }
    }
    for group in daily_groups {
        if let Some(i) = slot(group.date) {
            weeks[i].tokens += group
                .sessions
                .iter()
                .map(SessionInfo::work_tokens)
                .sum::<u64>();
        }
    }
    weeks
}

fn week_start(date: NaiveDate) -> NaiveDate {
    date - chrono::Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

/// Average work tokens per weekday (Monday = 0) and clock hour over the days
/// before `today`, idle days included. Today is left out: a day still under
/// way would drag its weekday's average down.
pub(crate) fn weekday_hour_avg(daily_groups: &[DailyGroup], today: NaiveDate) -> [[u64; 24]; 7] {
    let mut total = [[0u64; 24]; 7];
    let Some(first) = daily_groups
        .iter()
        .map(|g| g.date)
        .filter(|d| *d < today)
        .min()
    else {
        return total;
    };
    for group in daily_groups.iter().filter(|g| g.date < today) {
        let wd = group.date.weekday().num_days_from_monday() as usize;
        for session in &group.sessions {
            for (hour, tokens) in &session.day_hourly_work_tokens {
                if let Some(cell) = total[wd].get_mut(usize::from(*hour)) {
                    *cell += tokens;
                }
            }
        }
    }
    let days = (today - first).num_days() as usize;
    for (wd, row) in total.iter_mut().enumerate() {
        let weekday = Weekday::try_from(wd as u8).unwrap_or(Weekday::Mon);
        let occurrences = u64::from(weekday_occurrence_count(days, first, weekday));
        for cell in row.iter_mut() {
            *cell /= occurrences;
        }
    }
    total
}

/// Today's work tokens through `hour`, and what its weekday averages through
/// the same hour. `None` until the groups hold a day before today.
pub(crate) fn same_weekday_so_far(
    daily_groups: &[DailyGroup],
    today: NaiveDate,
    hour: u8,
) -> Option<(u64, u64)> {
    if !daily_groups.iter().any(|g| g.date < today) {
        return None;
    }
    let so_far = daily_groups
        .iter()
        .filter(|g| g.date == today)
        .flat_map(|g| &g.sessions)
        .flat_map(|s| &s.day_hourly_work_tokens)
        .filter(|(h, _)| **h <= hour)
        .map(|(_, t)| *t)
        .sum();
    let row =
        weekday_hour_avg(daily_groups, today)[today.weekday().num_days_from_monday() as usize];
    let typical = row.iter().take(usize::from(hour) + 1).sum();
    Some((so_far, typical))
}

/// A day's work against what its weekday averages before it. On `today` the
/// day is still under way, so both sides stop at `through` (the current hour)
/// and a morning is not judged by a whole day; a past day compares in full.
pub(crate) struct VsUsual {
    pub day: u64,
    pub usual: u64,
    pub through: Option<u8>,
}

impl VsUsual {
    pub(crate) fn pct(&self) -> Option<u64> {
        (self.day * 100).checked_div(self.usual)
    }
}

/// `None` until the view holds a day before `date`.
pub(crate) fn day_vs_usual(
    daily_groups: &[DailyGroup],
    date: NaiveDate,
    today: NaiveDate,
    hour: u8,
) -> Option<VsUsual> {
    let through = (date == today).then_some(hour);
    let (day, usual) = same_weekday_so_far(daily_groups, date, through.unwrap_or(23))?;
    Some(VsUsual {
        day,
        usual,
        through,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap() // lint-ok: date-literal
    }

    fn at_hour(date: NaiveDate, hour: u8, tokens: u64) -> DailyGroup {
        let mut s = crate::test_helpers::helpers::make_session("~/p", None, None);
        s.day_hourly_work_tokens.insert(hour, tokens);
        s.day_input_tokens = tokens;
        s.day_output_tokens = 0;
        crate::test_helpers::helpers::make_daily_group(date, vec![s])
    }

    #[test]
    fn trailing_weeks_bucket_by_monday_and_keep_idle_weeks() {
        // A Wednesday, so its week starts two days back on Monday.
        let today = day(2025, 3, 12);
        let groups = [at_hour(today, 9, 100), at_hour(day(2025, 3, 2), 9, 40)];
        let costs = [(today, 2.0), (day(2025, 3, 2), 1.0)];
        let weeks = trailing_weeks(&groups, &costs, today, 3);
        let starts: Vec<_> = weeks.iter().map(|w| w.start).collect();
        assert_eq!(
            starts,
            [day(2025, 2, 24), day(2025, 3, 3), day(2025, 3, 10)]
        );
        assert_eq!(
            (weeks[0].tokens, weeks[1].tokens, weeks[2].tokens),
            (40, 0, 100)
        );
        assert!((weeks[0].cost - 1.0).abs() < 1e-9 && weeks[1].cost == 0.0);
    }

    #[test]
    fn the_same_weekday_comparison_leaves_today_out_of_the_average() {
        let today = day(2025, 3, 12);
        let groups = [
            at_hour(today, 9, 500),
            at_hour(day(2025, 3, 5), 9, 100),
            at_hour(day(2025, 3, 5), 20, 999),
        ];
        // Two prior Wednesdays (one active, one idle) average 100/2 by
        // 10:00; the evening entry lies past the hour and does not count.
        let first = at_hour(day(2025, 2, 26), 3, 0);
        let mut all = groups.to_vec();
        all.push(first);
        assert_eq!(same_weekday_so_far(&all, today, 10), Some((500, 50)));
        assert_eq!(same_weekday_so_far(&groups[..1], today, 10), None);
    }

    // Today stops at the current hour on both sides; a past day compares in
    // full, its average built only from the weekdays before it.
    #[test]
    fn a_day_against_usual_is_cut_at_now_only_for_today() {
        let today = day(2025, 3, 12);
        let past = day(2025, 3, 5);
        let all = [
            at_hour(today, 9, 500),
            at_hour(past, 9, 100),
            at_hour(past, 20, 300),
            at_hour(day(2025, 2, 26), 9, 60),
            at_hour(day(2025, 2, 26), 20, 140),
        ];
        let now = day_vs_usual(&all, today, today, 10).unwrap();
        // Two prior Wednesdays by 10:00: (100 + 60) / 2.
        assert_eq!((now.day, now.usual, now.through), (500, 80, Some(10)));
        assert_eq!(now.pct(), Some(625));
        let then = day_vs_usual(&all, past, today, 10).unwrap();
        // The whole day, against the one Wednesday before it.
        assert_eq!((then.day, then.usual, then.through), (400, 200, None));
        assert_eq!(
            day_vs_usual(&all[..1], today, today, 10).map(|v| v.day),
            None
        );
    }

    // The month's tokens sit beside its cost, which counts subagents.
    #[test]
    fn monthly_tokens_count_subagents_like_the_cost() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let day = NaiveDate::from_ymd_opt(2025, 3, 4).unwrap(); // lint-ok: date-literal
        let user = make_session_with_tokens("~/p", 100, 0, "claude-sonnet-4");
        let mut sub = make_session_with_tokens("~/p", 50, 0, "claude-sonnet-4");
        sub.is_subagent = true;
        let months = aggregate_monthly_tokens(&[make_daily_group(day, vec![user, sub])]);
        assert_eq!(months.get("2025-03"), Some(&150));
    }
}
