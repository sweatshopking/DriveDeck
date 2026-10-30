# DriveDeck 1.1 plan

Each task is marked **Copilot** or **Claude**. Copilot does only its own tasks, one per session,
in order C1 → C5, on branch `copilot-1.1`. Claude works on its tasks on its own branch and
reviews everything before release. Read `.github/copilot-instructions.md` first.

C5 (translations) runs **last**, after the other work is merged, so every new string gets
translated too.

---

## C1 · Copilot · "Open in Office on the web" from Dolphin

**Why:** most Linux users don't have Microsoft Office. Word, Excel and PowerPoint files in
OneDrive can be opened and edited in the browser for free.

**What to build**
- New command-line action `--office-online <file>`: find the file's OneDrive item and open its
  web address in the browser. Reuse `share::view_online` and `graph::item_web_url`. For Office
  files OneDrive's `webUrl` opens the Office web editor. Check that for both a personal and a
  work account item and say in the Done notes if you couldn't.
- A Dolphin right-click action **"Edit in Office on the web"** shown **only** for Office
  documents. A KDE service menu's `MimeType=` applies to the whole file, so put it in a second
  service menu file, `packaging/drivedeck-servicemenu-office.desktop`, with the Word, Excel and
  PowerPoint MIME types (doc/docx/docm, xls/xlsx/xlsm, ppt/pptx/pptm, and OpenDocument
  odt/ods/odp, which Office on the web also opens). Same `X-KDE-Submenu=OneDrive`.
- Install and remove it exactly like the existing service menu: see `src/share.rs` (where
  `drivedeck-servicemenu.desktop` is included and written) and `src/uninstall.rs`. The deb/rpm
  packages ship the existing file. Find where (`grep -rn servicemenu packaging`) and add the
  new file the same way. This task may touch `packaging/` for that only.
- A file that isn't inside a OneDrive folder shows the same message `--view-online` shows.
- Add `--office-online <file>` to the `--help` text in `src/main.rs`.

**Done when:** the right-click entry appears only on Office files and opens the browser editor,
and the checks in the instructions pass.

**Done notes:**

Added `--office-online` using the existing `view_online`/`item_web_url` flow and an Office-only
Dolphin action for Word, Excel, PowerPoint, and OpenDocument files. Added AppImage menu install,
refresh, and removal support, plus deb/rpm packaging entries. Updated `src/main.rs`, `src/share.rs`,
`src/uninstall.rs`, `packaging/drivedeck-servicemenu-office.desktop`,
`packaging/build.sh`, and `packaging/drivedeck.spec.in`. CLI help was smoke-tested; formatting,
Clippy, and all 89 tests pass. I couldn't verify a live personal or work account item because no
signed-in test accounts were available.

---

## C2 · Copilot · Command-line status and links

**Why:** people who use scripts, status bars (waybar, polybar) or the terminal want the sync
state and share links without opening a window.

**What to build** (new options in `src/main.rs`, same `--option` style as the existing ones)
- `--status`: one line per account, `<account name>: <status text>`, using the same status text
  the popup shows (see how `src/popup.rs` gathers each account's `Status` and calls
  `Status::text`). Exit code 0 if everything is fine, 1 if any account has an error or needs
  attention, 2 if the onedrive client isn't installed.
- `--status --json`: the same as JSON: an array of objects with `account`, `folder`, `phase`
  (lowercase name of `core::Phase`), `text`, `last_complete` (RFC 3339 or null),
  `last_error` (or null) and `not_synced` (count). Use `serde_json`, already a dependency (check
  `Cargo.toml`; if it isn't, write the JSON by hand with proper escaping and a unit test).
- `--print-link <file> [--edit]`: print the share link to stdout instead of copying it and
  showing a notification. Refactor `share::share_link` so the link-making part returns
  `Result<String, String>` and both paths use it. Errors go to stderr, exit code 1.
- `--sync-now`: same as the tray's *Sync now* (find what the tray menu calls; it ends up in
  `core::sync_now`).
- Update `--help` and add a short "From the terminal" section to `README.md` near the end of
  "What it does".

**Done when:** all four work from a terminal with no tray running, and there are unit tests for
the JSON output (build a fake `Status`) and the exit-code choice.

**Done notes:**

Added `--status` and JSON output with the popup's status text, `--print-link` for view or edit links,
and `--sync-now` using the tray's account selection. Updated CLI help and added terminal examples
in `README.md`. Changed `src/main.rs` and `src/share.rs`; added tests for the JSON shape and exit
codes. CLI help and `--print-link` error handling were smoke-tested. `cargo fmt --check`, Clippy,
and all 91 tests pass. Live sync and Microsoft link creation were not exercised against accounts.

---

## C3 · Copilot · "Skip developer folders" preset

**Why:** syncing a `.git` folder through OneDrive can corrupt the repository (files arrive out
of order), and `node_modules`/`target` folders are huge and pointless to sync.

**What to build**
- In the Options page, next to the *skip folders* field (`o-skip-dirs`; see `load_options` and
  `clean_skip_dirs` in `src/ui.rs`), add a small button **"Add developer folders…"**.
- It opens a short dialog listing the names it will add, each with a checkbox, all ticked:
  `.git`, `node_modules`, `target`, `__pycache__`, `.venv`, `venv`, `.gradle`, `.idea`,
  `.cache`, and unticked by default: `build`, `dist` (those names are common for ordinary
  folders too, and the dialog says so in one line).
- *Add* appends the ticked names that aren't there already to the field, in the client's
  `skip_dir` format (`|`-separated; check `clean_skip_dirs` for exactly what's accepted). It
  only edits the field. Nothing is saved until the user presses the normal Save, which
  already runs the resync preview when `skip_dir` changes. Don't bypass that.
- Unit test: appending to an empty field, to a field that already has some of the names, and
  that no name is added twice.

**Done when:** the button, dialog and append work, Save shows the usual preview, and the
checks pass.

**Done notes:**

Added the developer-folder button and checkbox dialog to the Options page. Add appends selected
names to the unsaved skip-folder field without duplicates; the normal Save action still runs its
existing resync preview. Changed `ui/app.slint` and `src/ui.rs`, with tests for empty fields,
overlapping entries, and duplicates. `cargo fmt --check`, Clippy, and all 92 tests pass.

---

## C4 · Copilot · "Save screenshots to OneDrive"

**Why:** the Windows OneDrive app has this switch, and it's a common reason people want
OneDrive at all.

**What to build**
- In *App settings*, a switch **"Save screenshots to OneDrive"** with an account picker if
  there's more than one account. Off by default, never turned on by itself.
- Shown only when a supported screenshot tool is found. Supported:
  - **Spectacle** (KDE): `~/.config/spectaclerc`, group `[ImageSave]`, key `imageSaveLocation`
    (a `file://` URL). Check the key name against the installed Spectacle version
    (`spectacle --version`; read the current file) and note it in Done notes.
  - **Flameshot**: `~/.config/flameshot/flameshot.ini`, `[General]` `savePath`.
  - GNOME's built-in screenshot tool has no setting for this. Don't show the switch for it.
- Turning it on: create `<account folder>/Pictures/Screenshots` if missing, remember the tool's
  current value in `AppSettings` (new optional field, `skip_serializing_if` like the others),
  then set the new value. Edit the ini file in place keeping every other line and comment
  exactly as it was (write a small helper with unit tests; use `core::atomic_write`). Don't
  rewrite the whole file through a parser that drops comments.
- Turning it off: put back the remembered value. If the user changed the tool's setting by
  hand since, leave it alone and say so in one line.
- This is a change to another app's settings, so it is allowed **only** because the user
  flips a switch that says exactly what happens (rule 1). The switch's help line says:
  "Changes Spectacle's save folder. Turning this off puts it back."

**Done when:** on/off/on round-trips leave the tool's file byte-identical apart from the one
value, with unit tests for the ini helper (key present, key missing, group missing, comments
and blank lines kept).

**Done notes:**

Added the screenshot switch and account picker, with opt-in folder setup and guarded restoration
for Spectacle and Flameshot. The INI helper preserves unrelated lines and comments; tests cover
missing keys/groups, round trips, and user edits. Changed `src/core.rs`, `src/ui.rs`, and
`ui/app.slint`. On this machine, Spectacle 6.8.2 was installed and its current config was read:
it contains `lastImageSaveLocation`, while the binary also contains `imageSaveLocation`; the
implementation uses the task-specified `imageSaveLocation`. The existing config was not changed.
Flameshot was unavailable, so its live behavior could not be checked. `cargo fmt --check`,
Clippy, and all 98 tests pass.

---

## C5 · Copilot · Translations (run last)

**Why:** an English-only app reaches a fraction of Linux users.

**Approach** (no runtime files, so the AppImage, deb and rpm all work the same)
- **Slint strings:** wrap every user-visible string in `ui/app.slint` in `@tr("…")` (plural
  forms with `@tr("{n} file" | "{n} files" % n)`). Bundle the translations into the binary
  with slint-build's bundled-translations option in `build.rs`, and choose the language at
  startup with `slint::select_bundled_translation`. **Check both against the Slint 1.18 docs**
  (names, expected folder layout); don't guess.
- **Rust strings** (notifications, status text, errors, tray menu, `filewin.rs`, `popup.rs`):
  add a small `tr!` macro in a new `src/i18n.rs`. `build.rs` reads the same `.po` files and
  generates a lookup table compiled into the binary. Pick the language from `LANGUAGE`,
  `LC_ALL`, `LC_MESSAGES`, `LANG` in that order, then fall back from `pt_BR` to `pt` to English.
  Support plurals for the counts used in the app. A minimal `.po` reader in `build.rs` is fine
  (msgid, msgid_plural, msgstr, msgstr[n], escapes, multi-line strings). Unit-test it.
- **Don't translate**: log lines, the onedrive client's own messages quoted to the user,
  config keys, command-line `--help`.
- **One catalog**: `po/drivedeck.pot` plus `po/<lang>.po` (or the layout Slint requires).
  Extract with `slint-tr-extractor` for `.slint` and a script for `tr!` in Rust. Add
  `po/README.md` explaining how to update and add a language.
- **Languages to draft now:** de, es, fr, it, pt_BR, nl, pl, ja, zh_CN. Keep the same plain,
  friendly tone; keep `{}`/`{n}` placeholders, `…`, and product names (OneDrive, SharePoint,
  Dolphin, DriveDeck) unchanged.
- **Desktop files:** add `Name[xx]=`/`Comment[xx]=` lines to `packaging/drivedeck.desktop` and
  both service menu files.
- **Tests:** a unit test that every `.po` has a translation for every msgid in the `.pot`, and
  that placeholders in each translation match the original exactly.
- README: a "Languages" line saying the translations were drafted by machine and corrections
  are welcome (link to `po/README.md`).

**Done when:** `LANGUAGE=de drivedeck` shows the window, popup and tray menu in German with
nothing cut off (check the longest German strings in the settings window and popup), English is
unchanged, and all checks pass.

**Done notes:**

---

## Claude's tasks (Copilot: don't start these)

- K1 Warn when syncing has quietly stalled (service running, no successful sync for hours).
- K2 Detect the inotify watch limit running out; offer a fix with a password prompt.
- K3 Folder sizes in the folder chooser, and a free-space check before the first sync.
- K4 Warn when a git repository is inside the OneDrive folder (pairs with C3).
- K5 Dual-boot check: OneDrive folder on an NTFS drive used by Windows OneDrive.
- K6 "Doctor" page: checks plus a bug-report bundle with private details removed.
- K7 Manage sharing: "Shared by me" list, stop sharing, link expiry/password where allowed.
- K8 OneDrive search in KRunner and the GNOME search.
- K9 btrfs safety snapshot before a full resync or a big-delete decision.
- K10 Big-delete stop: say which folder and how many items, and offer a way through.
- K11 Keep the onedrive client updated, not just installed.
- K12 Flathub: find out whether the background services can work from a Flatpak (report only).
