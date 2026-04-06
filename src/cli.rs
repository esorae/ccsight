use crate::aggregator::{CostCalculator, DailyGrouper};
use crate::infrastructure::FileDiscovery;

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

    let cache = crate::infrastructure::Cache::load().ok();
    let daily_groups = DailyGrouper::group_by_date_with_shared_cache(&files, &cache);
    let calculator = CostCalculator::global();

    println!(
        "{:>12} {:>12} {:>12} {:>12} {:>12} {:>10}",
        "Date", "Input", "Output", "CacheW", "CacheR", "Cost($)"
    );
    println!("{}", "-".repeat(74));

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

        for session in group.sessions.iter().filter(|s| !s.is_subagent) {
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

        total_input += day_input;
        total_output += day_output;
        total_cache_w += day_cache_w;
        total_cache_r += day_cache_r;
        total_cost += day_cost;

        println!(
            "{:>12} {:>12} {:>12} {:>12} {:>12} {:>10.2}",
            group.date.format("%Y-%m-%d"),
            crate::format_number(day_input),
            crate::format_number(day_output),
            crate::format_number(day_cache_w),
            crate::format_number(day_cache_r),
            day_cost
        );
    }

    println!("{}", "-".repeat(74));
    println!(
        "{:>12} {:>12} {:>12} {:>12} {:>12} {:>10.2}",
        "Total",
        crate::format_number(total_input),
        crate::format_number(total_output),
        crate::format_number(total_cache_w),
        crate::format_number(total_cache_r),
        total_cost
    );
}
