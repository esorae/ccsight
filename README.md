# ccsight

A Rust TUI for viewing Claude Code session logs with statistics, cost analysis, and conversation browsing.

## Features

- **Dashboard**: Costs, tokens, projects, models, tools, languages, heatmap, hourly patterns
- **Daily View**: Day-by-day sessions with activity graph, breakdown, conversation viewer
- **Insights**: Metrics, today vs average, weekly/monthly trends with detail popups
- **Conversation**: Multi-pane browsing with syntax highlighting, search, copy
- **Search**: Find sessions by project, summary, branch, session ID, date, or content
- **Pin**: Mark important sessions, browse across dates
- **MCP Server**: Expose stats/sessions tools via stdio transport
- **Caching**: Fast startup with JSON cache at `~/.cache/ccsight/`

## Installation

```bash
# Homebrew
brew install esorae/tap/ccsight

# Cargo
cargo install ccsight

# From source
cargo install --path .
```

> **macOS**: If downloading the binary directly from GitHub Releases, run `xattr -d com.apple.quarantine ccsight` to clear the Gatekeeper flag. Homebrew and `cargo install` are not affected.

## Usage

```bash
ccsight                    # Run TUI
ccsight --daily            # Print daily cost summary to stdout
ccsight --mcp              # Run as MCP server
ccsight --clear-cache      # Clear cache and reparse all files
ccsight --limit 50         # Limit to 50 most recent sessions
```

Press `?` in TUI for key bindings.

## Data Source

Reads JSONL session files from `~/.claude/projects/`.

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)
