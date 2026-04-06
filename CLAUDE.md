# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

See [README.md](README.md) for usage and key bindings.

## Build & Development

```bash
cargo build --release        # Build
cargo run                    # Run TUI
cargo run -- --daily         # Print daily cost summary to stdout
cargo run -- --clear-cache   # Clear cache
cargo run -- --limit 50      # Limit to 50 most recent sessions
cargo test                   # Run tests
cargo clippy -- -D warnings  # Lint (warnings are errors)
cargo fmt                    # Format
bash scripts/lint.sh         # Project lint (UI patterns, safety)
```

**Before committing, always run all 3 checks and fix any issues:**
```bash
cargo test && cargo clippy -- -D warnings && bash scripts/lint.sh
```

**Visual UI verification (tmux headless capture):**
```bash
cargo build --release
tmux new-session -d -s ccsight_test -x 120 -y 40 'target/release/ccsight'
sleep 3
tmux send-keys -t ccsight_test '3' && sleep 1        # Switch to Insights tab
tmux send-keys -t ccsight_test 'i' && sleep 1        # Open detail popup
tmux capture-pane -t ccsight_test -p                  # Print to stdout
tmux kill-session -t ccsight_test 2>/dev/null
```
Use this to verify UI rendering after visual changes. Key mappings: `1`=Dashboard, `2`=Daily, `3`=Insights, `i`=detail, `Enter`=open conversation, `q`=quit.
- `cargo clippy -- -D warnings` must pass (warnings are errors)
- `scripts/lint.sh` must exit 0
- `cargo test` must pass all tests

## Architecture

```
src/
├── main.rs            # Entry point, event loop (~4200 lines)
├── state.rs           # AppState, ConversationPane, Tab, PeriodFilter
├── ui/                # Ratatui rendering
│   ├── mod.rs         # draw(), daily, conversation, popups (~5000 lines)
│   ├── dashboard.rs   # Dashboard tab panels + detail popup
│   └── insights.rs    # Insights tab panels + detail popup
├── summary.rs         # AI summary generation (claude CLI)
├── cli.rs             # CLI mode for --daily flag
├── search.rs          # Session search functionality
├── mcp.rs             # MCP server (stats/sessions tools)
├── pins.rs            # Pin persistence (~/.config/ccsight/pins.json)
├── test_helpers.rs    # Test AppState builder
├── domain.rs          # Data structures (LogEntry, Message, ContentBlock)
├── parser.rs          # JSONL file parsing
├── conversation.rs    # Conversation loading, message formatting
├── text.rs            # Text formatting, code block parsing
├── aggregator/        # Stats aggregation, cost calculation, daily grouping
│   ├── stats.rs       # StatsAggregator, TokenStats
│   ├── pricing.rs     # CostCalculator with per-model pricing
│   └── grouping.rs    # DailyGrouper, SessionInfo
└── infrastructure/    # File discovery, caching
    ├── cache.rs       # JSON cache at ~/.cache/ccsight/cache.json
    └── file_discovery.rs  # Glob-based JSONL discovery
```

## Rules (lint enforced)

The following are checked by `scripts/lint.sh`. Violations block commit.

| Rule | Bad | Good | Lint check |
|------|-----|------|-----------|
| Borders need style | `.borders(Borders::ALL)` | `.borders(Borders::ALL).border_style(theme::BORDER)` | #2 |
| Titles need Span | `.title("Foo")` | `.title(Span::styled(" Foo ", theme::PRIMARY))` | #1 |
| No raw u16 subtract | `area.height - 4` | `area.height.saturating_sub(4)` | #5 |
| Date format | `%y/`, `%m/%d`, `%b` | `%Y-%m-%d` or `%m-%d` | #3 |
| Cost precision | `${:.4}` | `${:.0}` (summary) or `${:.2}` (detail) | #4 |
| Cost formatting | `format!("${cost:.2}")` | `format_cost(cost, 2)` (prevents `$-0.00`) | #4b |
| Scroll indicators | `↑↓` in content | `▲▼` (↑↓ is only for keybind help text) | #7 |
| Project name shortening | `Path::new(&name).file_name()...` | `shorten_project(&name)` | #8 |
| Conversation loading | `ui::load_conversation(...)` in main.rs | `spawn_load_conversation(...)` | #9 |
| Weekday count calc | Inline `calendar_days / 7` | `weekday_occurrence_count(...)` | #10 |
| Scrollbar widget | `Scrollbar::new` / `ScrollbarState::new` | `draw_scrollbar()` | #11 |
| Legacy conv fields | `state.conversation_messages` etc. | Use `state.panes` | #12 |
| Inline pane init | `pane.load_task = Some(spawn_...)` | `ConversationPane::load_from()` | #13 |
| sessions[] direct index | `group.sessions[idx]` | `.iter().filter().nth()` or `.get()` | #14 (warn) |
| String cursor UTF-8 | `.remove(cursor)` / `.insert(cursor,c)` | Use `char_indices` for byte pos | #15 |

## Rules (manual review)

Not enforceable by lint. Must be checked during code review.

| Rule | Detail |
|------|--------|
| Footer format | `key: action` with double-space separator. e.g. `j/k: scroll  q: close` |
| Session enumerate | `group.sessions.iter().filter(!is_subagent).enumerate()` — never enumerate before filter |
| Popup overlay guards | New popup needs guards in ALL 4 functions: `handle_mouse_click`, `handle_double_click`, `handle_mouse_scroll`, `dismiss_overlay` |
| AppState new field | Update ALL: `state.rs` struct, `run()` init, `test_helpers.rs`, `ui/mod.rs` test init |
| Help bar sync | Update ALL 4 locations (Dashboard/Daily/Insights/Conversation) when adding global keybinds |
| Summary priority | `summary.or(custom_title).unwrap_or("—")` — summary first, custom_title fallback |
| Popup click guard | `has_blocking_popup()` in `handle_mouse_click` blocks clicks behind popups |

## Lint improvement policy

When a bug is found or UI inconsistency is fixed, consider whether a lint rule can prevent recurrence. If the pattern is detectable by grep/python, add it to `scripts/lint.sh` and document in the rules table above. Prefer automated enforcement over manual review.

## Key Patterns

- **Caching**: `Cache` stores parsed stats per file (keyed by path + mtime). Use `--clear-cache` to force reparse.
- **Async UI**: Background threads for data loading, summary generation, content search. Uses `mpsc::channel` for thread communication.
- **Syntax highlighting**: `syntect` with lazy-loaded themes. `warmup_syntax_highlighting()` runs on startup thread.
- **Token counting**: Tracks input/output/cache_creation/cache_read tokens separately per model.
- **Conversation reload**: Timestamp-based focus restoration to prevent flickering. Skips Thinking messages when auto-following.
