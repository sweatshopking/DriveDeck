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
mod gnomesearch;
mod graph;
mod i18n;
mod popup;
mod rename;
mod runner;
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
        Some("--status") => std::process::exit(run_status(
            args.iter().any(|a| a == "--json"),
            flag_value(&args, "--files")
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
                .min(RECENT_FILES_MAX),
        )),
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
        Some("--sync-now") => std::process::exit(run_sync_now(flag_value(&args, "--account"))),
        Some("--pause") => std::process::exit(run_pause(
            args.get(1).map(String::as_str),
            flag_value(&args, "--account"),
        )),
        Some("--update") => std::process::exit(run_update()),
        Some("--gnome-search-test") => gnomesearch::test_service(),
        Some("--version") => println!("{} {}", core::APP_NAME, env!("CARGO_PKG_VERSION")),
        Some("--doctor") if args.get(1).map(String::as_str) == Some("--report") => {
            print!("{}", doctor::report())
        }
        Some("--report-bug") => print!("{}", doctor::bug_report_body("")),
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
                "--status [--json [--files <n>]]",
                "--sync-now [--account <account>]",
                "--pause <minutes|morning|resume> [--account <account>]",
                "--update",
                "--doctor [--report]",
                "--report-bug",
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

/// Recent files `--status --json --files <n>` lists at most, per account
const RECENT_FILES_MAX: usize = 200;

/// The value after `flag` on the command line, as in `--account <config dir>`
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The accounts `--account` picks (its config folder, as `key` in `--status --json`, or its name;
/// no `--account` = all of them), or an error naming the ones there are.
fn pick_accounts<'a>(
    infos: &'a [core::AccountInfo],
    statuses: &'a [core::Status],
    account: Option<&str>,
) -> Result<Vec<(&'a core::AccountInfo, &'a core::Status)>, String> {
    let all = infos.iter().zip(statuses);
    let Some(want) = account else {
        return Ok(all.collect());
    };
    let picked: Vec<_> = all
        .filter(|(i, _)| i.key() == want || i.name == want)
        .collect();
    if picked.is_empty() {
        let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        return Err(format!(
            "No account called \"{want}\". Accounts: {}",
            names.join(", ")
        ));
    }
    Ok(picked)
}

fn run_status(json: bool, files: usize) -> i32 {
    if core::client_binary().is_none() {
        eprintln!(
            "The OneDrive Client for Linux isn't installed. See {}",
            core::CLIENT_INSTALL_URL
        );
        return 2;
    }

    let snapshot = ui::take_snapshot();
    if json {
        // recent files and transfers come from the client's log: only read when asked for
        let activity: Option<Vec<_>> = (files > 0).then(|| {
            snapshot
                .infos
                .iter()
                .map(|i| match &i.unit {
                    Some(u) => core::account_files(u, files),
                    None => (Vec::new(), Vec::new()),
                })
                .collect()
        });
        match status_json(
            &snapshot.infos,
            &snapshot.statuses,
            &snapshot.settings,
            activity.as_deref(),
        ) {
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

/// What a recent-file symbol from the log means, in a word scripts can match on
fn activity_word(kind: &str) -> &'static str {
    match kind {
        "↑" => "uploaded",
        "↓" => "downloaded",
        "✕" => "deleted",
        "+" => "created",
        _ => "changed",
    }
}

/// `--status --json`. The first seven fields are the original set; the rest were added for
/// status bars and desktop shells (Glaze) and only ever grow. `files` (when given, per account as
/// in `infos`) adds "transfers" and "recent".
fn status_json(
    infos: &[core::AccountInfo],
    statuses: &[core::Status],
    settings: &core::AppSettings,
    files: Option<&[(Vec<core::FileActivity>, Vec<core::Transfer>)]>,
) -> Result<String, serde_json::Error> {
    let accounts: Vec<_> = infos
        .iter()
        .zip(statuses)
        .enumerate()
        .map(|(n, (info, status))| {
            let prefs = settings.prefs(&info.confdir);
            let mut v = serde_json::json!({
                "account": info.name,
                "folder": info.sync_dir.display().to_string(),
                "phase": format!("{:?}", status.phase).to_lowercase(),
                "text": status.text(&prefs),
                "last_complete": status.last_complete.as_ref().map(|time| time.to_rfc3339()),
                "last_error": status.last_error.as_deref(),
                "not_synced": status.unsynced_changes(),
                // added in 1.7
                "key": info.key(),
                "kind": info.kind,
                "kind_label": info.kind_label(),
                "signed_in": info.signed_in,
                "background": status.unit.enabled,
                "paused_until": prefs
                    .paused()
                    .then(|| chrono::TimeZone::timestamp_opt(&chrono::Local, prefs.pause_until, 0).single())
                    .flatten()
                    .map(|t| t.to_rfc3339()),
            });
            if let Some((recent, transfers)) = files.and_then(|f| f.get(n)) {
                v["transfers"] = transfers
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "direction": if t.up { "up" } else { "down" },
                            "path": t.path,
                            "percent": t.percent,
                            "eta": t.eta,
                        })
                    })
                    .collect();
                v["recent"] = recent
                    .iter()
                    .map(|f| {
                        serde_json::json!({
                            "when": f.when.to_rfc3339(),
                            "action": activity_word(f.kind),
                            "path": f.path,
                        })
                    })
                    .collect();
            }
            v
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

/// `--update`: installs the newest release the way the window's "Update now" does
fn run_update() -> i32 {
    use update::Outcome;
    match update::install_latest() {
        Outcome::Updated(v) => {
            println!("{}", tr!("Updated to version {}.", v));
            0
        }
        Outcome::UpToDate => {
            println!("{}", tr!("This is the newest version."));
            0
        }
        Outcome::OpenedPage => {
            println!(
                "{}",
                tr!(
                    "This copy can't update itself; the download page is open: {}",
                    update::RELEASES_URL
                )
            );
            1
        }
        Outcome::Cancelled => 1,
        Outcome::Failed(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run_sync_now(account: Option<&str>) -> i32 {
    if core::client_binary().is_none() {
        eprintln!(
            "The OneDrive Client for Linux isn't installed. See {}",
            core::CLIENT_INSTALL_URL
        );
        return 2;
    }
    let snapshot = ui::take_snapshot();
    let picked = match pick_accounts(&snapshot.infos, &snapshot.statuses, account) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let mut started = 0;
    let mut errors = Vec::new();
    for (info, status) in picked {
        if !info.signed_in || status.phase == core::Phase::Resyncing {
            // one asked for by name: say why nothing happened, and fail (scripts check the exit status)
            if account.is_some() {
                errors.push(if info.signed_in {
                    format!(
                        "{}: a full resync is running; try again when it has finished",
                        info.name
                    )
                } else {
                    format!("{}: not signed in", info.name)
                });
            }
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
    // systemctl runs on a background thread: let it finish before this process ends
    core::wait_for_unit_actions(std::time::Duration::from_secs(90));
    if errors.is_empty() { 0 } else { 1 }
}

/// `--pause <minutes|morning|resume> [--account <account>]`: what the panel's pause buttons do.
/// Without `--account`, every signed-in account that syncs in the background, as in the panel.
fn run_pause(how: Option<&str>, account: Option<&str>) -> i32 {
    let secs: i64 = match how {
        Some("resume") => 0,
        Some("morning") => -1,
        Some(m) => match m.parse::<i64>() {
            Ok(m) if (1..=60 * 24 * 7).contains(&m) => m * 60,
            _ => {
                eprintln!(
                    "--pause takes a number of minutes (1 to 10080), \"morning\" or \"resume\""
                );
                return 2;
            }
        },
        None => {
            eprintln!("--pause takes a number of minutes, \"morning\" or \"resume\"");
            return 2;
        }
    };
    let snapshot = ui::take_snapshot();
    let picked = match pick_accounts(&snapshot.infos, &snapshot.statuses, account) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let mut done = 0;
    let mut errors = Vec::new();
    for (info, status) in picked {
        // a named account is paused even if its background sync is off (it may be turned on
        // later); "all" means the ones syncing in the background, as in the panel
        if !info.signed_in {
            // one asked for by name: say why nothing happened, and fail (scripts check the exit status)
            if account.is_some() {
                errors.push(format!("{}: not signed in", info.name));
            }
            continue;
        }
        if account.is_none() && !status.unit.enabled {
            continue;
        }
        match core::pause(info, secs) {
            Ok(_) => {
                println!(
                    "{} {}.",
                    if secs == 0 { "Resumed" } else { "Paused" },
                    info.name
                );
                done += 1;
            }
            Err(e) => errors.push(format!("{}: {e}", info.name)),
        }
    }
    if done == 0 && errors.is_empty() {
        println!("No accounts to change.");
    }
    for error in &errors {
        eprintln!("{error}");
    }
    // systemctl runs on a background thread: let it finish before this process ends
    core::wait_for_unit_actions(std::time::Duration::from_secs(90));
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
                None,
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
                "background",
                "folder",
                "key",
                "kind",
                "kind_label",
                "last_complete",
                "last_error",
                "not_synced",
                "paused_until",
                "phase",
                "signed_in",
                "text",
            ]
        );
        assert_eq!(accounts[0]["key"], "/tmp/drivedeck-test-config");
        assert!(accounts[0]["paused_until"].is_null());
        assert!(accounts[0].get("recent").is_none());
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
    fn status_json_files_add_transfers_and_recent() {
        let when = Local.with_ymd_and_hms(2025, 6, 12, 9, 31, 0).unwrap();
        let files = vec![(
            vec![
                core::FileActivity {
                    when,
                    kind: "↑",
                    path: "Documents/a.docx".into(),
                },
                core::FileActivity {
                    when,
                    kind: "+",
                    path: "Pictures/2026".into(),
                },
            ],
            vec![core::Transfer {
                up: false,
                path: "Videos/big.mp4".into(),
                percent: 42,
                eta: Some("2 min".into()),
                when,
            }],
        )];
        let v: serde_json::Value = serde_json::from_str(
            &status_json(
                &[account("Personal")],
                &[status(core::Phase::Syncing)],
                &core::AppSettings::default(),
                Some(&files),
            )
            .unwrap(),
        )
        .unwrap();
        let a = &v[0];
        assert_eq!(a["transfers"][0]["direction"], "down");
        assert_eq!(a["transfers"][0]["percent"], 42);
        assert_eq!(a["transfers"][0]["eta"], "2 min");
        assert_eq!(a["recent"][0]["action"], "uploaded");
        assert_eq!(a["recent"][1]["action"], "created");
        assert_eq!(a["recent"][1]["path"], "Pictures/2026");
    }

    #[test]
    fn accounts_are_picked_by_config_folder_or_name() {
        let mut work = account("Work");
        work.confdir = "/tmp/drivedeck-test-work".into();
        let infos = [account("Personal"), work];
        let sts = [status(core::Phase::UpToDate), status(core::Phase::UpToDate)];
        assert_eq!(pick_accounts(&infos, &sts, None).unwrap().len(), 2);
        assert_eq!(
            pick_accounts(&infos, &sts, Some("Work")).unwrap()[0].0.name,
            "Work"
        );
        assert_eq!(
            pick_accounts(&infos, &sts, Some("/tmp/drivedeck-test-work")).unwrap()[0]
                .0
                .name,
            "Work"
        );
        let err = pick_accounts(&infos, &sts, Some("Nope")).unwrap_err();
        assert!(err.contains("Personal, Work"), "{err}");
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
