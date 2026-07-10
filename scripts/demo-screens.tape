# vhs tape: README static screenshots, one per surface. Run via
# scripts/demo-assets.sh; the long hidden sleep covers cold-start data load
# so the search step and final dashboard shot see the committed index.
# vhs captures Screenshots on a delayed render pipeline (a few seconds
# behind the command), hence the idle Sleeps around every Screenshot.

Output docs/assets/screens-run.gif

# 120x35 cells at a larger font: fewer columns + bigger glyphs stay
# readable when the README scales the image down.
Set FontSize 17
Set Width 1370
Set Height 764
Set WindowBar Colorful
Set Padding 10
Set Framerate 20
Set TypingSpeed 60ms

Hide
Type "export HOME=/tmp/ccsight-demo-home && exec ./target/release/ccsight"
Enter
Sleep 90
Show

Type "2"
Sleep 3
Screenshot docs/assets/live.png
Sleep 4

Type "3"
Sleep 3
Screenshot docs/assets/daily.png
Sleep 4

Type "4"
Sleep 3
Screenshot docs/assets/insights.png
Sleep 4

Type "1"
Sleep 1
Type "/"
Sleep 1
Type "scheduler"
# Longer than the other captures: this frame has to absorb the typing time
# AND the async tantivy query, or it lands mid-word with the title still
# reading "[Searching...]".
Sleep 10
Screenshot docs/assets/search.png
Sleep 4

Enter
Sleep 4
Screenshot docs/assets/conversation.png
Sleep 4

# Dashboard last: by now the tantivy index has committed, so the header
# shows the clean state instead of "building search index...".
Escape
Sleep 1
Escape
Sleep 3
Screenshot docs/assets/dashboard.png
Sleep 4
