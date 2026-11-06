#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/debug/drivedeck"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/runtime" "$tmp/home/.config/onedrive" "$tmp/bin"
chmod 700 "$tmp/runtime"
for command in xdg-open gio kde-open; do
    cat >"$tmp/bin/$command" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$tmp/opened.log"
exit 0
EOF
    chmod +x "$tmp/bin/$command"
done

PATH="$tmp/bin:$PATH" XDG_RUNTIME_DIR="$tmp/runtime" HOME="$tmp/home" dbus-run-session -- bash -c '
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
    sleep 1
    if [ -s "$2/opened.log" ] || [ -e "$2/opened.log" ]; then
        echo "error: an unsafe file activation opened something: $(cat "$2/opened.log" 2>/dev/null || true)" >&2
        exit 1
    fi
    gdbus call --session --dest io.github.sweatshopking.DriveDeck.GnomeSearch \
        --object-path /io/github/sweatshopking/DriveDeck/GnomeSearch \
        --method org.gnome.Shell.SearchProvider2.LaunchSearch "['\''x'\'']" 0
    for _ in $(seq 1 20); do
        [ -e "$2/opened.log" ] && break
        sleep 0.1
    done
    expected="https://www.office.com/search?q=x"
    [ -f "$2/opened.log" ] || {
        echo "error: LaunchSearch did not invoke the fake opener" >&2
        exit 1
    }
    [ "$(wc -l <"$2/opened.log")" -eq 1 ] || {
        echo "error: expected exactly one opened URL, got: $(cat "$2/opened.log")" >&2
        exit 1
    }
    opened="$(cat "$2/opened.log")"
    [ "$opened" = "$expected" ] || {
        echo "error: expected $expected, got $opened" >&2
        exit 1
    }
    printf '%s\n' "$opened"
' -- "$BIN" "$tmp"
