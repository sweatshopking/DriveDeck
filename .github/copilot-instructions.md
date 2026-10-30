# Instructions for Copilot working on DriveDeck

DriveDeck for OneDrive is a Rust + Slint desktop app (tray icon, popup panel, settings window)
that manages the `onedrive` sync client (abraunegg/onedrive) through systemd user services.
The work list is `docs/plan-1.1.md`. Only do the tasks marked **Copilot** there, one task per
session, in the order given, unless the user says otherwise.

## Hard rules (breaking any of these is a bug)

1. **Never change another program's settings on your own.** Detect and report; change only when
   the user clicks something that clearly says what will change. That covers the onedrive
   client's `config` files, systemd units the app didn't create, desktop settings, and other
   apps' config files.
2. **No blocking D-Bus calls (anything in `src/dbus.rs`, `share::notify`, badge signals) inside
   ksni code**: `Tray` trait methods, menu callbacks or `Handle::update` closures in
   `src/tray.rs`. They run inside a tokio runtime and panic. Do that work on a
   `std::thread::spawn` thread.
3. **Never overwrite or delete a user's file.** Use `core::atomic_write` for files the app
   owns; anything else is renamed or copied, never replaced.
4. **Plain words in the UI.** Short sentences, no jargon ("full resync" is explained where it
   appears), no emoji. Match the tone of the existing strings in `ui/app.slint`.
5. **Don't add dependencies** unless the task says so. Every new crate must be GPL-3.0-compatible
   and get a line in `packaging/THIRD-PARTY.md`.
6. **Don't touch release files**: version numbers, `.github/workflows/`, `packaging/build*.sh`,
   the metainfo `<releases>` list, and `vendor/`, unless the task says so.
7. **Keep the code style**: `cargo fmt`, comments only where the code doesn't explain itself, and
   short doc comments like the ones already there.

## Before you say a task is done

Run all of these, and fix everything they report:

```
cargo fmt --check
cargo clippy --locked --all-targets --features test-driver -- -D warnings
cargo test --locked
```

Add unit tests for any new logic (parsing, decisions, string building) in the same file's
`#[cfg(test)] mod tests`. Then write a short summary at the end of the task's section in
`docs/plan-1.1.md` under **Done notes**: what changed, which files, anything left unsure.

## Map of the code

- `src/main.rs`: command-line entry (`--tray`, `--popup`, `--share-link`, …).
- `src/core.rs`: settings (`AppSettings`, `AccountPrefs`), accounts, services, status.
- `src/ui.rs` + `ui/app.slint`: the settings window. `src/popup.rs`: the tray panel.
- `src/tray.rs`: tray icon (ksni, see rule 2). `src/graph.rs`: Microsoft Graph calls.
- `src/share.rs`: Dolphin right-click actions; `packaging/drivedeck-servicemenu.desktop`.
- `src/filewin.rs`: small windows for version history and sharing.
- `plugins/`: Dolphin (C++), Nemo/Caja (Python) badge plugins.
