//! DriveDeck for OneDrive — a tray icon and settings window for the OneDrive Client for Linux.
//!
//!   drivedeck              open the window (and start the tray icon if it isn't running)
//!   drivedeck --tray       tray icon only (used at login)
//!   drivedeck --popup [x y] the panel a click on the tray icon opens (x, y: where)
//!   drivedeck --client …   run the installed onedrive client (kept for units written by
//!                                   older versions, which ran the client through the app)

mod activation;
mod advanced;
mod backup;
mod badges;
mod clientinstall;
mod conflict;
mod core;
mod dbus;
mod doctor;
mod filewin;
mod folderchecks;
mod glass;
mod graph;
mod popup;
mod rename;
mod share;
#[cfg(feature = "test-driver")]
mod testdrive;
mod tray;
mod ui;
mod uninstall;
mod update;

use std::os::unix::process::CommandExt;

/// An AppImage stays mounted until every process holding its runtime's keep-alive pipe has
/// exited, and that pipe isn't close-on-exec. So everything we start (log readers, the tray that
/// replaces us after an update) would inherit it, and keep an old version mounted, with its
/// runtime running, for as long as the new version runs. We keep our own copy; children get none.
/// Nothing we start runs from inside our mount: we relaunch ourselves through the AppImage file.
fn keep_inherited_files_to_ourselves() {
    if std::env::var_os("APPIMAGE").is_none() {
        return;
    }
    // SAFETY: only sets the close-on-exec flag on descriptors >= 3; runs before any other thread
    let r = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if r != 0 {
        // kernels before 5.11
        let fds: Vec<i32> = std::fs::read_dir("/proc/self/fd")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .filter(|&fd| fd > 2)
            .collect();
        for fd in fds {
            // SAFETY: as above
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
}

fn main() {
    keep_inherited_files_to_ourselves();
    #[cfg(feature = "test-driver")]
    testdrive::take_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    core::note_program_file();
    match args.first().map(String::as_str) {
        Some("--client") => run_client(&args[1..]),
        Some("--tray") => {
            // A tray restarting itself after an update waits for the old one to finish leaving.
            let tries = if std::env::var_os(tray::RESTART_ENV).is_some() {
                40
            } else {
                1
            };
            let mut claim = dbus::Claim::TakenByAnother;
            let mut unavailable = 0;
            for _ in 0..tries + 8 {
                claim = dbus::claim_name(core::TRAY_BUS_NAME);
                match claim {
                    dbus::Claim::Owned(_) => break,
                    // the session bus can be slow to accept connections when many programs
                    // start at once (login, or several windows): try again before running
                    // without the one-tray guard
                    dbus::Claim::Unavailable => {
                        unavailable += 1;
                        if unavailable > 8 {
                            break;
                        }
                    }
                    dbus::Claim::TakenByAnother if tries == 1 => break,
                    dbus::Claim::TakenByAnother => {}
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            // held for the life of the process; a second tray finds the name taken and leaves
            let _instance = match claim {
                dbus::Claim::TakenByAnother => return,
                dbus::Claim::Owned(c) => Some(c),
                dbus::Claim::Unavailable => None,
            };
            rename::migrate();
            tray::run()
        }
        Some("--popup") => {
            let at = match (args.get(1), args.get(2)) {
                (Some(x), Some(y)) => x.parse().ok().zip(y.parse().ok()),
                _ => None,
            };
            popup::run(at)
        }
        Some("--share-link") => args
            .get(1)
            .iter()
            .for_each(|p| share::share_link(std::path::Path::new(p), false)),
        Some("--share-edit-link") => args
            .get(1)
            .iter()
            .for_each(|p| share::share_link(std::path::Path::new(p), true)),
        Some("--versions") => args
            .get(1)
            .iter()
            .for_each(|p| filewin::run(filewin::Mode::Versions, std::path::Path::new(p))),
        Some("--share-people") => args
            .get(1)
            .iter()
            .for_each(|p| filewin::run(filewin::Mode::Share, std::path::Path::new(p))),
        Some("--manage-access") => args
            .get(1)
            .iter()
            .for_each(|p| filewin::run(filewin::Mode::Access, std::path::Path::new(p))),
        Some("--view-online") => args
            .get(1)
            .iter()
            .for_each(|p| share::view_online(std::path::Path::new(p))),
        Some("--office-online") => args
            .get(1)
            .iter()
            .for_each(|p| share::view_online(std::path::Path::new(p))),
        Some("--status") => std::process::exit(run_status(args.iter().any(|a| a == "--json"))),
        Some("--print-link") => {
            let Some(path) = args.get(1) else {
                eprintln!("Usage: {} --print-link <file> [--edit]", core::APP_ID);
                std::process::exit(1);
            };
            std::process::exit(share::print_share_link(
                std::path::Path::new(path),
                args.iter().any(|a| a == "--edit"),
            ));
        }
        Some("--sync-now") => std::process::exit(run_sync_now()),
        Some("--version") => println!("{} {}", core::APP_NAME, env!("CARGO_PKG_VERSION")),
        Some("--doctor") if args.get(1).map(String::as_str) == Some("--report") => {
            print!("{}", doctor::report())
        }
        Some("--doctor") => {
            let checks = doctor::checks();
            for c in &checks {
                println!("{}", doctor::line(c));
            }
            if checks.iter().any(|c| !c.ok) {
                std::process::exit(1);
            }
        }
        Some("--help") | Some("-h") => {
            let usage = [
                "[--settings] [--account <config dir>]",
                "--tray",
                "--share-link <file>",
                "--share-edit-link <file>",
                "--share-people <file>",
                "--versions <file>",
                "--manage-access <file>",
                "--view-online <file>",
                "--office-online <file>",
                "--print-link <file> [--edit]",
                "--status [--json]",
                "--sync-now",
                "--doctor [--report]",
            ];
            println!("{} {}\n\nUsage:", core::APP_NAME, env!("CARGO_PKG_VERSION"));
            for u in usage {
                println!("  {} {u}", core::APP_ID);
            }
        }
        _ => {
            // (a window of an older version, handed "account\ntoken", passes it all on here)
            let account = args
                .iter()
                .position(|a| a == "--account")
                .and_then(|i| args.get(i + 1))
                .map(|a| a.split('\n').next().unwrap_or_default().to_string());
            // before anything reads the settings
            rename::migrate();
            ui::run(account)
        }
    }
}

fn run_status(json: bool) -> i32 {
    if core::client_binary().is_none() {
        eprintln!(
            "The OneDrive Client for Linux isn't installed. See {}",
            core::CLIENT_INSTALL_URL
        );
        return 2;
    }

    let snapshot = ui::take_snapshot();
    if json {
        match status_json(&snapshot.infos, &snapshot.statuses, &snapshot.settings) {
            Ok(output) => println!("{output}"),
            Err(e) => {
                eprintln!("Couldn't format account status: {e}");
                return 1;
            }
        }
    } else {
        for (info, status) in snapshot.infos.iter().zip(&snapshot.statuses) {
            let prefs = snapshot.settings.prefs(&info.confdir);
            println!("{}: {}", info.name, status.text(&prefs));
        }
    }
    status_exit_code(true, &snapshot.statuses)
}

fn status_json(
    infos: &[core::AccountInfo],
    statuses: &[core::Status],
    settings: &core::AppSettings,
) -> Result<String, serde_json::Error> {
    let accounts: Vec<_> = infos
        .iter()
        .zip(statuses)
        .map(|(info, status)| {
            let prefs = settings.prefs(&info.confdir);
            serde_json::json!({
                "account": info.name,
                "folder": info.sync_dir.display().to_string(),
                "phase": format!("{:?}", status.phase).to_lowercase(),
                "text": status.text(&prefs),
                "last_complete": status.last_complete.as_ref().map(|time| time.to_rfc3339()),
                "last_error": status.last_error.as_deref(),
                "not_synced": status.unsynced_changes(),
            })
        })
        .collect();
    serde_json::to_string(&accounts)
}

fn status_exit_code(client_installed: bool, statuses: &[core::Status]) -> i32 {
    if !client_installed {
        return 2;
    }
    if statuses.iter().any(|status| {
        matches!(
            status.phase,
            core::Phase::Error
                | core::Phase::Stopped
                | core::Phase::NeedsResync
                | core::Phase::SignedOut
                | core::Phase::BigDelete
                | core::Phase::Stalled
        ) || status.unsynced_changes() > 0
    }) {
        1
    } else {
        0
    }
}

fn run_sync_now() -> i32 {
    if core::client_binary().is_none() {
        eprintln!(
            "The OneDrive Client for Linux isn't installed. See {}",
            core::CLIENT_INSTALL_URL
        );
        return 2;
    }
    let snapshot = ui::take_snapshot();
    let mut started = 0;
    let mut errors = Vec::new();
    for (info, status) in snapshot.infos.iter().zip(&snapshot.statuses) {
        if !info.signed_in || status.phase == core::Phase::Resyncing {
            continue;
        }
        match core::sync_now(info) {
            Ok(_) => {
                println!("Requested a sync for {}.", info.name);
                started += 1;
            }
            Err(e) => errors.push(format!("{}: {e}", info.name)),
        }
    }
    if started == 0 && errors.is_empty() {
        println!("No accounts are ready to sync.");
    }
    for error in &errors {
        eprintln!("{error}");
    }
    if errors.is_empty() { 0 } else { 1 }
}

fn run_client(args: &[String]) {
    let Some(bin) = core::client_binary() else {
        eprintln!(
            "The OneDrive Client for Linux isn't installed. See {}",
            core::CLIENT_INSTALL_URL
        );
        std::process::exit(127);
    };
    let err = std::process::Command::new(bin).args(args).exec();
    eprintln!("Couldn't start the onedrive client: {err}");
    std::process::exit(127);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    fn account(name: &str) -> core::AccountInfo {
        core::AccountInfo {
            name: name.into(),
            confdir: "/tmp/drivedeck-test-config".into(),
            unit: Some("onedrive.service".into()),
            managed: true,
            sync_dir: "/home/test/OneDrive".into(),
            drive_id: None,
            kind: "personal".into(),
            signed_in: true,
        }
    }

    fn status(phase: core::Phase) -> core::Status {
        core::Status {
            phase,
            last_complete: None,
            stalled_since: None,
            last_error: None,
            blocked: Vec::new(),
            big_delete: None,
            unit: core::UnitState::default(),
        }
    }

    #[test]
    fn status_json_has_the_documented_fields_and_values() {
        let personal = account("Personal");
        let completed = Local.with_ymd_and_hms(2025, 6, 12, 9, 30, 0).unwrap();
        let mut current = status(core::Phase::UpToDate);
        current.last_complete = Some(completed);
        current.last_error = Some("Last run failed".into());
        current.blocked.push(core::Blocked {
            path: "Documents/report.docx".into(),
            reason: core::BlockReason::ReadOnly,
        });
        let signed_out = status(core::Phase::SignedOut);
        let settings = core::AppSettings::default();
        let expected_text = current.text(&settings.prefs(&personal.confdir));

        let value: serde_json::Value = serde_json::from_str(
            &status_json(
                &[personal, account("Work")],
                &[current, signed_out],
                &settings,
            )
            .unwrap(),
        )
        .unwrap();
        let accounts = value.as_array().unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            accounts[0]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            [
                "account",
                "folder",
                "last_complete",
                "last_error",
                "not_synced",
                "phase",
                "text",
            ]
        );
        assert_eq!(accounts[0]["account"], "Personal");
        assert_eq!(accounts[0]["folder"], "/home/test/OneDrive");
        assert_eq!(accounts[0]["phase"], "uptodate");
        assert_eq!(accounts[0]["text"], expected_text);
        assert_eq!(accounts[0]["last_complete"], completed.to_rfc3339());
        assert_eq!(accounts[0]["last_error"], "Last run failed");
        assert_eq!(accounts[0]["not_synced"], 1);
        assert_eq!(accounts[1]["phase"], "signedout");
        assert!(accounts[1]["last_complete"].is_null());
        assert!(accounts[1]["last_error"].is_null());
    }

    #[test]
    fn status_exit_code_reports_attention_and_missing_client() {
        assert_eq!(status_exit_code(true, &[status(core::Phase::UpToDate)]), 0);
        assert_eq!(status_exit_code(true, &[status(core::Phase::Error)]), 1);
        assert_eq!(status_exit_code(true, &[status(core::Phase::Stopped)]), 1);
        assert_eq!(status_exit_code(true, &[status(core::Phase::Stalled)]), 1);
        assert_eq!(
            status_exit_code(true, &[status(core::Phase::NeedsResync)]),
            1
        );
        assert_eq!(status_exit_code(false, &[status(core::Phase::UpToDate)]), 2);

        let mut unsynced = status(core::Phase::UpToDate);
        unsynced.blocked.push(core::Blocked {
            path: "report.docx".into(),
            reason: core::BlockReason::Locked,
        });
        assert_eq!(status_exit_code(true, &[unsynced]), 1);
    }
}
