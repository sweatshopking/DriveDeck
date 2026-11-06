#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/drivedeck"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/runtime" "$tmp/home/.config/onedrive" "$tmp/bin"
# A stand-in xdg-open that only records the address, so the check never opens a real browser
printf '#!/bin/sh\nprintf "%%s\\n" "$1" >> "%s/opened"\n' "$tmp" > "$tmp/bin/xdg-open"
chmod +x "$tmp/bin/xdg-open"
chmod 700 "$tmp/runtime"

PATH="$tmp/bin:$PATH" DISPLAY= WAYLAND_DISPLAY= XDG_RUNTIME_DIR="$tmp/runtime" HOME="$tmp/home" dbus-run-session -- bash -c '
    set -euo pipefail
    "$1" --gnome-search-test >"$2/provider.log" 2>&1 &
    pid=$!
    trap "kill $pid 2>/dev/null || true" EXIT
    ready=0
    for _ in $(seq 1 50); do
        if gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
            --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
            --method org.gnome.Shell.SearchProvider2.GetInitialResultSet "['\''x'\'']" \
            >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.1
    done
    test "$ready" -eq 1
    gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
        --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
        --method org.gnome.Shell.SearchProvider2.GetInitialResultSet "['\''x'\'']"
    gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
        --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
        --method org.gnome.Shell.SearchProvider2.GetResultMetas "[]"
    gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
        --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
        --method org.gnome.Shell.SearchProvider2.GetSubsearchResultSet "[]" "['\''x'\'']"
    gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
        --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
        --method org.gnome.Shell.SearchProvider2.ActivateResult "file:///etc/passwd" "[]" 0
    gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
        --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
        --method org.gnome.Shell.SearchProvider2.LaunchSearch "['\''x'\'']" 0
' -- "$BIN" "$tmp"
# LaunchSearch must have asked for exactly the Office search page; ActivateResult of a bad
# address must have opened nothing
sleep 0.3
test "$(cat "$tmp/opened")" = "https://www.office.com/search?q=x"
echo "LaunchSearch opened (recorded, not a real browser): $(cat "$tmp/opened")"
