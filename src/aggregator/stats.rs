use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::domain::{EntryType, LogEntry, Usage};
use crate::infrastructure::{
    get_file_modified_secs, get_file_size, Cache, CachedDailyStats, CachedFileStats,
    CachedTokenStats,
};
use crate::parser::JsonlParser;

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub total_entries: usize,
    pub total_tokens: TokenStats,
    pub tool_usage: HashMap<String, usize>,
    pub model_usage: HashMap<String, usize>,
    pub model_tokens: HashMap<String, TokenStats>,
    pub daily_activity: HashMap<NaiveDate, u64>,
    pub daily_work_activity: HashMap<NaiveDate, u64>,
    pub project_stats: HashMap<String, ProjectStats>,
    pub hourly_activity: HashMap<u8, u64>,
    pub hourly_work_activity: HashMap<u8, u64>,
    pub weekday_activity: HashMap<Weekday, u64>,
    pub weekday_work_activity: HashMap<Weekday, u64>,
    pub tool_error_count: usize,
    pub tool_success_count: usize,
    pub sessions_with_summary: usize,
    pub total_sessions_count: usize,
    pub branch_stats: HashMap<String, BranchStats>,
    pub language_usage: HashMap<String, usize>,
    pub extension_usage: HashMap<String, usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BranchStats {
    pub session_count: usize,
    pub total_duration_mins: i64,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectStats {
    pub sessions: usize,
    pub tokens: u64,
    pub work_tokens: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl TokenStats {
    pub fn work_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn all_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    pub fn add(&mut self, usage: &Usage) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_tokens += usage.cache_creation_input_tokens;
        self.cache_read_tokens += usage.cache_read_input_tokens;
    }
}

pub struct StatsAggregator;

#[derive(Debug, Clone, Default)]
struct DailyStats {
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    input_tokens: u64,
    output_tokens: u64,
    tokens_by_model: HashMap<String, TokenStats>,
    hourly_activity: HashMap<u8, u64>,
    hourly_work_activity: HashMap<u8, u64>,
    tool_usage: HashMap<String, usize>,
    language_usage: HashMap<String, usize>,
    extension_usage: HashMap<String, usize>,
}

#[derive(Debug, Clone, Default)]
struct FileStats {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    tool_usage: HashMap<String, usize>,
    model_usage: HashMap<String, usize>,
    model_tokens: HashMap<String, TokenStats>,
    session_date: Option<NaiveDate>,
    session_id: Option<String>,
    git_branch: Option<String>,
    first_timestamp: Option<DateTime<Utc>>,
    last_timestamp: Option<DateTime<Utc>>,
    summary: Option<String>,
    custom_title: Option<String>,
    model: Option<String>,
    is_subagent: bool,
    daily_stats: HashMap<NaiveDate, DailyStats>,
    hourly_activity: HashMap<u8, u64>,
    hourly_work_activity: HashMap<u8, u64>,
    weekday_activity: HashMap<u8, u64>,
    weekday_work_activity: HashMap<u8, u64>,
    tool_error_count: usize,
    tool_success_count: usize,
    session_duration_mins: Option<i64>,
    language_usage: HashMap<String, usize>,
    extension_usage: HashMap<String, usize>,
}

pub struct CacheStats {
    pub cached_files: usize,
    pub parsed_files: usize,
}

impl StatsAggregator {

    pub fn aggregate_with_shared_cache(files: &[PathBuf], mut cache: Cache) -> (Stats, CacheStats) {
        let mut stats = Stats::default();

        let mut cache_stats = CacheStats {
            cached_files: 0,
            parsed_files: 0,
        };

        for file in files {
            if cache.is_valid(file)
                && let Some(cached) = cache.get(file) {
                    let project_name = cached.project_name.clone();
                    Self::merge_cached_stats(&mut stats, cached, &project_name);
                    cache_stats.cached_files += 1;
                    continue;
                }

            if let Ok(entries) = JsonlParser::parse_file(file) {
                let project_name = Self::extract_project_name_from_entries(&entries);
                let file_stats =
                    Self::aggregate_file_entries(&mut stats, &entries, &project_name, file);

                let model_tokens_cached: HashMap<String, CachedTokenStats> = file_stats
                    .model_tokens
                    .iter()
                    .map(|(model, ts)| (model.clone(), ts.clone()))
                    .collect();

                let daily_stats_cached: HashMap<String, CachedDailyStats> = file_stats
                    .daily_stats
                    .into_iter()
                    .map(|(date, ds)| {
                        let tokens_by_model = ds
                            .tokens_by_model
                            .iter()
                            .map(|(model, ts)| (model.clone(), ts.clone()))
                            .collect();
                        (
                            date.to_string(),
                            CachedDailyStats {
                                first_timestamp: ds.first_timestamp,
                                last_timestamp: ds.last_timestamp,
                                input_tokens: ds.input_tokens,
                                output_tokens: ds.output_tokens,
                                tokens_by_model,
                                hourly_activity: ds.hourly_activity,
                                hourly_work_activity: ds.hourly_work_activity,
                                tool_usage: ds.tool_usage,
                                language_usage: ds.language_usage,
                                extension_usage: ds.extension_usage,
                            },
                        )
                    })
                    .collect();

                let cached_file_stats = CachedFileStats {
                    modified_secs: get_file_modified_secs(file),
                    file_size: get_file_size(file),
                    entry_count: entries.len(),
                    input_tokens: file_stats.input_tokens,
                    output_tokens: file_stats.output_tokens,
                    cache_creation_tokens: file_stats.cache_creation_tokens,
                    cache_read_tokens: file_stats.cache_read_tokens,
                    tool_usage: file_stats.tool_usage,
                    model_usage: file_stats.model_usage,
                    model_tokens: model_tokens_cached,
                    session_date: file_stats.session_date,
                    project_name: project_name.clone(),
                    session_id: file_stats.session_id,
                    git_branch: file_stats.git_branch.clone(),
                    first_timestamp: file_stats.first_timestamp,
                    last_timestamp: file_stats.last_timestamp,
                    summary: file_stats.summary.clone(),
                    custom_title: file_stats.custom_title.clone(),
                    model: file_stats.model,
                    is_subagent: file_stats.is_subagent,
                    daily_stats: daily_stats_cached,
                    hourly_activity: file_stats.hourly_activity,
                    hourly_work_activity: file_stats.hourly_work_activity,
                    weekday_activity: file_stats.weekday_activity,
                    weekday_work_activity: file_stats.weekday_work_activity,
                    tool_error_count: file_stats.tool_error_count,
                    tool_success_count: file_stats.tool_success_count,
                    session_duration_mins: file_stats.session_duration_mins,
                    language_usage: file_stats.language_usage,
                    extension_usage: file_stats.extension_usage,
                };

                Self::apply_productivity_stats(&mut stats, &cached_file_stats);
                cache.insert(file, cached_file_stats);
                cache_stats.parsed_files += 1;
            }
        }

        let _ = cache.save();

        (stats, cache_stats)
    }

    fn merge_cached_stats(
        stats: &mut Stats,
        cached: &CachedFileStats,
        project_name: &Option<String>,
    ) {
        stats.total_entries += cached.entry_count;
        stats.total_tokens.input_tokens += cached.input_tokens;
        stats.total_tokens.output_tokens += cached.output_tokens;
        stats.total_tokens.cache_creation_tokens += cached.cache_creation_tokens;
        stats.total_tokens.cache_read_tokens += cached.cache_read_tokens;

        for (tool, count) in &cached.tool_usage {
            *stats.tool_usage.entry(tool.clone()).or_insert(0) += count;
        }

        for (model, count) in &cached.model_usage {
            *stats.model_usage.entry(model.clone()).or_insert(0) += count;
        }

        for (model, cached_ts) in &cached.model_tokens {
            let model_stats = stats.model_tokens.entry(model.clone()).or_default();
            model_stats.input_tokens += cached_ts.input_tokens;
            model_stats.output_tokens += cached_ts.output_tokens;
            model_stats.cache_creation_tokens += cached_ts.cache_creation_tokens;
            model_stats.cache_read_tokens += cached_ts.cache_read_tokens;
        }

        let session_tokens = cached.input_tokens
            + cached.output_tokens
            + cached.cache_creation_tokens
            + cached.cache_read_tokens;
        let session_work_tokens = cached.input_tokens + cached.output_tokens;

        if let Some(name) = project_name {
            let project = stats.project_stats.entry(name.clone()).or_default();
            project.sessions += 1;
            project.tokens += session_tokens;
            project.work_tokens += session_work_tokens;
        }

        if let Some(date) = cached.session_date {
            *stats.daily_activity.entry(date).or_insert(0) += session_tokens;
            *stats.daily_work_activity.entry(date).or_insert(0) += session_work_tokens;
        }

        for (hour, tokens) in &cached.hourly_activity {
            *stats.hourly_activity.entry(*hour).or_insert(0) += tokens;
        }

        for (hour, tokens) in &cached.hourly_work_activity {
            *stats.hourly_work_activity.entry(*hour).or_insert(0) += tokens;
        }

        for (weekday, tokens) in &cached.weekday_activity {
            *stats
                .weekday_activity
                .entry(Weekday::try_from(*weekday).unwrap_or(Weekday::Mon))
                .or_insert(0) += tokens;
        }

        for (weekday, tokens) in &cached.weekday_work_activity {
            *stats
                .weekday_work_activity
                .entry(Weekday::try_from(*weekday).unwrap_or(Weekday::Mon))
                .or_insert(0) += tokens;
        }

        stats.tool_error_count += cached.tool_error_count;
        stats.tool_success_count += cached.tool_success_count;

        for (lang, count) in &cached.language_usage {
            *stats.language_usage.entry(lang.clone()).or_insert(0) += count;
        }

        for (ext, count) in &cached.extension_usage {
            *stats.extension_usage.entry(ext.clone()).or_insert(0) += count;
        }

        Self::apply_productivity_stats(stats, cached);
    }

    fn apply_productivity_stats(stats: &mut Stats, cached: &CachedFileStats) {
        if !cached.is_subagent {
            stats.total_sessions_count += 1;
            if cached.summary.is_some() {
                stats.sessions_with_summary += 1;
            }

            if let Some(ref branch) = cached.git_branch {
                let branch_stats = stats.branch_stats.entry(branch.clone()).or_default();
                branch_stats.session_count += 1;
                if let Some(duration) = cached.session_duration_mins {
                    branch_stats.total_duration_mins += duration;
                }
                if let Some(ts) = cached.first_timestamp
                    && (branch_stats.first_seen.is_none() || Some(ts) < branch_stats.first_seen) {
                        branch_stats.first_seen = Some(ts);
                    }
                if let Some(ts) = cached.last_timestamp
                    && (branch_stats.last_seen.is_none() || Some(ts) > branch_stats.last_seen) {
                        branch_stats.last_seen = Some(ts);
                    }
            }
        }
    }

    fn aggregate_file_entries(
        stats: &mut Stats,
        entries: &[LogEntry],
        project_name: &Option<String>,
        file: &Path,
    ) -> FileStats {
        let mut file_stats = FileStats::default();

        stats.total_entries += entries.len();

        let entries_with_ts: Vec<_> = entries.iter().filter(|e| e.timestamp.is_some()).collect();
        if let Some(first) = entries_with_ts.first() {
            file_stats.first_timestamp = first.timestamp;
            file_stats.session_id = first.session_id.clone();
            file_stats.git_branch = first.git_branch.clone();
            let is_subagent_by_entry = first.is_sidechain;
            let is_subagent_by_filename = file
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-"));
            file_stats.is_subagent = is_subagent_by_entry || is_subagent_by_filename;
        }
        if let Some(last) = entries_with_ts.last() {
            file_stats.last_timestamp = last.timestamp;
        }

        file_stats.summary = entries
            .iter()
            .rfind(|e| e.entry_type == EntryType::Summary)
            .and_then(|e| e.summary.clone())
            .or_else(|| {
                entries
                    .iter()
                    .rev()
                    .filter(|e| e.entry_type == EntryType::User || e.entry_type == EntryType::System)
                    .find_map(|e| {
                        let text = e.message.as_ref()?.content.extract_text();
                        let text = text.trim();
                        if text.is_empty() || text.starts_with('<') {
                            None
                        } else {
                            let truncated: String = text.chars().take(80).collect();
                            Some(truncated)
                        }
                    })
            });

        file_stats.custom_title = entries
            .iter()
            .rfind(|e| e.entry_type == EntryType::CustomTitle)
            .and_then(|e| e.custom_title.clone());

        file_stats.model = entries
            .iter().find_map(|e| e.message.as_ref()?.model.as_ref())
            .cloned();

        for entry in entries {
            if let Some(ts) = entry.timestamp {
                let date = ts.with_timezone(&Local).date_naive();
                let daily = file_stats.daily_stats.entry(date).or_default();

                if daily.first_timestamp.is_none() || Some(ts) < daily.first_timestamp {
                    daily.first_timestamp = Some(ts);
                }
                if daily.last_timestamp.is_none() || Some(ts) > daily.last_timestamp {
                    daily.last_timestamp = Some(ts);
                }

                if entry.entry_type == EntryType::Assistant
                    && let Some(ref message) = entry.message
                        && let Some(ref usage) = message.usage {
                            let model_key = message
                                .model
                                .clone()
                                .unwrap_or_else(|| "unknown".to_string());
                            if model_key == "<synthetic>" {
                                continue;
                            }
                            daily.input_tokens += usage.input_tokens;
                            daily.output_tokens += usage.output_tokens;

                            let daily_model = daily.tokens_by_model.entry(model_key).or_default();
                            daily_model.input_tokens += usage.input_tokens;
                            daily_model.output_tokens += usage.output_tokens;
                            daily_model.cache_creation_tokens += usage.cache_creation_input_tokens;
                            daily_model.cache_read_tokens += usage.cache_read_input_tokens;

                            let local_ts = ts.with_timezone(&Local);
                            let hour = local_ts.hour() as u8;
                            let weekday = local_ts.weekday().num_days_from_monday() as u8;
                            let tokens = usage.input_tokens
                                + usage.output_tokens
                                + usage.cache_creation_input_tokens
                                + usage.cache_read_input_tokens;
                            let work_tokens = usage.input_tokens + usage.output_tokens;

                            *daily.hourly_activity.entry(hour).or_insert(0) += tokens;
                            *daily.hourly_work_activity.entry(hour).or_insert(0) += work_tokens;
                            *file_stats.hourly_activity.entry(hour).or_insert(0) += tokens;
                            *file_stats.hourly_work_activity.entry(hour).or_insert(0) +=
                                work_tokens;
                            *file_stats.weekday_activity.entry(weekday).or_insert(0) += tokens;
                            *file_stats.weekday_work_activity.entry(weekday).or_insert(0) +=
                                work_tokens;
                            *stats.hourly_activity.entry(hour).or_insert(0) += tokens;
                            *stats.hourly_work_activity.entry(hour).or_insert(0) += work_tokens;
                            *stats
                                .weekday_activity
                                .entry(local_ts.weekday())
                                .or_insert(0) += tokens;
                            *stats
                                .weekday_work_activity
                                .entry(local_ts.weekday())
                                .or_insert(0) += work_tokens;
                        }
            }

            if let Some(ref message) = entry.message {
                if entry.entry_type == EntryType::Assistant {
                    if let Some(ref usage) = message.usage {
                        stats.total_tokens.add(usage);
                        file_stats.input_tokens += usage.input_tokens;
                        file_stats.output_tokens += usage.output_tokens;
                        file_stats.cache_creation_tokens += usage.cache_creation_input_tokens;
                        file_stats.cache_read_tokens += usage.cache_read_input_tokens;

                        if let Some(ref model) = message.model {
                            let model_stats = stats.model_tokens.entry(model.clone()).or_default();
                            model_stats.add(usage);
                            let file_model_stats =
                                file_stats.model_tokens.entry(model.clone()).or_default();
                            file_model_stats.add(usage);
                        }
                    }

                    if let Some(ref model) = message.model {
                        *stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                        *file_stats.model_usage.entry(model.clone()).or_insert(0) += 1;
                    }
                }

                let daily_date = entry
                    .timestamp
                    .map(|ts| ts.with_timezone(&Local).date_naive());
                Self::count_tool_usage_with_file_stats(stats, &mut file_stats, daily_date, message);
            }
        }

        let session_tokens = file_stats.input_tokens
            + file_stats.output_tokens
            + file_stats.cache_creation_tokens
            + file_stats.cache_read_tokens;
        let session_work_tokens = file_stats.input_tokens + file_stats.output_tokens;

        if let Some(name) = project_name {
            let project = stats.project_stats.entry(name.clone()).or_default();
            project.sessions += 1;
            project.tokens += session_tokens;
            project.work_tokens += session_work_tokens;
        }

        file_stats.session_date = Self::extract_session_date(entries);
        if let Some(date) = file_stats.session_date {
            *stats.daily_activity.entry(date).or_insert(0) += session_tokens;
            *stats.daily_work_activity.entry(date).or_insert(0) += session_work_tokens;
        }

        if let (Some(first), Some(last)) = (file_stats.first_timestamp, file_stats.last_timestamp) {
            let duration = (last - first).num_minutes();
            file_stats.session_duration_mins = Some(duration.max(1));
        }

        file_stats
    }

    fn count_tool_usage_with_file_stats(
        stats: &mut Stats,
        file_stats: &mut FileStats,
        daily_date: Option<NaiveDate>,
        message: &crate::domain::Message,
    ) {
        use crate::domain::{ContentBlock, MessageContent};

        if let MessageContent::Blocks(ref blocks) = message.content {
            for block in blocks {
                match block {
                    ContentBlock::ToolUse { name, input, .. } => {
                        *stats.tool_usage.entry(name.clone()).or_insert(0) += 1;
                        *file_stats.tool_usage.entry(name.clone()).or_insert(0) += 1;

                        let lang = Self::extract_language_from_tool_input(name, input);
                        if let Some(lang) = lang {
                            *stats.language_usage.entry(lang.to_string()).or_insert(0) += 1;
                            *file_stats
                                .language_usage
                                .entry(lang.to_string())
                                .or_insert(0) += 1;
                        }

                        let exts = Self::extract_extensions_from_tool_input(name, input);
                        for ext in &exts {
                            *stats.extension_usage.entry(ext.clone()).or_insert(0) += 1;
                            *file_stats.extension_usage.entry(ext.clone()).or_insert(0) += 1;
                        }

                        if let Some(date) = daily_date
                            && let Some(d) = file_stats.daily_stats.get_mut(&date) {
                                *d.tool_usage.entry(name.clone()).or_insert(0) += 1;
                                if let Some(lang) = lang {
                                    *d.language_usage.entry(lang.to_string()).or_insert(0) += 1;
                                }
                                for ext in exts {
                                    *d.extension_usage.entry(ext).or_insert(0) += 1;
                                }
                            }
                    }
                    ContentBlock::ToolResult { is_error, .. } => {
                        if *is_error {
                            stats.tool_error_count += 1;
                            file_stats.tool_error_count += 1;
                        } else {
                            stats.tool_success_count += 1;
                            file_stats.tool_success_count += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    pub(crate) fn extract_language_from_tool_input(
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Option<&'static str> {
        match tool_name {
            "Read" | "Edit" | "Write" | "MultiEdit" => {
                let path = input
                    .get("file_path")
                    .or_else(|| input.get("path"))
                    .and_then(|v| v.as_str())?;
                Self::get_language_from_path(path)
            }
            "NotebookEdit" => {
                let path = input.get("notebook_path").and_then(|v| v.as_str())?;
                Self::get_language_from_path(path)
            }
            "Glob" => {
                let pattern = input.get("pattern").and_then(|v| v.as_str())?;
                Self::get_language_from_glob_pattern(pattern)
            }
            "Grep" => {
                let glob = input.get("glob").and_then(|v| v.as_str());
                if let Some(g) = glob {
                    return Self::get_language_from_glob_pattern(g);
                }
                let type_filter = input.get("type").and_then(|v| v.as_str())?;
                Self::get_language_from_type_filter(type_filter)
            }
            _ => None,
        }
    }

    fn get_language_from_type_filter(type_filter: &str) -> Option<&'static str> {
        match type_filter {
            "ada" => Some("Ada"),
            "astro" => Some("Astro"),
            "c" | "h" => Some("C"),
            "clojure" | "clj" => Some("Clojure"),
            "cpp" | "c++" => Some("C++"),
            "cs" | "csharp" => Some("C#"),
            "css" | "scss" | "sass" | "less" => Some("CSS"),
            "d" => Some("D"),
            "dart" => Some("Dart"),
            "docker" | "dockerfile" => Some("Docker"),
            "elixir" => Some("Elixir"),
            "elm" => Some("Elm"),
            "erlang" | "erl" => Some("Erlang"),
            "fortran" => Some("Fortran"),
            "fs" | "fsharp" => Some("F#"),
            "gdscript" | "gd" => Some("GDScript"),
            "glsl" => Some("GLSL"),
            "go" => Some("Go"),
            "graphql" | "gql" => Some("GraphQL"),
            "haskell" | "hs" => Some("Haskell"),
            "html" => Some("HTML"),
            "java" => Some("Java"),
            "js" | "jsx" | "javascript" => Some("JavaScript"),
            "json" => Some("JSON"),
            "julia" | "jl" => Some("Julia"),
            "kotlin" | "kt" => Some("Kotlin"),
            "latex" | "tex" => Some("LaTeX"),
            "lua" => Some("Lua"),
            "md" | "markdown" => Some("Markdown"),
            "nim" => Some("Nim"),
            "nix" => Some("Nix"),
            "ocaml" | "ml" => Some("OCaml"),
            "perl" | "pl" => Some("Perl"),
            "php" => Some("PHP"),
            "powershell" | "ps1" => Some("PowerShell"),
            "proto" | "protobuf" => Some("Protobuf"),
            "purescript" | "purs" => Some("PureScript"),
            "py" | "python" => Some("Python"),
            "r" => Some("R"),
            "ruby" | "rb" => Some("Ruby"),
            "rust" => Some("Rust"),
            "scala" => Some("Scala"),
            "sh" | "shell" | "bash" | "zsh" | "fish" => Some("Shell"),
            "solidity" | "sol" => Some("Solidity"),
            "sql" => Some("SQL"),
            "svelte" => Some("Svelte"),
            "swift" => Some("Swift"),
            "terraform" | "tf" | "hcl" => Some("Terraform"),
            "toml" => Some("TOML"),
            "ts" | "tsx" | "typescript" => Some("TypeScript"),
            "vue" => Some("Vue"),
            "xml" => Some("XML"),
            "yaml" => Some("YAML"),
            "zig" => Some("Zig"),
            _ => None,
        }
    }

    pub fn language_for_extension(ext: &str) -> &'static str {
        match ext {
            // A
            "abap" => "ABAP",
            "ada" | "adb" | "ads" => "Ada",
            "apex" => "Apex",
            "applescript" | "scpt" => "AppleScript",
            "asm" | "s" | "nasm" => "Assembly",
            "astro" => "Astro",
            "awk" => "AWK",

            // B
            "bat" | "cmd" => "Batch",

            // C
            "c" | "h" => "C",
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "ipp" | "inl" => "C++",
            "cs" => "C#",
            "cairo" => "Cairo",
            "clj" | "cljs" | "cljc" | "edn" => "Clojure",
            "cmake" => "CMake",
            "cob" | "cbl" | "cpy" => "COBOL",
            "coffee" | "litcoffee" => "CoffeeScript",
            "cr" => "Crystal",
            "css" | "scss" | "sass" | "less" | "styl" | "stylus" => "CSS",
            "csv" | "tsv" => "CSV",
            "cu" | "cuh" => "CUDA",

            // D
            "d" => "D",
            "dart" => "Dart",
            "dhall" => "Dhall",
            "dockerfile" => "Docker",

            // E
            "ejs" => "EJS",
            "elm" => "Elm",
            "ex" | "exs" | "heex" | "leex" => "Elixir",
            "erl" | "hrl" => "Erlang",

            // F
            "f" | "f90" | "f95" | "f03" | "f08" | "for" => "Fortran",
            "fs" | "fsi" | "fsx" => "F#",

            // G
            "gd" | "gdscript" => "GDScript",
            "gleam" => "Gleam",
            "glsl" | "vert" | "frag" | "geom" | "comp" => "GLSL",
            "go" => "Go",
            "gradle" => "Gradle",
            "graphql" | "gql" => "GraphQL",
            "groovy" | "gvy" => "Groovy",

            // H
            "haml" => "HAML",
            "hbs" | "handlebars" => "Handlebars",
            "hs" | "lhs" => "Haskell",
            "hcl" => "HCL",
            "hlsl" => "HLSL",
            "html" | "htm" | "xhtml" => "HTML",
            "http" | "rest" => "HTTP",

            // I-J
            "idris" | "idr" => "Idris",
            "ipynb" => "Jupyter",
            "java" => "Java",
            "jinja" | "jinja2" | "j2" => "Jinja",
            "jl" => "Julia",
            "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
            "json" | "jsonc" | "jsonl" | "json5" | "geojson" => "JSON",

            // K
            "kdl" => "KDL",
            "kt" | "kts" => "Kotlin",

            // L
            "latex" | "tex" | "ltx" | "sty" => "LaTeX",
            "liquid" => "Liquid",
            "lisp" | "cl" | "el" | "elisp" => "Lisp",
            "lock" => "Lock",
            "lua" => "Lua",

            // M
            "m" | "mm" => "Objective-C",
            "makefile" | "mk" => "Makefile",
            "mat" | "matlab" => "MATLAB",
            "md" | "mdx" | "rst" | "adoc" | "asciidoc" => "Markdown",
            "ml" | "mli" => "OCaml",
            "mojo" => "Mojo",
            "move" => "Move",
            "mustache" => "Mustache",

            // N
            "nim" => "Nim",
            "nix" => "Nix",
            "njk" | "nunjucks" => "Nunjucks",
            "nu" => "Nushell",

            // O
            "odin" => "Odin",

            // P
            "pas" | "pp" | "lpr" => "Pascal",
            "pdf" => "PDF",
            "perl" | "pl" | "pm" | "t" | "pod" => "Perl",
            "php" | "phtml" | "phps" => "PHP",
            "prisma" => "Prisma",
            "proto" => "Protobuf",
            "ps1" | "psm1" | "psd1" => "PowerShell",
            "pug" | "jade" => "Pug",
            "purs" => "PureScript",
            "py" | "pyi" | "pyw" | "pyx" => "Python",

            // R
            "r" => "R",
            "rkt" | "scrbl" => "Racket",
            "re" | "rei" => "ReScript",
            "rmd" => "R Markdown",
            "robot" => "Robot Framework",
            "rs" => "Rust",

            // S
            "scala" | "sc" => "Scala",
            "scm" | "ss" => "Scheme",
            "sh" | "bash" | "zsh" | "fish" | "ksh" | "csh" | "tcsh" => "Shell",
            "slim" => "Slim",
            "snap" => "Snapshot",
            "sol" => "Solidity",
            "sql" | "psql" | "mysql" | "pgsql" | "plsql" => "SQL",
            "sv" | "svh" | "verilog" => "Verilog",
            "svelte" => "Svelte",
            "swift" => "Swift",

            // T
            "tcl" | "tk" => "Tcl",
            "tf" | "tfvars" => "Terraform",
            "toml" => "TOML",
            "ts" | "tsx" | "mts" | "cts" => "TypeScript",
            "twig" => "Twig",
            "txt" | "text" | "log" => "Text",

            // V
            "v" => "V",
            "vala" | "vapi" => "Vala",
            "vb" => "Visual Basic",
            "vhd" | "vhdl" => "VHDL",
            "vue" => "Vue",

            // W
            "wasm" | "wat" => "WebAssembly",
            "wgsl" => "WGSL",

            // X-Y
            "xaml" => "XAML",
            "xml" | "xsl" | "xslt" | "xsd" | "plist" | "rss" | "atom" => "XML",
            "yaml" | "yml" => "YAML",

            // Z
            "zig" => "Zig",

            // Binary & media
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp" | "tiff" | "avif" | "svg" => "Image",
            "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" => "Audio",
            "mp4" | "webm" | "avi" | "mov" | "mkv" | "flv" => "Video",
            "ttf" | "otf" | "woff" | "woff2" | "eot" => "Font",
            "zip" | "gz" | "tar" | "bz2" | "xz" | "7z" | "rar" | "zst" => "Archive",

            _ => "Other",
        }
    }

    fn get_language_from_glob_pattern(pattern: &str) -> Option<&'static str> {
        let filename = pattern.rsplit('/').next().unwrap_or(pattern);
        if let Some(brace_start) = filename.find('{')
            && let Some(brace_end) = filename.find('}') {
                let inner = &filename[brace_start + 1..brace_end];
                for part in inner.split(',') {
                    let ext = part.trim().trim_start_matches('.');
                    let lang = Self::language_for_extension(&ext.to_lowercase());
                    if lang != "Other" {
                        return Some(lang);
                    }
                }
                return None;
            }
        Self::get_language_from_path(pattern)
    }

    fn get_language_from_path(path: &str) -> Option<&'static str> {
        let filename = path.rsplit('/').next().unwrap_or(path);
        let filename_lower = filename.to_lowercase();

        match filename_lower.as_str() {
            "makefile" | "gnumakefile" | "justfile" => return Some("Makefile"),
            "dockerfile" | "containerfile" => return Some("Docker"),
            "gemfile" | "rakefile" | "vagrantfile" => return Some("Ruby"),
            "cmakelists.txt" => return Some("CMake"),
            _ => {}
        }

        let ext = filename.rsplit('.').next()?.to_lowercase();
        if ext == filename_lower {
            return Some("Other");
        }

        Some(Self::language_for_extension(&ext))
    }

    fn get_extension_from_path(path: &str) -> Option<String> {
        let filename = path.rsplit('/').next().unwrap_or(path);
        let filename_lower = filename.to_lowercase();

        match filename_lower.as_str() {
            "makefile" | "gnumakefile" | "justfile" | "dockerfile" | "containerfile"
            | "gemfile" | "rakefile" | "vagrantfile" | "cmakelists.txt" => return None,
            _ => {}
        }

        let ext = filename.rsplit('.').next()?.to_lowercase();
        if ext == filename_lower {
            return None;
        }
        Some(ext)
    }

    fn parse_extensions_from_glob(pattern: &str) -> Vec<String> {
        let filename = pattern.rsplit('/').next().unwrap_or(pattern);
        let Some(dot_pos) = filename.rfind('.') else {
            return vec![];
        };
        let after_dot = &filename[dot_pos + 1..];
        if after_dot.is_empty() {
            return vec![];
        }

        if after_dot.starts_with('{') && after_dot.ends_with('}') {
            let inner = &after_dot[1..after_dot.len() - 1];
            return inner
                .split(',')
                .filter_map(|s| {
                    let s = s.trim().to_lowercase();
                    let s = s.replace('*', "");
                    if s.is_empty() || s.contains('{') || s.contains('}') {
                        None
                    } else {
                        Some(s)
                    }
                })
                .collect();
        }

        let ext = after_dot.to_lowercase().replace('*', "");
        if ext.is_empty() || ext == filename.to_lowercase() {
            return vec![];
        }
        vec![ext]
    }

    pub(crate) fn extract_extensions_from_tool_input(
        tool_name: &str,
        input: &serde_json::Value,
    ) -> Vec<String> {
        match tool_name {
            "Read" | "Edit" | "Write" | "MultiEdit" => {
                let path = input
                    .get("file_path")
                    .or_else(|| input.get("path"))
                    .and_then(|v| v.as_str());
                path.and_then(Self::get_extension_from_path)
                    .into_iter()
                    .collect()
            }
            "NotebookEdit" => {
                let path = input.get("notebook_path").and_then(|v| v.as_str());
                path.and_then(Self::get_extension_from_path)
                    .into_iter()
                    .collect()
            }
            "Glob" => {
                let pattern = input.get("pattern").and_then(|v| v.as_str());
                pattern
                    .map(Self::parse_extensions_from_glob)
                    .unwrap_or_default()
            }
            "Grep" => {
                let glob = input.get("glob").and_then(|v| v.as_str());
                glob.map(Self::parse_extensions_from_glob)
                    .unwrap_or_default()
            }
            _ => vec![],
        }
    }

    fn extract_project_name_from_entries(entries: &[LogEntry]) -> Option<String> {
        super::extract_project_name(entries)
    }

    fn extract_session_date(entries: &[LogEntry]) -> Option<NaiveDate> {
        entries
            .iter().find_map(|e| e.timestamp)
            .map(|ts| ts.with_timezone(&Local).date_naive())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_stats_work_tokens_excludes_cache() {
        let stats = TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
        };
        assert_eq!(stats.work_tokens(), 1500);
    }

    #[test]
    fn test_token_stats_all_tokens_includes_cache() {
        let stats = TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
        };
        assert_eq!(stats.all_tokens(), 2000);
    }

    #[test]
    fn test_token_stats_all_tokens_vs_work_tokens_difference() {
        let stats = TokenStats {
            input_tokens: 1000,
            output_tokens: 500,
            cache_creation_tokens: 200,
            cache_read_tokens: 300,
        };
        let cache_tokens = stats.cache_creation_tokens + stats.cache_read_tokens;
        assert_eq!(stats.all_tokens(), stats.work_tokens() + cache_tokens);
    }

    #[test]
    fn test_token_stats_add_from_usage() {
        let mut stats = TokenStats::default();
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 20,
            cache_read_input_tokens: 30,
            service_tier: None,
        };
        stats.add(&usage);

        assert_eq!(stats.input_tokens, 100);
        assert_eq!(stats.output_tokens, 50);
        assert_eq!(stats.cache_creation_tokens, 20);
        assert_eq!(stats.cache_read_tokens, 30);
        assert_eq!(stats.all_tokens(), 200);
    }

    #[test]
    fn test_token_stats_add_accumulates() {
        let mut stats = TokenStats::default();
        let usage1 = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 20,
            cache_read_input_tokens: 30,
            service_tier: None,
        };
        let usage2 = Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_creation_input_tokens: 40,
            cache_read_input_tokens: 60,
            service_tier: None,
        };
        stats.add(&usage1);
        stats.add(&usage2);

        assert_eq!(stats.all_tokens(), 600);
    }

    #[test]
    fn test_get_language_from_path_extensions() {
        assert_eq!(
            StatsAggregator::get_language_from_path("/src/main.rs"),
            Some("Rust")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/app.tsx"),
            Some("TypeScript")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/script.py"),
            Some("Python")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/data.json"),
            Some("JSON")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/data.jsonl"),
            Some("JSON")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/style.scss"),
            Some("CSS")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/page.vue"),
            Some("Vue")
        );
    }

    #[test]
    fn test_get_language_from_path_new_extensions() {
        assert_eq!(
            StatsAggregator::get_language_from_path("/readme.txt"),
            Some("Text")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/data.csv"),
            Some("CSV")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/icon.png"),
            Some("Image")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/logo.svg"),
            Some("Image")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/notebook.ipynb"),
            Some("Jupyter")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/Cargo.lock"),
            Some("Lock")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/app.conf"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/test.snap"),
            Some("Snapshot")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/template.hbs"),
            Some("Handlebars")
        );
    }

    #[test]
    fn test_get_language_from_path_dotfiles() {
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/.gitignore"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/.editorconfig"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/.env"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/.prettierrc"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/.node-version"),
            Some("Other")
        );
    }

    #[test]
    fn test_get_language_from_path_extensionless_files() {
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/Makefile"),
            Some("Makefile")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/Dockerfile"),
            Some("Docker")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/Gemfile"),
            Some("Ruby")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/Justfile"),
            Some("Makefile")
        );
    }

    #[test]
    fn test_get_language_from_path_unknown_extensionless() {
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/LICENSE"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/CHANGELOG"),
            Some("Other")
        );
    }

    #[test]
    fn test_get_language_from_path_real_world_other() {
        assert_eq!(
            StatsAggregator::get_language_from_path("/config.kdl"),
            Some("KDL")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/.envrc"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/.env.example"),
            Some("Other")
        );
        assert_eq!(
            StatsAggregator::get_language_from_path("/project/.claude"),
            Some("Other")
        );
    }

    #[test]
    fn test_extract_language_glob_uses_pattern() {
        let input = serde_json::json!({"pattern": "**/*.rs", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Glob", &input),
            Some("Rust")
        );
    }

    #[test]
    fn test_extract_language_glob_brace_pattern() {
        let input = serde_json::json!({"pattern": "**/*.{ts,js}", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Glob", &input),
            Some("TypeScript")
        );
        let input2 = serde_json::json!({"pattern": "**/*.{json,yaml}", "path": "/"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Glob", &input2),
            Some("JSON")
        );
    }

    #[test]
    fn test_extract_language_grep_brace_glob() {
        let input = serde_json::json!({"pattern": "TODO", "glob": "*.{js,json}"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            Some("JavaScript")
        );
    }

    #[test]
    fn test_extract_language_grep_uses_glob_field() {
        let input = serde_json::json!({"pattern": "fn main", "glob": "*.py", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            Some("Python")
        );
    }

    #[test]
    fn test_extract_language_grep_uses_type_filter() {
        let input = serde_json::json!({"pattern": "fn main", "type": "rust", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            Some("Rust")
        );
    }

    #[test]
    fn test_extract_language_grep_dir_only_returns_none() {
        let input = serde_json::json!({"pattern": "fn main", "path": "/src"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Grep", &input),
            None
        );
    }

    #[test]
    fn test_extract_language_read_uses_file_path() {
        let input = serde_json::json!({"file_path": "/src/main.rs"});
        assert_eq!(
            StatsAggregator::extract_language_from_tool_input("Read", &input),
            Some("Rust")
        );
    }
}
