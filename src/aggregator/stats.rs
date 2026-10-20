use std::collections::HashMap;

use crate::domain::Usage;

/// Slash-command name from a "command invocation" user message. Accepts
/// only messages with exactly one `<command-name>` tag AND body < 500
/// chars — real invocations are tiny XML blocks; summary regen prompts
/// can list dozens of historical commands and would otherwise count.
pub(crate) fn extract_command_names(text: &str) -> Vec<String> {
    if text.len() >= 500 {
        return Vec::new();
    }
    if text.matches("<command-name>").count() != 1 {
        return Vec::new();
    }
    let Some(open) = text.find("<command-name>") else {
        return Vec::new();
    };
    let after_open = &text[open + "<command-name>".len()..];
    let Some(close) = after_open.find("</command-name>") else {
        return Vec::new();
    };
    let raw = after_open[..close].trim();
    let name = raw.strip_prefix('/').unwrap_or(raw);
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        return Vec::new();
    }
    vec![name.to_string()]
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    /// Cumulative lines added / removed across every edit in scope. Churn, so
    /// re-editing a line counts again; not a net diff and not scoped to one
    /// context window the way `/usage` is.
    pub line_churn: crate::domain::LineChurn,
    pub total_tokens: TokenStats,
    pub tool_usage: HashMap<String, usize>,
    pub model_tokens: HashMap<String, TokenStats>,
    pub project_stats: HashMap<String, ProjectStats>,
    pub hourly_work_activity: HashMap<u8, u64>,
    pub tool_error_count: usize,
    pub tool_success_count: usize,
    pub total_sessions_count: usize,
    pub language_usage: HashMap<String, usize>,
    pub extension_usage: HashMap<String, usize>,
    /// Per tool key: distinct user sessions that used it, a subagent's use
    /// credited to its parent.
    pub tool_sessions: HashMap<String, usize>,
    /// Per-MCP-server: distinct user sessions that used one of its tools, a
    /// subagent's use credited to its parent. Keyed by the normalized server
    /// name (`serverA`, or `orgA/serverB` for plugin-form servers).
    pub mcp_server_sessions: HashMap<String, usize>,
    /// Distinct user sessions that used at least one Skills entry (key starts with `skill:`).
    pub sessions_using_skills: usize,
    /// Distinct user sessions that used at least one Subagents entry (key starts with `agent:`).
    pub sessions_using_subagents: usize,
    /// Distinct user sessions that used at least one MCP tool (key starts with `mcp__`).
    pub sessions_using_mcp: usize,
    /// Distinct user sessions that invoked at least one slash command (key starts with `command:`).
    pub sessions_using_commands: usize,
}

impl Stats {
    /// The whole-view figures folded from per-day session slices — what a
    /// filtered view shows, and the one definition every view can share.
    pub(crate) fn from_groups(groups: &[crate::aggregator::DailyGroup]) -> Stats {
        let mut stats = Stats::default();
        // `groups` repeats a session once per active day; every session count
        // below dedups by path, so a multi-day session counts once.
        let mut seen_paths: std::collections::HashSet<&std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut tool_owners: HashMap<&String, std::collections::HashSet<&std::path::PathBuf>> =
            HashMap::new();
        let mut server_owners: HashMap<String, std::collections::HashSet<&std::path::PathBuf>> =
            HashMap::new();
        let mut adoption_owners: [std::collections::HashSet<&std::path::PathBuf>; 4] =
            Default::default();

        for group in groups {
            for session in &group.sessions {
                // The subagent rule (CLAUDE.md, Key Patterns): counts leave
                // subagents out, everything consumed or done takes them in.
                let first_sighting = !session.is_subagent && seen_paths.insert(&session.file_path);
                if first_sighting {
                    stats.total_sessions_count += 1;
                }
                // A subagent's tools count for the session that launched it,
                // so a tool's session count stays a count of user sessions.
                let owner = if session.is_subagent {
                    session.parent_path.as_ref()
                } else {
                    Some(&session.file_path)
                };
                if let Some(owner) = owner {
                    for tool in session.day_tool_usage.keys() {
                        if let Some(slot) = adoption_slot(tool) {
                            adoption_owners[slot].insert(owner);
                        }
                        tool_owners.entry(tool).or_default().insert(owner);
                        if let Some(server) = crate::aggregator::mcp_server_of(tool) {
                            server_owners.entry(server).or_default().insert(owner);
                        }
                    }
                }

                let work_tokens = session.work_tokens();
                // Cache reads and writes included — the measure the unfiltered
                // aggregation credits to `tokens` / `*_activity`.
                let all_tokens = work_tokens
                    + session
                        .day_tokens_by_model
                        .values()
                        .map(|t| t.cache_creation_tokens + t.cache_read_tokens)
                        .sum::<u64>();
                stats.line_churn.add(session.line_churn);
                stats.tool_error_count += session.day_tool_errors;
                stats.tool_success_count += session.day_tool_successes;
                stats.total_tokens.input_tokens += session.day_input_tokens;
                stats.total_tokens.output_tokens += session.day_output_tokens;

                for (model, tokens) in &session.day_tokens_by_model {
                    stats
                        .model_tokens
                        .entry(model.clone())
                        .or_default()
                        .merge(tokens);

                    stats.total_tokens.cache_creation_tokens += tokens.cache_creation_tokens;
                    stats.total_tokens.cache_creation_5m_tokens += tokens.cache_creation_5m_tokens;
                    stats.total_tokens.cache_creation_1h_tokens += tokens.cache_creation_1h_tokens;
                    stats.total_tokens.cache_read_tokens += tokens.cache_read_tokens;
                }

                for (hour, tokens) in &session.day_hourly_work_tokens {
                    *stats.hourly_work_activity.entry(*hour).or_insert(0) += tokens;
                }

                let project_stats = stats
                    .project_stats
                    .entry(session.project_name.clone())
                    .or_default();
                // Distinct user sessions, the count the project popup shows.
                project_stats.sessions += usize::from(first_sighting);
                project_stats.work_tokens += work_tokens;
                project_stats.tokens += all_tokens;

                for (tool, count) in &session.day_tool_usage {
                    *stats.tool_usage.entry(tool.clone()).or_insert(0) += count;
                }
                for (lang, count) in &session.day_language_usage {
                    *stats.language_usage.entry(lang.clone()).or_insert(0) += count;
                }
                for (ext, count) in &session.day_extension_usage {
                    *stats.extension_usage.entry(ext.clone()).or_insert(0) += count;
                }
            }
        }
        // Only owners that are user sessions of this view count: a parent the
        // view does not hold (or a transcript naming none) is no session here.
        let sessions_among = |owners: std::collections::HashSet<&std::path::PathBuf>| {
            owners
                .into_iter()
                .filter(|p| seen_paths.contains(p))
                .count()
        };
        stats.tool_sessions = tool_owners
            .into_iter()
            .map(|(tool, owners)| (tool.clone(), sessions_among(owners)))
            .filter(|(_, n)| *n > 0)
            .collect();
        let [skills, subagents, mcp, commands] = adoption_owners.map(sessions_among);
        stats.sessions_using_skills = skills;
        stats.sessions_using_subagents = subagents;
        stats.sessions_using_mcp = mcp;
        stats.sessions_using_commands = commands;
        stats.mcp_server_sessions = server_owners
            .into_iter()
            .map(|(server, owners)| (server, sessions_among(owners)))
            .filter(|(_, n)| *n > 0)
            .collect();

        stats
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProjectStats {
    pub sessions: usize,
    pub tokens: u64,
    pub work_tokens: u64,
}

/// Speeds with a rate; any other marks its aggregate's cost unknown.
const RATED_SPEEDS: &[&str] = &["standard", "fast"];

pub(crate) fn is_unrated_speed(speed: Option<&str>) -> bool {
    speed.is_some_and(|s| !RATED_SPEEDS.contains(&s))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Sum of 5m + 1h cache writes. Kept as a single field so existing UI /
    /// MCP / aggregation code that reads "cache creation tokens" stays
    /// correct. Cost math uses the per-TTL split below.
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    /// 5-minute TTL portion of `cache_creation_tokens`. Billed at 1.25x
    /// base input. Default 0 lets old cache files deserialize cleanly.
    #[serde(default)]
    pub cache_creation_5m_tokens: u64,
    /// 1-hour TTL portion of `cache_creation_tokens`. Billed at 2x base
    /// input. Claude subscription users (Pro/Max/Team) get this TTL by
    /// default, so on real data this is typically the larger half.
    #[serde(default)]
    pub cache_creation_1h_tokens: u64,

    /// Sticky across the fold: one flagged message makes the whole
    /// aggregate's cost unknown.
    #[serde(default)]
    pub non_standard_speed: bool,
}

impl TokenStats {
    pub fn work_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn all_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    pub fn add(&mut self, usage: &Usage) {
        self.non_standard_speed |= is_unrated_speed(usage.speed.as_deref());
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_tokens += usage.cache_creation_input_tokens;
        self.cache_read_tokens += usage.cache_read_input_tokens;
        if let Some(ref cc) = usage.cache_creation {
            self.cache_creation_5m_tokens += cc.ephemeral_5m_input_tokens;
            self.cache_creation_1h_tokens += cc.ephemeral_1h_input_tokens;
        } else {
            // JSONL without the structured `cache_creation` breakdown:
            // treat the flat aggregate as 5m TTL (the Anthropic API
            // default). Build flag, not a time question.
            self.cache_creation_5m_tokens += usage.cache_creation_input_tokens;
        }
    }

    /// Fold another already-aggregated `TokenStats` into this one. The single
    /// home for combining two token counts — a new token field is added here
    /// only, instead of in every per-field fold site.
    pub fn merge(&mut self, other: &TokenStats) {
        self.non_standard_speed |= other.non_standard_speed;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_creation_5m_tokens += other.cache_creation_5m_tokens;
        self.cache_creation_1h_tokens += other.cache_creation_1h_tokens;
    }
}

/// The transcript's `sessionId`, off the first entry that carries one — a
/// subagent's parent link is derived from it.
pub(crate) fn session_id_of(entries: &[crate::domain::LogEntry]) -> Option<String> {
    entries.iter().find_map(|e| e.session_id.clone())
}

/// A user entry someone typed. `role: user` also carries every tool result,
/// the wrappers Claude Code injects for slash commands, hooks and `!` output,
/// and compaction recaps — counting those reports a turn per tool call. The
/// filter is the one the conversation preview applies, so the two agree.
pub(crate) fn is_human_prompt(entry: &crate::domain::LogEntry) -> bool {
    human_prompt_text(entry).is_some()
}

/// The typed text of a human prompt, cleaned for display; `None` for every
/// entry [`is_human_prompt`] rejects.
pub(crate) fn human_prompt_text(entry: &crate::domain::LogEntry) -> Option<String> {
    if entry.is_compact_summary || entry.is_meta {
        return None;
    }
    let message = entry.message.as_ref()?;
    if !matches!(message.role, crate::domain::Role::User) {
        return None;
    }
    let cleaned = crate::text::clean_user_message_preview(&message.content.extract_text());
    (!cleaned.is_empty() && !crate::text::looks_like_system_injection(&cleaned)).then_some(cleaned)
}

pub struct CacheStats {
    pub cached_files: usize,
    pub parsed_files: usize,
}

/// Which `sessions_using_*` counter a tool key moves: skills, subagents,
/// MCP, commands. `None` for a key no adoption counter tracks.
fn adoption_slot(key: &str) -> Option<usize> {
    if key.starts_with("skill:") {
        Some(0)
    } else if key.starts_with("agent:") {
        Some(1)
    } else if key.starts_with("mcp__") {
        Some(2)
    } else if key.starts_with("command:") {
        Some(3)
    } else {
        None
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
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
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
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
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
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
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
            cache_creation: None,
            speed: None,
            iterations: Vec::new(),
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
            cache_creation: None,
            speed: None,
            iterations: Vec::new(),
        };
        let usage2 = Usage {
            input_tokens: 200,
            output_tokens: 100,
            cache_creation_input_tokens: 40,
            cache_read_input_tokens: 60,
            service_tier: None,
            cache_creation: None,
            speed: None,
            iterations: Vec::new(),
        };
        stats.add(&usage1);
        stats.add(&usage2);

        assert_eq!(stats.all_tokens(), 600);
    }

    // ─── Adoption / tool_sessions counters ───────────────────────────────────

    #[test]
    fn only_typed_user_entries_count_as_prompts() {
        let entry = |json: &str| serde_json::from_str::<crate::domain::LogEntry>(json).unwrap();
        assert!(is_human_prompt(&entry(
            r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#
        )));
        for (json, why) in [
            (
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#,
                "a tool result",
            ),
            (
                r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"This session is being continued"}}"#,
                "a compaction recap",
            ),
            (
                r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>Caveat</local-command-caveat>"}}"#,
                "an injected wrapper",
            ),
            (
                r#"{"type":"assistant","message":{"role":"assistant","content":"hi"}}"#,
                "an assistant reply",
            ),
        ] {
            assert!(!is_human_prompt(&entry(json)), "{why} counted as a prompt");
        }
    }

    // A tool's session count is a count of user sessions: a subagent's use
    // credits the session that launched it, once however many days or
    // transcripts it spans. Category adoption follows the same rule.
    #[test]
    fn a_subagents_tools_count_for_the_session_that_launched_it() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let next = day.succ_opt().unwrap();
        let parent = make_session("p", None, None);
        let parent_path = parent.file_path.clone();
        let sub = |name: &str| {
            let mut s = make_session("p", None, Some(name));
            s.is_subagent = true;
            s.parent_path = Some(parent_path.clone());
            s.day_tool_usage.insert("skill:s1".to_string(), 2);
            s
        };
        let stats = Stats::from_groups(&[
            make_daily_group(day, vec![parent.clone(), sub("a"), sub("b")]),
            make_daily_group(next, vec![parent, sub("a")]),
        ]);
        assert_eq!(
            stats.tool_sessions.get("skill:s1"),
            Some(&1),
            "one user session"
        );
        assert_eq!(
            stats.tool_usage.get("skill:s1"),
            Some(&6),
            "every call counts"
        );
        assert_eq!(
            stats.sessions_using_skills, 1,
            "credited to the parent once"
        );
    }

    #[test]
    fn a_project_counts_its_subagents_tokens_but_only_the_users_sessions() {
        use crate::test_helpers::helpers::{make_daily_group, make_session_with_tokens};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let user = make_session_with_tokens("p", 100, 0, "claude-opus-4-6");
        let mut sub = make_session_with_tokens("p", 100, 0, "claude-opus-4-6");
        sub.is_subagent = true;
        sub.file_path = std::path::PathBuf::from("/tmp/test/sub.jsonl");
        let stats = Stats::from_groups(&[make_daily_group(day, vec![user, sub])]);
        let p = &stats.project_stats["p"];
        assert_eq!(
            p.work_tokens, 200,
            "subagent tokens count toward the project"
        );
        assert_eq!(p.sessions, 1, "the popup's session count, not transcripts");
    }

    #[test]
    fn adoption_slots_cover_every_category_prefix() {
        assert_eq!(adoption_slot("skill:s1"), Some(0));
        assert_eq!(adoption_slot("agent:type-a"), Some(1));
        assert_eq!(adoption_slot("mcp__server1__action"), Some(2));
        assert_eq!(adoption_slot("mcp__plugin_orgA_serverB__action"), Some(2));
        assert_eq!(adoption_slot("command:my-cmd"), Some(3));
        assert_eq!(adoption_slot("Bash"), None);
    }

    // A session that touches a category on several days, or with several of
    // its keys, counts once; a session using none counts nowhere.
    #[test]
    fn adoption_counts_each_user_session_once() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let with = |keys: &[&str]| {
            let mut s = make_session("p", None, None);
            for k in keys {
                s.day_tool_usage.insert((*k).to_string(), 1);
            }
            s
        };
        let a = with(&["skill:s1", "skill:s2", "mcp__server1__action"]);
        let stats = Stats::from_groups(&[
            make_daily_group(day, vec![a.clone()]),
            make_daily_group(day.succ_opt().unwrap(), vec![a]),
        ]);
        assert_eq!(stats.sessions_using_skills, 1);
        assert_eq!(stats.sessions_using_mcp, 1);
        assert_eq!(stats.sessions_using_commands, 0);
        let plain = Stats::from_groups(&[make_daily_group(day, vec![with(&["Bash"])])]);
        assert_eq!(plain.sessions_using_skills + plain.sessions_using_mcp, 0);
    }

    // A subagent whose parent is unknown, or outside the groups folded, adds
    // calls but no session: the count never exceeds the user sessions held.
    #[test]
    fn a_tools_session_count_never_exceeds_the_user_sessions_held() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let mut user = make_session("p", None, None);
        user.day_tool_usage.insert("Read".to_string(), 1);
        let sub = |name: &str, parent: Option<&str>| {
            let mut s = make_session("p", None, Some(name));
            s.is_subagent = true;
            s.parent_path = parent.map(std::path::PathBuf::from);
            s.day_tool_usage.insert("Read".to_string(), 3);
            s
        };
        let stats = Stats::from_groups(&[make_daily_group(
            day,
            vec![
                user,
                sub("orphan", None),
                sub("away", Some("/elsewhere/x.jsonl")),
            ],
        )]);
        assert_eq!(stats.tool_sessions.get("Read"), Some(&1));
        assert_eq!(stats.tool_usage.get("Read"), Some(&7));
    }

    // A server's session count is distinct user sessions: two of its tools in
    // one session count once, and a subagent's call credits its parent.
    #[test]
    fn a_servers_sessions_are_distinct_user_sessions() {
        use crate::test_helpers::helpers::{make_daily_group, make_session};
        let day = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(); // lint-ok: date-literal
        let mut a = make_session("p", None, Some("a"));
        a.day_tool_usage
            .insert("mcp__server1__action1".to_string(), 1);
        a.day_tool_usage
            .insert("mcp__server1__action2".to_string(), 1);
        let mut b = make_session("p", None, Some("b"));
        b.day_tool_usage
            .insert("mcp__server2__other".to_string(), 1);
        let mut sub = make_session("p", None, Some("sub"));
        sub.is_subagent = true;
        sub.parent_path = Some(b.file_path.clone());
        sub.day_tool_usage
            .insert("mcp__server1__action1".to_string(), 4);
        let stats = Stats::from_groups(&[make_daily_group(day, vec![a, b, sub])]);
        assert_eq!(stats.mcp_server_sessions.get("server1"), Some(&2));
        assert_eq!(stats.mcp_server_sessions.get("server2"), Some(&1));
    }
}
