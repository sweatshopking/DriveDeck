# Could DriveDeck be on Flathub?

Short answer: not as DriveDeck works today. A Flatpak version would need a different design.
Checked 2026-10-30.

## What DriveDeck relies on that a Flatpak sandbox blocks

| DriveDeck needs | In a Flatpak |
| --- | --- |
| Runs the `onedrive` client installed on the system | The sandbox can't see host programs. Reaching them needs `flatpak-spawn --host` (`--talk-name=org.freedesktop.Flatpak`), a full sandbox escape that Flathub reviewers normally refuse for an app like this. |
| One systemd user service per account (`systemctl --user`, unit files in `~/.config/systemd/user`) | Needs host filesystem access to that folder and the systemd D-Bus API; both are effectively unsandboxed. |
| Reads the client's log with `journalctl --user -u …` | No journal access from inside the sandbox. |
| Installs and updates the client with `pkexec` | No pkexec in the sandbox. |
| Dolphin badge plugin in the system Qt plugin folder, Nemo/Caja extensions | Can't write system folders; file-manager plugins can't be shipped in a Flatpak. |
| Raises the folder-watch limit (`/etc/sysctl.d`) | Not possible. |

## What a Flatpak edition would have to be

- Bundle the onedrive client (it's D, built with LDC) inside the Flatpak instead of using the
  system's.
- Run each account's client as a child of the tray process, started at login through the
  Background portal, instead of systemd services; read its output directly instead of the
  journal.
- Drop the client installer, Dolphin/Nemo/Caja badges and the watch-limit fix in that edition.
- Ask for access only to the chosen sync folders (filesystem permissions per folder).

That is a second backend for most of `core.rs`, `tray.rs` and the status code. It's possible,
but large, and the two editions would behave differently.

## Better ways to be found today

- AUR: the `drivedeck-bin` package is ready locally; it needs an AUR account to publish.
- Fedora COPR and openSUSE OBS repositories for the rpm, and a Launchpad PPA or OBS for the deb,
  so updates come with system updates.
- AppImageHub / AppImage catalogue listing for the AppImage.
- GitHub topics are already set; a short demo GIF in the README helps search results.
