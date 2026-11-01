//! "Check for problems": every check the app knows, in one list, and a report to attach to a
//! bug with personal details taken out (home folder, user and computer names, email addresses,
//! long ids and tokens, file names). Read only.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::core::{self, Phase};

/// One finding: fine or worth a look
pub struct Check {
    pub ok: bool,
    pub text: String,
}

impl Check {
    fn ok(text: impl Into<String>) -> Self {
        Check {
            ok: true,
            text: text.into(),
        }
    }
    fn warn(text: impl Into<String>) -> Self {
        Check {
            ok: false,
            text: text.into(),
        }
    }
}

pub fn line(c: &Check) -> String {
    format!("{} {}", if c.ok { "✓" } else { "⚠" }, c.text)
}

/// Free space below this on an account's drive is worth a mention
const LOW_SPACE: u64 = 2 << 30;

pub fn checks() -> Vec<Check> {
    checks_named(true)
}

/// As `checks`; `names` false leaves out names of folders inside OneDrive (for a report)
fn checks_named(names: bool) -> Vec<Check> {
    let mut out = Vec::new();
    match core::check_client() {
        core::ClientCheck::Ok { version, .. } => match crate::update::client_news() {
            Some((_, latest)) => out.push(Check::warn(crate::tr!(
                "OneDrive Client for Linux {} is installed; {} is out.",
                version,
                latest
            ))),
            None => out.push(Check::ok(crate::tr!(
                "OneDrive Client for Linux {} is installed.",
                version
            ))),
        },
        core::ClientCheck::TooOld { version, .. } => out.push(Check::warn(crate::tr!(
            "OneDrive Client for Linux {} is too old for this app.",
            version
        ))),
        core::ClientCheck::Missing => out.push(Check::warn(crate::tr!(
            "The OneDrive Client for Linux isn't installed."
        ))),
    }
    if crate::dbus::name_owner_pid(core::TRAY_BUS_NAME).is_some() {
        out.push(Check::ok(crate::tr!("The tray icon is running.")));
    } else {
        out.push(Check::warn(crate::tr!(
            "The tray icon isn't running, so pauses, schedules and notifications aren't working."
        )));
    }
    let snap = crate::ui::take_snapshot();
    if snap.infos.is_empty() {
        out.push(Check::warn(crate::tr!("No accounts are set up yet.")));
    }
    let mut dirs = 0usize;
    for (info, status) in snap.infos.iter().zip(&snap.statuses) {
        let prefs = snap.settings.prefs(&info.confdir);
        let text = crate::tr!("{}: {}", info.name, status.text(&prefs));
        let fine = matches!(
            status.phase,
            Phase::UpToDate | Phase::Syncing | Phase::Paused | Phase::Off
        ) && status.unsynced_changes() == 0;
        out.push(if fine {
            Check::ok(text)
        } else {
            Check::warn(text)
        });
        if let Some(e) = &status.last_error {
            out.push(Check::warn(crate::tr!("{}: {}", info.name, e)));
        }
        if let Some(u) = &info.unit
            && core::watch_limit_hit(u)
        {
            out.push(Check::warn(crate::tr!(
                "{}: ran out of folder watches; some changes aren't noticed.",
                info.name
            )));
        }
        // a service somebody else wrote: after the client says "needs a full resync" (exit 78),
        // systemd must not keep restarting it. Ours say so; this is only reported, never edited.
        if let Some(u) = &info.unit
            && !info.managed
        {
            let value = core::unit_props(u, "RestartPreventExitStatus")
                .remove("RestartPreventExitStatus")
                .unwrap_or_default();
            if !exit_status_listed(&value, 78) {
                out.push(Check::warn(crate::tr!(
                    "{}: its service file (set up outside this app) doesn't list exit status 78 in RestartPreventExitStatus, so after the client asks for a full resync it is started again and again. Add \"RestartPreventExitStatus=78\" to that file, or let this app manage the service.",
                    info.name
                )));
            }
        }
        match core::free_space(&info.sync_dir) {
            Some(free) if free < LOW_SPACE => out.push(Check::warn(crate::tr!(
                "{}: only {} free on its drive.",
                info.name,
                crate::graph::human_size(free)
            ))),
            _ => {}
        }
        let cfg = core::ClientConfig::load(&info.confdir);
        let skip_dirs = cfg.get_joined("skip_dir").unwrap_or_default();
        let dotfiles = cfg.get_bool("skip_dotfiles");
        for note in crate::folderchecks::notes_named(&info.sync_dir, &skip_dirs, dotfiles, names) {
            out.push(Check::warn(format!("{}: {note}", info.name)));
        }
        dirs += count_dirs(&info.sync_dir, 2_000_000);
    }
    // every program you run shares the watch limit; the client needs about one per folder
    if let Some(limit) = core::watch_limit()
        && dirs as u64 > limit / 2
    {
        out.push(Check::warn(crate::tr!(
            "Your OneDrive folders hold {} folders, more than half of Linux's limit on watched folders ({}). Other programs need some too.",
            dirs,
            limit
        )));
    }
    for o in core::outside_starts() {
        out.push(Check::warn(crate::tr!(
            "{} is started outside this app.",
            o.unit
        )));
    }
    out
}

/// Whether a RestartPreventExitStatus value ("78 126", "SIGTERM 78") names this exit status
fn exit_status_listed(value: &str, code: u32) -> bool {
    value
        .split_whitespace()
        .any(|word| word.parse::<u32>().ok() == Some(code))
}

/// Folders under `root`, counting up to `max`
fn count_dirs(root: &Path, max: usize) -> usize {
    let mut todo = vec![root.to_path_buf()];
    let mut n = 0;
    while let Some(dir) = todo.pop() {
        n += 1;
        if n >= max {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        todo.extend(
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path()),
        );
    }
    n
}

/// Client settings whose values are safe to show as they are; any other set key shows as "set"
const PLAIN_KEYS: &[&str] = &[
    "monitor_interval",
    "monitor_fullscan_frequency",
    "rate_limit",
    "threads",
    "classify_as_big_delete",
    "skip_dotfiles",
    "skip_symlinks",
    "download_only",
    "upload_only",
    "local_first",
    "no_remote_delete",
    "cleanup_local_files",
    "sync_business_shared_items",
    "check_nosync",
    "dry_run",
    "force_http_11",
    "ip_protocol_version",
    "use_recycle_bin",
    "disable_notifications",
    "remove_source_files",
    "skip_size",
];

/// What to take out of report text
struct Private {
    /// Longest first, so a folder inside home is replaced before home itself
    paths: Vec<(String, &'static str)>,
    words: Vec<(String, String)>,
}

impl Private {
    fn of(infos: &[core::AccountInfo]) -> Self {
        let mut paths: Vec<(String, &'static str)> = infos
            .iter()
            .flat_map(|i| {
                [
                    (
                        i.sync_dir.to_string_lossy().into_owned(),
                        "<OneDrive folder>",
                    ),
                    (i.confdir.to_string_lossy().into_owned(), "<config folder>"),
                ]
            })
            .collect();
        paths.push((core::home().to_string_lossy().into_owned(), "~"));
        paths.retain(|(p, _)| p.len() > 1);
        paths.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        // account names are often an organisation's, and services are named after them
        let mut words: Vec<(String, String)> = Vec::new();
        for (n, i) in infos.iter().enumerate() {
            words.push((i.name.clone(), format!("Account {}", n + 1)));
            if let Some(u) = &i.unit
                && u != "onedrive.service"
            {
                words.push((u.clone(), format!("<service {}>", n + 1)));
            }
        }
        if let Ok(user) = std::env::var("USER")
            && user.len() > 2
        {
            words.push((user, "<user>".into()));
        }
        if let Ok(host) = std::fs::read_to_string("/etc/hostname")
            && host.trim().len() > 2
        {
            words.push((host.trim().to_string(), "<computer>".into()));
        }
        // longest first, so "LSA business account" goes before a user named "LSA"
        words.sort_by_key(|(w, _)| std::cmp::Reverse(w.len()));
        Private { paths, words }
    }

    fn scrub(&self, text: &str) -> String {
        // file names first: the path substitutions below would hide the ": /" they key on
        let mut s = scrub_names(text);
        for (p, with) in &self.paths {
            s = s.replace(p.as_str(), with);
        }
        for (w, with) in &self.words {
            s = replace_word(&s, w, with);
        }
        scrub_tokens(&scrub_emails(&s))
    }
}

/// `word` only where it stands alone (not inside a longer word)
fn replace_word(s: &str, word: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find(word) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + word.len()..].chars().next();
        let alone = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..i]);
        out.push_str(if alone(before) && alone(after) {
            with
        } else {
            word
        });
        rest = &rest[i + word.len()..];
    }
    out.push_str(rest);
    out
}

fn scrub_emails(s: &str) -> String {
    let edge = |c: char| c.is_whitespace() || "<>()[]\"',;:".contains(c);
    s.split_inclusive(edge)
        .map(|w| {
            let word = w.trim_end_matches(|c: char| edge(c) || c == '.');
            let end = &w[word.len()..];
            match word.split_once('@') {
                Some((a, b)) if !a.is_empty() && b.contains('.') => format!("<email>{end}"),
                _ => w.to_string(),
            }
        })
        .collect()
}

/// Runs of 24 or more id-like characters (drive and item ids, tokens, long hashes)
fn scrub_tokens(s: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let id_char = |c: char| c.is_ascii_alphanumeric() || "-_+/=!".contains(c);
    for c in s.chars().chain(std::iter::once('\n')) {
        if id_char(c) {
            run.push(c);
            continue;
        }
        let long_id = run.len() >= 24 && run.chars().any(|c| c.is_ascii_digit());
        out.push_str(if long_id { "<id>" } else { &run });
        run.clear();
        out.push(c);
    }
    out.pop();
    out
}

/// What the client says about a file ends with its name: keep the message, not the name
fn scrub_names(s: &str) -> String {
    s.lines()
        .map(|l| match l.find(": ./").or_else(|| l.find(": /")) {
            Some(i) => format!("{}: <file>", &l[..i]),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn report() -> String {
    let snap = crate::ui::take_snapshot();
    let private = Private::of(&snap.infos);
    let os = crate::clientinstall::os_release();
    // written as is: nothing personal in it, and a computer named after its distribution must
    // not turn the distribution into "<computer>"
    let mut head = String::new();
    let _ = writeln!(
        head,
        "DriveDeck problem report, {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );
    let _ = writeln!(
        head,
        "App: {} {}",
        core::APP_NAME,
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(
        head,
        "System: {}; desktop: {}; session: {}",
        os.get("PRETTY_NAME")
            .map(String::as_str)
            .unwrap_or("unknown"),
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        std::env::var("XDG_SESSION_TYPE").unwrap_or_default()
    );
    let mut r = String::new();
    let _ = writeln!(r, "\n== Checks");
    for c in checks_named(false) {
        let _ = writeln!(r, "{}", line(&c));
    }
    for (info, status) in snap.infos.iter().zip(&snap.statuses) {
        let _ = writeln!(
            r,
            "\n== Account ({}, {})",
            info.kind,
            if info.managed {
                "set up here"
            } else {
                "set up outside"
            }
        );
        let _ = writeln!(
            r,
            "Phase: {:?}; service: {}",
            status.phase,
            info.unit.as_deref().unwrap_or("none")
        );
        let cfg = core::ClientConfig::load(&info.confdir);
        for key in PLAIN_KEYS {
            if let Some(v) = cfg.get(key) {
                let _ = writeln!(r, "{key} = {v}");
            }
        }
        for key in ["skip_dir", "skip_file", "sync_list"] {
            if cfg.get(key).is_some() {
                let _ = writeln!(r, "{key} = (set)");
            }
        }
        if let Some(u) = &info.unit {
            let _ = writeln!(r, "-- recent status lines");
            for l in core::journal_status_lines(u).iter().rev().take(40).rev() {
                let _ = writeln!(r, "{l}");
            }
        }
    }
    head + &private.scrub(&r)
}

/// Writes the report to a new file in the home folder (only you can read it) and returns it.
pub fn save_report() -> Result<PathBuf, String> {
    let text = report();
    let file = core::home().join(format!(
        "drivedeck-report-{}.txt",
        chrono::Local::now().format("%Y-%m-%d-%H%M%S")
    ));
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .map_err(|e| format!("Couldn't write {}: {e}", file.display()))?;
    f.write_all(text.as_bytes())
        .map_err(|e| format!("Couldn't write {}: {e}", file.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_after_an_absolute_path_are_left_out() {
        let p = Private {
            paths: vec![
                ("/home/ann/OneDrive".into(), "<OneDrive folder>"),
                ("/home/ann".into(), "~"),
            ],
            words: vec![],
        };
        let s = p.scrub(
            "ERROR: Unable to read: /home/ann/OneDrive/Taxes/2025 return.pdf\n\
             ERROR: Unable to read: /home/ann/Documents/letter.docx",
        );
        assert!(!s.contains("Taxes"), "{s}");
        assert!(!s.contains("letter"), "{s}");
        assert!(!s.contains("2025"), "{s}");
    }

    #[test]
    fn reports_leave_out_personal_details() {
        let p = Private {
            paths: vec![
                ("/home/ann/OneDrive".into(), "<OneDrive folder>"),
                ("/home/ann".into(), "~"),
            ],
            words: vec![
                ("Smith Family".into(), "Account 1".into()),
                ("annpc".into(), "<computer>".into()),
                ("ann".into(), "<user>".into()),
            ],
        };
        let s = p.scrub(
            "ERROR: Unable to upload: ./Taxes/2025 return.pdf\n\
             Sync dir /home/ann/OneDrive, config /home/ann/.config/onedrive\n\
             Signed in as ann.lee@example.com (drive b!Xy7_abc123DEF456ghi789JKL012mno)\n\
             Smith Family: Up to date\n\
             Sync with Microsoft OneDrive is complete",
        );
        assert!(s.contains("Account 1: Up to date"), "{s}");
        assert!(!s.contains("Taxes"), "{s}");
        assert!(!s.contains("example.com"), "{s}");
        assert!(!s.contains("Xy7_abc123"), "{s}");
        assert!(!s.contains("/home/ann"), "{s}");
        assert!(s.contains("ERROR: Unable to upload: <file>"), "{s}");
        assert!(
            s.contains("Sync dir <OneDrive folder>, config ~/.config/onedrive"),
            "{s}"
        );
        assert!(
            s.contains("Sync with Microsoft OneDrive is complete"),
            "{s}"
        );
    }

    #[test]
    fn names_are_replaced_only_as_whole_words() {
        assert_eq!(
            replace_word("ann planning ann.", "ann", "<user>"),
            "<user> planning <user>."
        );
    }

    #[test]
    fn short_words_and_versions_stay() {
        assert_eq!(
            scrub_tokens("onedrive v2.5.12 monitor_interval = 300"),
            "onedrive v2.5.12 monitor_interval = 300"
        );
        assert_eq!(scrub_emails("a@b and x@y.org."), "a@b and <email>.");
    }

    #[test]
    fn restart_prevention_is_read_from_the_units_value() {
        assert!(exit_status_listed("78 126", 78));
        assert!(exit_status_listed("126 78", 78));
        assert!(exit_status_listed("78", 78));
        assert!(!exit_status_listed("", 78));
        assert!(!exit_status_listed("126", 78));
        assert!(!exit_status_listed("780", 78));
    }
}
