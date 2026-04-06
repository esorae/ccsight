pub(crate) mod grouping;
pub(crate) mod pricing;
pub(crate) mod stats;

pub use grouping::*;
pub use pricing::*;
pub use stats::{CacheStats, Stats, StatsAggregator, TokenStats};

pub(crate) fn extract_project_name(entries: &[crate::domain::LogEntry]) -> Option<String> {
    entries
        .iter().find_map(|e| e.cwd.as_ref())
        .map(|cwd| format_project_path(cwd))
}

pub(crate) fn format_project_path(path: &str) -> String {
    let stripped = path
        .strip_prefix("/Users/")
        .or_else(|| path.strip_prefix("/home/"));
    if let Some(stripped) = stripped {
        if let Some(rest) = stripped.split_once('/') {
            format!("~/{}", rest.1)
        } else {
            format!("~/{stripped}")
        }
    } else {
        path.to_string()
    }
}
