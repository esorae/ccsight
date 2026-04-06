#!/bin/bash
# Project linter for ccsight
# Checks for common pattern violations across all source files

set -e

ERRORS=0
UI_FILES="src/ui/mod.rs src/ui/dashboard.rs src/ui/insights.rs"
MAIN_FILE="src/main.rs"
SUMMARY_FILE="src/summary.rs"

# 1. Plain string titles (should be Span::styled)
PLAIN_TITLES=$(grep -n '\.title("' $UI_FILES 2>/dev/null || true)
if [ -n "$PLAIN_TITLES" ]; then
    echo "ERROR: Plain string titles found (use Span::styled with theme color):"
    echo "$PLAIN_TITLES"
    ERRORS=$((ERRORS + 1))
fi

PLAIN_FORMAT_TITLES=$(grep -n '\.title(format!' $UI_FILES 2>/dev/null || true)
if [ -n "$PLAIN_FORMAT_TITLES" ]; then
    echo "ERROR: format! titles without Span::styled found:"
    echo "$PLAIN_FORMAT_TITLES"
    ERRORS=$((ERRORS + 1))
fi

# 1b. .title(variable) without Span::styled (multi-line aware)
PLAIN_VAR_TITLES=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.read().split('\n')
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith('.title(') and 'Span::styled' not in stripped and 'Line::from' not in stripped:
            content = stripped[7:].rstrip(')')  .rstrip(',')
            if content and not content.startswith('\"') and not content.startswith('Span') and not content.startswith('Line'):
                print(f'  {fname}:L{i+1}: {stripped}')
" 2>/dev/null || true)
if [ -n "$PLAIN_VAR_TITLES" ]; then
    echo "ERROR: .title(variable) without Span::styled found:"
    echo "$PLAIN_VAR_TITLES"
    ERRORS=$((ERRORS + 1))
fi

# 2. borders(Borders::ALL) without border_style (multi-line aware)
MISSING_BORDER=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.read().split('\n')
    for i, line in enumerate(lines):
        if 'borders(Borders::ALL)' in line:
            window = '\n'.join(lines[i:i+8])
            if 'border_style' not in window:
                print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$MISSING_BORDER" ]; then
    echo "ERROR: borders(Borders::ALL) without border_style:"
    echo "$MISSING_BORDER"
    ERRORS=$((ERRORS + 1))
fi

# 3. Wrong date format (%y/ with 2-digit year, %m/%d instead of %m-%d, %b locale-dependent month)
WRONG_DATES=$(grep -n '%y/' $UI_FILES 2>/dev/null || true)
WRONG_SLASH=$(grep -n '%m/%d' $UI_FILES "$MAIN_FILE" 2>/dev/null || true)
WRONG_LOCALE=$(grep -n '%b' $UI_FILES "$MAIN_FILE" 2>/dev/null || true)
if [ -n "$WRONG_DATES" ] || [ -n "$WRONG_SLASH" ] || [ -n "$WRONG_LOCALE" ]; then
    echo "ERROR: Non-standard date format found (use %Y-%m-%d or %m-%d):"
    [ -n "$WRONG_DATES" ] && echo "$WRONG_DATES"
    [ -n "$WRONG_SLASH" ] && echo "$WRONG_SLASH"
    [ -n "$WRONG_LOCALE" ] && echo "$WRONG_LOCALE"
    ERRORS=$((ERRORS + 1))
fi

# 4. Cost with .4+ precision (never use .4 or higher)
WRONG_COST=$(grep -nE '\{[a-z_]*:\.[4-9]\}' $UI_FILES 2>/dev/null || true)
if [ -n "$WRONG_COST" ]; then
    echo "ERROR: Cost with .4+ precision found (use .0 or .2):"
    echo "$WRONG_COST"
    ERRORS=$((ERRORS + 1))
fi

# 4b. Direct cost formatting without format_cost() (use format_cost to prevent $-0.00)
DIRECT_COST=$(grep -nE 'format!\(".*\$\{.*cost.*:\.[0-9]\}' $UI_FILES src/ui/dashboard.rs src/ui/insights.rs 2>/dev/null | grep -v 'format_cost\|max(0.0)\|\.max(' || true)
if [ -n "$DIRECT_COST" ]; then
    echo "ERROR: Direct cost formatting (use format_cost() to prevent \$-0.00):"
    echo "$DIRECT_COST"
    ERRORS=$((ERRORS + 1))
fi

# 5. Raw u16 subtraction on area dimensions for popup sizing (use saturating_sub)
RAW_SUB=$(grep -nE '(popup|inner).*area\.(height|width)\s*-\s*[0-9]|area\.(height|width)\s*-\s*[0-9].*\)\.min' $UI_FILES 2>/dev/null || true)
if [ -n "$RAW_SUB" ]; then
    echo "ERROR: Raw subtraction on area dimensions (use saturating_sub):"
    echo "$RAW_SUB"
    ERRORS=$((ERRORS + 1))
fi

# 6. sessions.iter().enumerate() without subagent filter (main.rs)
UNFILTERED=$(python3 -c "
import re
with open('$MAIN_FILE') as f:
    content = f.read()
    lines = content.split('\n')
issues = []
for i, line in enumerate(lines):
    if '.sessions' in line and 'enumerate' in line and 'iter' in line:
        window = '\n'.join(lines[max(0,i-2):i+3])
        if 'is_subagent' not in window and 'filter' not in window:
            issues.append(f'  L{i+1}: {line.strip()}')
for issue in issues:
    print(issue)
" 2>/dev/null || true)
if [ -n "$UNFILTERED" ]; then
    echo "WARNING: sessions.iter().enumerate() without subagent filter:"
    echo "$UNFILTERED"
    echo "  (Verify this is intentional - selected_session uses filtered indices)"
fi

# 7. Scroll indicator using ↑↓ instead of ▲▼ (in scroll state indicators, not keybind help)
WRONG_SCROLL=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.read().split('\n')
    for i, line in enumerate(lines):
        if '↑↓' in line and 'scroll_indicator' in line:
            print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$WRONG_SCROLL" ]; then
    echo "ERROR: Scroll indicator using ↑↓ instead of ▲▼:"
    echo "$WRONG_SCROLL"
    ERRORS=$((ERRORS + 1))
fi

# 8. Direct project name shortening instead of shorten_project()
DIRECT_PROJECT=$(grep -n 'Path::new.*project_name.*file_name()' $UI_FILES "$SUMMARY_FILE" 2>/dev/null | grep -v 'shorten_project' || true)
if [ -n "$DIRECT_PROJECT" ]; then
    echo "ERROR: Direct project name shortening (use shorten_project()):"
    echo "$DIRECT_PROJECT"
    ERRORS=$((ERRORS + 1))
fi

# 9. Direct ui::load_conversation in main.rs (use spawn_load_conversation)
DIRECT_LOAD=$(python3 -c "
with open('$MAIN_FILE') as f:
    lines = f.readlines()
helper_lines = set()
for i, line in enumerate(lines):
    if 'fn spawn_load_conversation' in line:
        for j in range(max(0,i-1), min(len(lines), i+10)):
            helper_lines.add(j)
for i, line in enumerate(lines):
    if 'ui::load_conversation' in line and i not in helper_lines:
        print(f'  L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$DIRECT_LOAD" ]; then
    echo "ERROR: Direct ui::load_conversation call (use spawn_load_conversation()):"
    echo "$DIRECT_LOAD"
    ERRORS=$((ERRORS + 1))
fi

# 10. Inline weekday occurrence count calculation (use weekday_occurrence_count())
INLINE_WEEKDAY=$(python3 -c "
for fname in '$UI_FILES'.split():
    with open(fname) as f:
        lines = f.readlines()
    helper_lines = set()
    for i, line in enumerate(lines):
        if 'fn weekday_occurrence_count' in line:
            for j in range(max(0,i-1), min(len(lines), i+15)):
                helper_lines.add(j)
    for i, line in enumerate(lines):
        if i not in helper_lines and ('calendar_days' in line and ('/ 7' in line or '% 7' in line)):
            print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$INLINE_WEEKDAY" ]; then
    echo "ERROR: Inline weekday count calculation (use weekday_occurrence_count()):"
    echo "$INLINE_WEEKDAY"
    ERRORS=$((ERRORS + 1))
fi

# 11. Direct ratatui Scrollbar widget (use draw_scrollbar())
DIRECT_SCROLLBAR=$(grep -n 'Scrollbar::new\|ScrollbarState::new' $UI_FILES 2>/dev/null || true)
if [ -n "$DIRECT_SCROLLBAR" ]; then
    echo "ERROR: Direct ratatui Scrollbar widget (use draw_scrollbar()):"
    echo "$DIRECT_SCROLLBAR"
    ERRORS=$((ERRORS + 1))
fi

# 12. Legacy conversation_* fields on AppState (use panes instead)
LEGACY_CONV=$(grep -n 'state\.conversation_messages\|state\.conversation_scroll\|state\.conversation_rendered\|state\.conversation_file_path\|state\.conversation_loading\|state\.conversation_load_task\|state\.conv_search_mode\|state\.conv_search_query\|state\.conv_search_matches\|state\.conv_search_current\|state\.conv_search_saved_scroll\|state\.selected_conversation_message\|state\.conversation_message_lines\|state\.conversation_last_modified\|state\.conversation_reload_check\|state\.last_conversation_width' "$MAIN_FILE" $UI_FILES 2>/dev/null || true)
if [ -n "$LEGACY_CONV" ]; then
    echo "ERROR: Legacy conversation_* fields found (use panes instead):"
    echo "$LEGACY_CONV"
    ERRORS=$((ERRORS + 1))
fi

# 13. Inline pane initialization (use ConversationPane::load_from or open_conversation_in_pane)
INLINE_PANE=$(python3 -c "
with open('$MAIN_FILE') as f:
    lines = f.readlines()
helper_lines = set()
for i, line in enumerate(lines):
    if 'fn load_from' in line or 'fn open_conversation_in_pane' in line:
        for j in range(max(0,i-1), min(len(lines), i+25)):
            helper_lines.add(j)
for i, line in enumerate(lines):
    if 'load_task' in line and 'spawn_load_conversation' in line and i not in helper_lines:
        window = ''.join(lines[max(0,i-15):i+2])
        if 'needs_reload' not in window and 'reload_check' not in window:
            print(f'  L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$INLINE_PANE" ]; then
    echo "ERROR: Inline pane initialization (use ConversationPane::load_from()):"
    echo "$INLINE_PANE"
    ERRORS=$((ERRORS + 1))
fi

# 14. Direct sessions[idx] index access (use .iter().filter().nth() or .get())
DIRECT_SESSION_IDX=$(python3 -c "
import re
for fname in '$UI_FILES'.split() + ['$MAIN_FILE']:
    with open(fname) as f:
        for i, line in enumerate(f.readlines()):
            if re.search(r'\.sessions\[[a-z_]+\]', line.strip()):
                if 'actual_idx' in line or 'session_indices' in line:
                    continue
                print(f'  {fname}:L{i+1}: {line.strip()}')
" 2>/dev/null || true)
if [ -n "$DIRECT_SESSION_IDX" ]; then
    echo "WARNING: Direct sessions[idx] access (prefer .iter().filter().nth() or .get()):"
    echo "$DIRECT_SESSION_IDX"
fi

# 15. String::remove/insert with cursor (needs char_indices for UTF-8 safety)
UNSAFE_STRING_OP=$(grep -n '\.remove(.*cursor\|\.insert(.*cursor' "$MAIN_FILE" 2>/dev/null | grep -v 'char_indices\|byte_pos' || true)
if [ -n "$UNSAFE_STRING_OP" ]; then
    echo "ERROR: String::remove/insert with cursor without char_indices (UTF-8 unsafe):"
    echo "$UNSAFE_STRING_OP"
    ERRORS=$((ERRORS + 1))
fi

if [ $ERRORS -eq 0 ]; then
    echo "Lint: OK"
else
    echo ""
    echo "Lint: $ERRORS issue(s) found"
    exit 1
fi
