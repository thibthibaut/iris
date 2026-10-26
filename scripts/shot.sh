#!/bin/bash
# shot.sh URL OUT [W,H] — headless Brave screenshot for checking the GUI.
# The old OUT is removed first, so a failed capture never passes for a new one.
set -u
cd "$(dirname "$0")/.."
SIZE=${3:-1400,1000}
rm -f "$2"
perl -e 'alarm 45; exec @ARGV' "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser" --headless=new --disable-gpu --hide-scrollbars --window-size="$SIZE" --user-data-dir="$PWD/.iris-cache/brave-profile" --screenshot="$2" "$1" >/dev/null 2>&1
ls -la "$2" 2>&1 | awk '{print $5, $9}'
