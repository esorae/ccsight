# CLAUDE.md

See [README.md](README.md) for usage. Press `?` in TUI for key bindings.

## Build & Development

```bash
cargo build --release        # Build
cargo run                    # Run TUI
cargo test                   # Run tests
cargo clippy -- -D warnings  # Lint (warnings are errors)
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
- `cargo clippy -- -D warnings` must pass (warnings are errors)
- `scripts/lint.sh` must exit 0
- `cargo test` must pass all tests

## Architecture

```
src/
├── main.rs              # Entry point, event loop
├── state.rs             # AppState, ConversationPane, TextInput, Tab
├── ui/
│   ├── mod.rs           # draw(), daily, conversation, popups
│   ├── dashboard.rs     # Dashboard panels + detail popup + sparklines
│   └── insights.rs      # Insights panels + detail popup
├── search.rs            # Metadata search + content fallback
├── mcp.rs               # MCP server (stats/sessions/search tools)
├── domain.rs            # LogEntry, Message, ContentBlock
├── parser.rs            # JSONL parsing
├── aggregator/          # Stats, pricing, daily grouping
└── infrastructure/
    ├── cache.rs         # JSON cache (~/.cache/ccsight/cache.json)
    ├── file_discovery.rs
    └── search_index.rs  # tantivy full-text index (~/.cache/ccsight/index/)
```

## Rules (lint enforced)

Checked by `scripts/lint.sh`. Violations block commit.

| Rule | Bad | Good |
|------|-----|------|
| Borders need style | `.borders(Borders::ALL)` | `.borders(Borders::ALL).border_style(theme::BORDER)` |
| Titles need Span | `.title("Foo")` | `.title(Span::styled(" Foo ", theme::PRIMARY))` |
| No raw u16 subtract | `area.height - 4` | `area.height.saturating_sub(4)` |
| Date format | `%y/`, `%m/%d`, `%b` | `%Y-%m-%d` or `%m-%d` |
| Cost formatting | `format!("${cost:.2}")` | `format_cost(cost, 2)` |
| Scroll indicators | `↑↓` in content | `▲▼` (↑↓ only for keybind help) |
| Project name | `Path::new(&name).file_name()` | `shorten_project(&name)` |
| Conv loading | `ui::load_conversation(...)` | `spawn_load_conversation(...)` |
| Scrollbar | `Scrollbar::new` | `draw_scrollbar()` |
| Legacy fields | `state.conversation_messages` | `state.panes` |
| Pane init | `pane.load_task = Some(spawn_...)` | `ConversationPane::load_from()` |
| sessions[] index | `group.sessions[idx]` | `.iter().filter().nth()` or `.get()` |
| Text input | `.remove(cursor)` / `.insert(cursor,c)` | `TextInput` methods |

## Rules (manual review)

| Rule | Detail |
|------|--------|
| Footer format | `key: action` with double-space separator |
| Session enumerate | `.filter(!is_subagent).enumerate()` — never enumerate before filter |
| Popup overlay guards | Guards in ALL 4: `handle_mouse_click`, `handle_double_click`, `handle_mouse_scroll`, `dismiss_overlay` |
| AppState new field | Update ALL: `state.rs`, `run()` init, `test_helpers.rs`, `ui/mod.rs` test init |
| TextInput usage | All text inputs use `TextInput` struct — never raw `String` + `usize` cursor |
| Search preview | Enter saves state via `search_saved_state`, Esc restores original tab/position |

## Key Patterns

- **Full-text search**: tantivy ngram(2,3). `SearchIndex::update_or_build()` handles build/incremental/reuse. Parallel parsing with rayon. `--clear-cache` clears index too.
- **TextInput**: Shared struct for all inputs. Methods: `insert_char`, `delete_back`, `move_left/right/home/end`, `clear`, `set`, `render_spans`.
- **Search state**: `[Normal] → / → [Search] → Enter → [Preview] → Esc → [Search] → Esc → [Normal]`
- **Pane search**: VS Code style — Enter/Shift+Enter = next/prev match, Esc closes bar (n/N still work after).
- **Sparklines**: Models/Projects detail popups show usage sparklines with shared X/Y axes.
- **Async**: Background threads for data loading, summary, index building. `mpsc::channel` for results.
- **MCP tools**: `stats` (metrics), `sessions` (list/detail + `conversation_query`), `search` (tantivy full-text). All share `date_from`/`date_to` param naming. Local timezone.
