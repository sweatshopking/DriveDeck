#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/drivedeck"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/runtime" "$tmp/home/.config/onedrive"
chmod 700 "$tmp/runtime"

XDG_RUNTIME_DIR="$tmp/runtime" HOME="$tmp/home" dbus-run-session -- bash -c '
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
