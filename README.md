# ccsight

Claude Code session analytics in the terminal, read from your local
`~/.claude/` logs.

![Dashboard](docs/assets/dashboard.png)

## Why ccsight

`/usage` and other trackers already give you cost totals. ccsight adds:

- **Find & resume past work** — full-text search over every conversation,
  open the transcript, copy a ready-to-paste `claude -r` resume command.
- **Know when a session needs you** — the Live tab tracks busy / paused
  sessions across reboots; `--wait` blocks until a session awaits input,
  stalls, errors, or ends.
- **Audit your setup** — every MCP server, skill, command, and subagent
  marked active, stale, or never used.

Plus cost with the 5m / 1h cache-write split, per-project / model / tool /
language breakdowns, heatmaps, and trends.

<img src="docs/assets/live.png" alt="Live sessions" width="49%"><img src="docs/assets/search.png" alt="Full-text search" width="49%">
<img src="docs/assets/conversation.png" alt="Conversation viewer" width="49%"><img src="docs/assets/insights.png" alt="Insights" width="49%">

<details>
<summary>More screenshots: Daily view</summary>

![Daily view](docs/assets/daily.png)

</details>

## Installation

Homebrew:

```bash
brew install esorae/tap/ccsight
```

crates.io:

```bash
cargo install ccsight
```

From source, inside a clone:

```bash
cargo install --path .
```

> **macOS**: a binary downloaded straight from GitHub Releases needs `xattr -d com.apple.quarantine ccsight`. Homebrew and `cargo install` are not affected.

## Usage

```bash
ccsight                              # TUI
ccsight --daily | --weekly | --monthly [--json]
ccsight --wait <id> && say "done"    # block until that session needs you
ccsight --mcp                        # MCP server over stdio
```

`ccsight --help` lists every flag; `?` in the TUI lists the key bindings.

### Reading the numbers

**Work** tokens and the headline token totals are input + output; cache
traffic would drown them. **Cost** includes everything, each request priced at
the rate it got (fast mode, size tiers, dated price changes, fallback
attempts). `$?` or a trailing `*` marks a figure with unknown pricing.

### Search

`/` searches titles, summaries, session IDs, projects, branches, dates, and
conversation content. Sessions the query names come first, then content
matches by relevance and recency, then project / branch / date-only matches.
Narrow with tokens:

| Token | Effect |
|-------|--------|
| `filter:live` / `filter:paused` / `filter:busy` | Sessions in the current Live poll |
| `filter:today` / `filter:week` / `filter:month` | Today, last 7 days, last 30 days (local time) |
| `filter:date:YYYY-MM-DD` | Exact date |
| `project:NAME` / `branch:NAME` / `model:NAME` | Substring match (case-insensitive) |

```text
/filter:today project:ccsight             # today's ccsight sessions
/filter:month model:opus mcp setup        # last 30 days, Opus, containing "mcp setup"
```

Date filters apply to the date the matched message was written.

## MCP Server

```bash
claude mcp add --scope user ccsight -- ccsight --mcp
```

Tools: `stats`, `sessions`, `search`, `live_sessions`. The first three take
`date_from` / `date_to` (`YYYY-MM-DD`, local time). Other hosts: add
`ccsight --mcp` as a stdio server.

## Data Source

- `~/.claude/projects/**/*.jsonl` — session logs.
- `~/.claude.json` + enabled-plugin `.mcp.json` — MCP config, to mark each
  server active, stale (configured but idle), or inactive (no longer
  configured).
- `~/.claude/{skills,commands,agents}/` + enabled-plugin paths — installed
  entries, so never-used ones show up.
- `~/Library/Application Support/Claude/local-agent-mode-sessions/` (macOS) —
  Claude Desktop Cowork sessions; one that fails to parse is skipped.

State lives under `~/.ccsight/` (cache, search index, pins, live history);
`--clear-cache` keeps pins and history.

## License

[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)
