#!/bin/bash
# Smoke test scenario for ccsight TUI.
#
# Walks through the regression-prone surfaces (Tools popup, search → conv pane,
# filter / project popups, Insights detail) and captures each step as a text
# file under /tmp. Use 140x45 — popup-width / content-width math assumes this
# size; smaller terminals truncate rightmost columns silently.
#
# Watch the captures for: rightmost column truncation, popup borders without
# `border_style`, `▼/▶` arrow inversion, "Searching..." stuck after Enter.

set -e

cargo build --release

tmux kill-session -t ccsight_smoke 2>/dev/null || true
tmux new-session -d -s ccsight_smoke -x 140 -y 45 'target/release/ccsight'
sleep 5  # data load + tantivy index build

# Dashboard → Tools popup (Built-in synthetic + MCP servers, expandable)
tmux send-keys -t ccsight_smoke '1' Enter && sleep 1
tmux send-keys -t ccsight_smoke 'l l l' && sleep 0.5  # cycle to Ecosystem panel
tmux send-keys -t ccsight_smoke Enter && sleep 1      # open detail
tmux capture-pane -t ccsight_smoke -p > /tmp/ccsight_tools.txt
tmux send-keys -t ccsight_smoke 'q' && sleep 0.5

# Search → conversation pane
tmux send-keys -t ccsight_smoke '/' && sleep 0.5
tmux send-keys -t ccsight_smoke 'mcp' && sleep 1
tmux send-keys -t ccsight_smoke Enter && sleep 1
tmux capture-pane -t ccsight_smoke -p > /tmp/ccsight_search.txt
tmux send-keys -t ccsight_smoke Escape && sleep 0.3
tmux send-keys -t ccsight_smoke Escape && sleep 0.3

# Filter / Project popup (popup overlay guard regression target)
tmux send-keys -t ccsight_smoke 'f' && sleep 0.5 && tmux send-keys -t ccsight_smoke Escape && sleep 0.3
tmux send-keys -t ccsight_smoke 'p' && sleep 0.5 && tmux send-keys -t ccsight_smoke Escape && sleep 0.3

# Insights → detail popup (4-panel cycle)
tmux send-keys -t ccsight_smoke '3' && sleep 1
tmux send-keys -t ccsight_smoke 'i' && sleep 1
tmux send-keys -t ccsight_smoke 'Right' && sleep 0.5
tmux capture-pane -t ccsight_smoke -p > /tmp/ccsight_insights.txt

tmux kill-session -t ccsight_smoke 2>/dev/null || true

echo "Captures: /tmp/ccsight_{tools,search,insights}.txt"
