use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lines added / removed by one tool result. Churn: re-editing the same line
/// counts again, and nothing is ever subtracted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineChurn {
    pub added: u32,
    pub removed: u32,
}

impl LineChurn {
    pub fn add(&mut self, other: Self) {
        self.added = self.added.saturating_add(other.added);
        self.removed = self.removed.saturating_add(other.removed);
    }
}

/// Extracts `LineChurn` from a `toolUseResult` payload. Separate from
/// `LineChurn` so that type stays a plain pair of integers whose
/// `Deserialize` is the inverse of its `Serialize` — the cache stores it
/// directly, and reading a stored value back cannot silently yield zero.
/// Read through `LogEntry::line_churn`, never constructed by callers.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ToolResultChurn(pub LineChurn);

impl<'de> Deserialize<'de> for ToolResultChurn {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};

        // Every value below is read through one of these, which accept ANY
        // JSON type and read it to its end. `next_value::<T>()` on a mismatched
        // array or object stops mid-value, so the entry's next key fails to
        // parse and the whole entry — tokens included — is dropped.

        /// Scalars carry no churn; each visit returns the type's default.
        macro_rules! skip_scalars {
            ($v:ty) => {
                fn visit_bool<E>(self, _: bool) -> Result<$v, E> {
                    Ok(<$v>::default())
                }
                fn visit_i64<E>(self, _: i64) -> Result<$v, E> {
                    Ok(<$v>::default())
                }
                fn visit_u64<E>(self, _: u64) -> Result<$v, E> {
                    Ok(<$v>::default())
                }
                fn visit_f64<E>(self, _: f64) -> Result<$v, E> {
                    Ok(<$v>::default())
                }
                fn visit_unit<E>(self) -> Result<$v, E> {
                    Ok(<$v>::default())
                }
                fn visit_none<E>(self) -> Result<$v, E> {
                    Ok(<$v>::default())
                }
            };
        }

        /// A string's value when it is one; `None` for anything else.
        #[derive(Default)]
        struct Text(Option<String>);
        impl<'de> Deserialize<'de> for Text {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = Text;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("any value")
                    }
                    fn visit_str<E>(self, v: &str) -> Result<Text, E> {
                        Ok(Text(Some(v.to_string())))
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Text, A::Error> {
                        while s.next_element::<IgnoredAny>()?.is_some() {}
                        Ok(Text(None))
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Text, A::Error> {
                        while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                        Ok(Text(None))
                    }
                    skip_scalars!(Text);
                }
                d.deserialize_any(V)
            }
        }

        /// One `lines` element's sign: a non-string element counts nothing
        /// and leaves its well-formed neighbours counted.
        #[derive(Default)]
        struct Line(LineChurn);
        impl<'de> Deserialize<'de> for Line {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let mut churn = LineChurn::default();
                match Text::deserialize(d)?
                    .0
                    .as_deref()
                    .and_then(|l| l.bytes().next())
                {
                    Some(b'+') => churn.added = 1,
                    Some(b'-') => churn.removed = 1,
                    _ => {}
                }
                Ok(Line(churn))
            }
        }

        /// Sum over a sequence of `T`; anything but a sequence sums to zero.
        #[derive(Default)]
        struct Sum<T>(LineChurn, std::marker::PhantomData<T>);
        trait Churn {
            fn churn(&self) -> LineChurn;
        }
        impl Churn for Line {
            fn churn(&self) -> LineChurn {
                self.0
            }
        }
        impl Churn for Hunk {
            fn churn(&self) -> LineChurn {
                self.0
            }
        }
        impl<'de, T: Deserialize<'de> + Churn + Default> Deserialize<'de> for Sum<T> {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V<T>(std::marker::PhantomData<T>);
                impl<'de, T: Deserialize<'de> + Churn + Default> Visitor<'de> for V<T> {
                    type Value = Sum<T>;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("any value")
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Sum<T>, A::Error> {
                        let mut total = LineChurn::default();
                        while let Some(item) = s.next_element::<T>()? {
                            total.add(item.churn());
                        }
                        Ok(Sum(total, std::marker::PhantomData))
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Sum<T>, A::Error> {
                        while m.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                        Ok(Sum::default())
                    }
                    fn visit_str<E>(self, _: &str) -> Result<Sum<T>, E> {
                        Ok(Sum::default())
                    }
                    skip_scalars!(Sum<T>);
                }
                d.deserialize_any(V(std::marker::PhantomData))
            }
        }

        /// One `structuredPatch` hunk: the churn of its `lines`.
        #[derive(Default)]
        struct Hunk(LineChurn);
        impl<'de> Deserialize<'de> for Hunk {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = Hunk;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("a diff hunk")
                    }
                    fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Hunk, A::Error> {
                        let mut churn = LineChurn::default();
                        while let Some(key) = m.next_key::<String>()? {
                            if key == "lines" {
                                churn.add(m.next_value::<Sum<Line>>()?.0);
                            } else {
                                m.next_value::<IgnoredAny>()?;
                            }
                        }
                        Ok(Hunk(churn))
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Hunk, A::Error> {
                        while s.next_element::<IgnoredAny>()?.is_some() {}
                        Ok(Hunk::default())
                    }
                    fn visit_str<E>(self, _: &str) -> Result<Hunk, E> {
                        Ok(Hunk::default())
                    }
                    skip_scalars!(Hunk);
                }
                d.deserialize_any(V)
            }
        }

        /// Hand-written rather than `#[serde(untagged)]`: untagged buffers the
        /// whole value first, which both re-enters serde_json's recursion
        /// ceiling (dropping the entry) and copies payloads that carry entire
        /// file bodies. `IgnoredAny` skips them in place instead.
        struct ChurnVisitor;

        impl<'de> Visitor<'de> for ChurnVisitor {
            type Value = ToolResultChurn;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a tool result")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
                let mut patch = LineChurn::default();
                let mut content: Option<String> = None;
                let mut is_create = false;
                while let Some(key) = m.next_key::<String>()? {
                    match key.as_str() {
                        "structuredPatch" => patch.add(m.next_value::<Sum<Hunk>>()?.0),
                        // Only a Write's result holds a new file's body here;
                        // other tools (Grep's content mode among them) put
                        // their output under the same key.
                        "content" => content = m.next_value::<Text>()?.0,
                        "type" => {
                            is_create = m.next_value::<Text>()?.0.as_deref() == Some("create");
                        }
                        _ => {
                            m.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                // A Write leaves the patch empty, so counting only the patch
                // reports 0 lines for a file that was just created.
                if is_create
                    && patch == LineChurn::default()
                    && let Some(text) = content.filter(|t| !t.is_empty())
                {
                    patch.added = u32::try_from(text.trim_end_matches('\n').split('\n').count())
                        .unwrap_or(u32::MAX);
                }
                Ok(ToolResultChurn(patch))
            }
            // A string (Bash stdout), an array of content blocks, a scalar or
            // null carries no churn; skipping in place costs no allocation.
            fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Self::Value, A::Error> {
                while s.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ToolResultChurn::default())
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ToolResultChurn::default())
            }
            skip_scalars!(ToolResultChurn);
        }
        d.deserialize_any(ChurnVisitor)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogEntry {
    pub uuid: Option<String>,

    #[serde(rename = "parentUuid")]
    pub parent_uuid: Option<String>,

    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,

    pub timestamp: Option<DateTime<Utc>>,

    #[serde(rename = "type")]
    pub entry_type: EntryType,

    #[serde(default)]
    pub message: Option<Message>,

    #[serde(default)]
    pub summary: Option<String>,

    #[serde(rename = "customTitle", default)]
    pub custom_title: Option<String>,

    #[serde(rename = "aiTitle", default)]
    pub ai_title: Option<String>,

    #[serde(default)]
    pub cwd: Option<String>,

    #[serde(rename = "gitBranch")]
    pub git_branch: Option<String>,

    #[serde(default)]
    pub version: Option<String>,

    #[serde(rename = "isSidechain", default)]
    pub is_sidechain: bool,

    /// Claude Code writes its context-compaction recap as a `user` message
    /// carrying this flag. It condenses everything before it, so it is the
    /// strongest single signal for what a long session has been about.
    #[serde(rename = "isCompactSummary", default)]
    pub is_compact_summary: bool,

    #[serde(rename = "userType")]
    pub user_type: Option<UserType>,

    #[serde(rename = "requestId")]
    pub request_id: Option<String>,

    /// `system` entries carry a level (suggestion / info / warning / error);
    /// `level == "error"` is the only reliable "true error" signal (api_error
    /// etc.), unlike the noisy per-tool `is_error`.
    #[serde(default)]
    pub level: Option<String>,

    /// Churn from this entry's tool result, folded during deserialization so
    /// the payload — which carries whole file bodies — is never retained.
    #[serde(default, rename = "toolUseResult", skip_serializing)]
    #[doc(hidden)]
    pub tool_result_churn: ToolResultChurn,
}

impl LogEntry {
    pub fn line_churn(&self) -> LineChurn {
        self.tool_result_churn.0
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EntryType {
    User,
    Assistant,
    Summary,
    CustomTitle,
    AiTitle,
    System,
    FileHistorySnapshot,
    QueueOperation,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserType {
    External,
    Internal,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: Role,

    #[serde(default)]
    pub content: MessageContent,

    #[serde(default)]
    pub usage: Option<Usage>,

    #[serde(default)]
    pub model: Option<String>,

    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    #[default]
    Assistant,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum MessageContent {
    #[default]
    Empty,
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    pub fn extract_text(&self) -> String {
        match self {
            MessageContent::Empty => String::new(),
            MessageContent::Text(s) => s.clone(),
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    pub fn extract_tool_calls(&self) -> Vec<(String, Option<String>)> {
        match self {
            MessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse { name, input, .. } => {
                        let file_path = input
                            .get("file_path")
                            .or_else(|| input.get("path"))
                            .or_else(|| input.get("command"))
                            .and_then(|v| v.as_str())
                            .map(std::string::ToString::to_string);
                        Some((name.clone(), file_path))
                    }
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: Option<String>,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "tool_use_id")]
        tool_use_id: Option<String>,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },

    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },

    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,

    #[serde(default)]
    pub output_tokens: u64,

    #[serde(default, rename = "cache_creation_input_tokens")]
    pub cache_creation_input_tokens: u64,

    #[serde(default, rename = "cache_read_input_tokens")]
    pub cache_read_input_tokens: u64,

    /// Per-TTL breakdown of `cache_creation_input_tokens`. 1h tokens bill
    /// at 2x base (vs 5m at 1.25x), so missing this split undercounts cost
    /// for 1h-TTL users. Absent on old JSONL → fall back to flat 5m rate.
    #[serde(default)]
    pub cache_creation: Option<CacheCreationBreakdown>,

    #[serde(default)]
    pub service_tier: Option<String>,

    #[serde(default)]
    pub speed: Option<String>,
}

impl Usage {
    /// Same "work tokens" concept as `aggregator::stats::TokenStats` and
    /// `infrastructure::cache::CachedFileStats` — input + output only,
    /// excluding cache. Named the same across all three so the formula has
    /// one name to grep for instead of inline arithmetic repeated per site.
    pub fn work_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CacheCreationBreakdown {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,

    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `toolUseResult` is foreign JSON: an edit carries a unified-diff
    /// `structuredPatch`, a create carries the whole new file in `content`
    /// with an EMPTY patch, and everything else is a string or an unrelated
    /// object. Getting any shape wrong must not cost the entry.
    #[test]
    fn line_churn_reads_every_tool_result_shape() {
        let churn = |json: &str| {
            serde_json::from_str::<LogEntry>(json)
                .unwrap_or_else(|e| panic!("entry must parse: {json} -> {e}"))
                .line_churn()
        };

        // Edit: only +/- count, context lines do not.
        assert_eq!(
            churn(
                r#"{"type":"user","toolUseResult":{"type":"update","structuredPatch":
                   [{"lines":[" ctx","-gone","+new","+also"]}]}}"#
            ),
            LineChurn {
                added: 2,
                removed: 1
            }
        );
        // Hunks accumulate.
        assert_eq!(
            churn(
                r#"{"type":"user","toolUseResult":{"structuredPatch":
                   [{"lines":["+a"]},{"lines":["-b","-c"]}]}}"#
            ),
            LineChurn {
                added: 1,
                removed: 2
            }
        );
        // Create: empty patch, whole file in `content`. Counting the patch
        // alone reports 0 for a file that was just written.
        assert_eq!(
            churn(
                r#"{"type":"user","toolUseResult":
                   {"type":"create","structuredPatch":[],"content":"one\ntwo\nthree"}}"#
            ),
            LineChurn {
                added: 3,
                removed: 0
            }
        );
        // A create whose content ends in a newline is still 3 lines.
        assert_eq!(
            churn(
                r#"{"type":"user","toolUseResult":
                   {"type":"create","structuredPatch":[],"content":"one\ntwo\nthree\n"}}"#
            ),
            LineChurn {
                added: 3,
                removed: 0
            }
        );
        // `content` is only a fallback: when a patch exists it wins, so an
        // edit's untouched file body is never counted as additions.
        assert_eq!(
            churn(
                r#"{"type":"user","toolUseResult":{"structuredPatch":
                   [{"lines":["+x"]}],"content":"a\nb\nc\nd"}}"#
            ),
            LineChurn {
                added: 1,
                removed: 0
            }
        );

        // One malformed hunk must not discard the well-formed ones.
        assert_eq!(
            churn(
                r#"{"type":"user","toolUseResult":{"structuredPatch":
                   [{"lines":["+a","+b"]},{"lines":[7]},{"lines":["-c"]}]}}"#
            ),
            LineChurn {
                added: 2,
                removed: 1
            }
        );
        // Shapes carrying no churn at all — each must still yield an entry.
        for json in [
            r#"{"type":"user","toolUseResult":"stdout text"}"#,
            r#"{"type":"user","toolUseResult":[{"type":"text","text":"x"}]}"#,
            r#"{"type":"user","toolUseResult":{"stdout":"ok","stderr":""}}"#,
            r#"{"type":"user","toolUseResult":{"structuredPatch":"@@ -1 +1 @@"}}"#,
            r#"{"type":"user","toolUseResult":null}"#,
            r#"{"type":"user"}"#,
        ] {
            assert_eq!(churn(json), LineChurn::default(), "{json}");
        }
    }

    /// A value of the wrong type must be read to its end. A mismatched array
    /// or object left half-read makes the NEXT key fail to parse, which costs
    /// the whole entry — so every case puts a field after the odd value and
    /// checks that it arrives.
    #[test]
    fn an_odd_tool_result_value_leaves_the_rest_of_the_entry_readable() {
        let read = |result: &str| {
            let json = format!(r#"{{"type":"user","toolUseResult":{result},"uuid":"after"}}"#);
            let entry = serde_json::from_str::<LogEntry>(&json)
                .unwrap_or_else(|e| panic!("entry must parse: {json} -> {e}"));
            assert_eq!(entry.uuid.as_deref(), Some("after"), "{result}");
            entry.line_churn()
        };
        // Agent / Task results: `content` is an array of blocks.
        assert_eq!(
            read(r#"{"content":[{"type":"text","text":"x"}],"status":"completed"}"#),
            LineChurn::default()
        );
        // A patch that is an object, not a list of hunks.
        assert_eq!(
            read(r#"{"structuredPatch":{"oops":1},"filePath":"/a"}"#),
            LineChurn::default()
        );
        // A non-string line: its neighbours still count.
        assert_eq!(
            read(r#"{"structuredPatch":[{"lines":["+a",7,{"x":1},"+b"]}],"filePath":"/a"}"#),
            LineChurn {
                added: 2,
                removed: 0
            }
        );
        // A `type` that is not a string.
        assert_eq!(
            read(r#"{"type":["create"],"content":"a\nb"}"#),
            LineChurn::default()
        );
    }

    /// `content` is a new file's body only on a Write (`type: create`).
    /// Grep's content mode and an Edit carry text there too, and counting it
    /// reports their output as added lines.
    #[test]
    fn content_counts_as_added_lines_only_for_a_create() {
        let churn = |result: &str| {
            serde_json::from_str::<LogEntry>(&format!(
                r#"{{"type":"user","toolUseResult":{result}}}"#
            ))
            .unwrap()
            .line_churn()
        };
        assert_eq!(
            churn(r#"{"mode":"content","content":"l1\nl2\nl3","numLines":3}"#),
            LineChurn::default(),
            "Grep output is not an edit"
        );
        assert_eq!(
            churn(r#"{"type":"update","structuredPatch":[],"content":"a\nb\nc"}"#),
            LineChurn::default(),
            "an Edit's file body is not new lines"
        );
        // Order-independent: `type` may follow `content`.
        assert_eq!(
            churn(r#"{"structuredPatch":[],"content":"a\nb","type":"create"}"#),
            LineChurn {
                added: 2,
                removed: 0
            }
        );
    }

    /// Reading a tool result must carry no recursion ceiling: transcripts
    /// embed third-party JSON, and a deeply nested payload that fails the
    /// entry takes the tokens that entry reports down with it.
    #[test]
    fn a_deeply_nested_tool_result_does_not_cost_the_entry() {
        let deep = format!(
            r#"{{"type":"assistant","uuid":"u1","message":{{"role":"assistant","usage":{{"input_tokens":100,"output_tokens":50}}}},"toolUseResult":{}1{}}}"#,
            "[".repeat(400),
            "]".repeat(400)
        );
        let entry = serde_json::from_str::<LogEntry>(&deep)
            .expect("a nested tool result must not fail the entry");
        assert_eq!(entry.line_churn(), LineChurn::default());
        assert_eq!(
            entry.message.and_then(|m| m.usage).map(|u| u.input_tokens),
            Some(100),
            "the entry's usage must survive"
        );
    }

    #[test]
    fn usage_work_tokens_excludes_cache() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 9_000,
            cache_read_input_tokens: 500_000,
            cache_creation: None,
            service_tier: None,
            speed: None,
        };
        assert_eq!(usage.work_tokens(), 150);
    }

    #[test]
    fn test_extract_text_empty() {
        let content = MessageContent::Empty;
        assert_eq!(content.extract_text(), "");
    }

    #[test]
    fn test_extract_text_string() {
        let content = MessageContent::Text("hello world".to_string());
        assert_eq!(content.extract_text(), "hello world");
    }

    #[test]
    fn test_extract_text_blocks_text_only() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "first".to_string(),
            },
            ContentBlock::Text {
                text: "second".to_string(),
            },
        ]);
        assert_eq!(content.extract_text(), "first\nsecond");
    }

    #[test]
    fn test_extract_text_blocks_with_thinking() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Thinking {
                thinking: "let me think".to_string(),
                signature: None,
            },
            ContentBlock::Text {
                text: "answer".to_string(),
            },
        ]);
        assert_eq!(content.extract_text(), "let me think\nanswer");
    }

    #[test]
    fn test_extract_text_blocks_skips_tool_use() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "before".to_string(),
            },
            ContentBlock::ToolUse {
                id: Some("t1".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/tmp/f.rs"}),
            },
            ContentBlock::Text {
                text: "after".to_string(),
            },
        ]);
        assert_eq!(content.extract_text(), "before\nafter");
    }

    #[test]
    fn test_extract_text_blocks_skips_tool_result() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "text".to_string(),
            },
            ContentBlock::ToolResult {
                tool_use_id: Some("t1".to_string()),
                content: serde_json::json!("result"),
                is_error: false,
            },
        ]);
        assert_eq!(content.extract_text(), "text");
    }

    #[test]
    fn test_extract_text_empty_blocks() {
        let content = MessageContent::Blocks(vec![]);
        assert_eq!(content.extract_text(), "");
    }

    #[test]
    fn test_extract_tool_calls_empty() {
        let content = MessageContent::Empty;
        assert!(content.extract_tool_calls().is_empty());
    }

    #[test]
    fn test_extract_tool_calls_text() {
        let content = MessageContent::Text("no tools".to_string());
        assert!(content.extract_tool_calls().is_empty());
    }

    #[test]
    fn test_extract_tool_calls_file_path() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: Some("t1".to_string()),
            name: "Read".to_string(),
            input: serde_json::json!({"file_path": "/src/main.rs"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "Read");
        assert_eq!(calls[0].1.as_deref(), Some("/src/main.rs"));
    }

    #[test]
    fn test_extract_tool_calls_path_fallback() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Glob".to_string(),
            input: serde_json::json!({"path": "/src", "pattern": "*.rs"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].1.as_deref(), Some("/src"));
    }

    #[test]
    fn test_extract_tool_calls_command_fallback() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "ls -la"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].0, "Bash");
        assert_eq!(calls[0].1.as_deref(), Some("ls -la"));
    }

    #[test]
    fn test_extract_tool_calls_file_path_priority_over_path() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Edit".to_string(),
            input: serde_json::json!({"file_path": "/a.rs", "path": "/b.rs"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].1.as_deref(), Some("/a.rs"));
    }

    #[test]
    fn test_extract_tool_calls_no_known_keys() {
        let content = MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: None,
            name: "Custom".to_string(),
            input: serde_json::json!({"foo": "bar"}),
        }]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls[0].0, "Custom");
        assert_eq!(calls[0].1, None);
    }

    #[test]
    fn test_extract_tool_calls_multiple() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::ToolUse {
                id: None,
                name: "Read".to_string(),
                input: serde_json::json!({"file_path": "/a.rs"}),
            },
            ContentBlock::Text {
                text: "thinking...".to_string(),
            },
            ContentBlock::ToolUse {
                id: None,
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "cargo build"}),
            },
        ]);
        let calls = content.extract_tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "Read");
        assert_eq!(calls[1].0, "Bash");
    }

    #[test]
    fn test_extract_tool_calls_skips_non_tool_blocks() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "text".to_string(),
            },
            ContentBlock::Thinking {
                thinking: "hmm".to_string(),
                signature: None,
            },
            ContentBlock::ToolResult {
                tool_use_id: None,
                content: serde_json::json!("ok"),
                is_error: false,
            },
        ]);
        assert!(content.extract_tool_calls().is_empty());
    }
}
