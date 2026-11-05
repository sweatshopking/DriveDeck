//! System tray icon: live status per account, the panel a click opens (src/popup.rs), a short
//! menu, and the sync schedule enforcer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem, SubMenu};

use crate::core::{
    self, AccountInfo, AccountPrefs, AppSettings, LogEvent, Phase, Status, UnitState,
};
use crate::share;

struct Acc {
    info: AccountInfo,
    prefs: AccountPrefs,
    unit: UnitState,
    resyncing: bool,
    log_phase: Phase,
    last_complete: Option<DateTime<Local>>,
    last_error: Option<String>,
    blocked: Vec<core::Blocked>,
    /// Files newly blocked since the tray started, waiting for a notification
    fresh_blocked: Vec<core::Blocked>,
    /// Files already notified about this session (until they sync), so a client restart or a
    /// new login doesn't repeat the same notification
    told_blocked: HashSet<String>,
    told_blocked_order: VecDeque<String>,
    /// Told about the current big delete; a finished sync means it's over
    told_big_delete: bool,
    /// Large files being uploaded or downloaded right now (file manager badges)
    moving: HashSet<String>,
    /// The client has said nothing for far too long while running (see core::stall_limit)
    stalled_since: Option<DateTime<Local>>,
    /// We restarted the service once for being stuck; until a sync completes after this, a
    /// second stall is reported instead of restarting again
    stall_restart: Option<DateTime<Local>>,
    /// The client just ran out of folder watches, waiting for a notification
    fresh_watch_limit: bool,
}

impl Acc {
    fn new(info: AccountInfo, prefs: AccountPrefs) -> Self {
        Acc {
            info,
            prefs,
            unit: UnitState::default(),
            resyncing: false,
            log_phase: Phase::Unknown,
            last_complete: None,
            last_error: None,
            blocked: Vec::new(),
            fresh_blocked: Vec::new(),
            told_blocked: HashSet::new(),
            told_blocked_order: VecDeque::new(),
            told_big_delete: false,
            moving: HashSet::new(),
            stalled_since: None,
            stall_restart: None,
            fresh_watch_limit: false,
        }
    }
    /// The phase from systemd, the log and settings, before any stall is taken into account
    fn base_phase(&self) -> Phase {
        core::compose_phase_cached(
            &self.info,
            &self.prefs,
            &self.unit,
            self.resyncing,
            self.log_phase,
        )
    }
    fn status(&self) -> Status {
        Status {
            phase: core::with_stall(self.base_phase(), self.stalled_since),
            last_complete: self.last_complete,
            stalled_since: self.stalled_since,
            last_error: self.last_error.clone(),
            blocked: self.blocked.clone(),
            big_delete: None,
            unit: self.unit.clone(),
        }
    }
}

pub struct Tray {
    accs: Vec<Acc>,
    /// Set while the OneDrive Client for Linux is missing or too old
    client_problem: Option<String>,
    /// "Quit" also stops syncing (App settings)
    stop_on_quit: bool,
    /// A newer version that's out
    update: Option<String>,
}

fn open_settings(account: Option<&str>) {
    let me = core::self_command();
    let mut c = Command::new(me);
    c.arg("--settings");
    if let Some(a) = account {
        c.args(["--account", a]);
    }
    if let Ok(mut ch) = c.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        thread::spawn(move || {
            let _ = ch.wait();
        });
    }
}

/// The open panel's process and when it opened, and when the last one closed.
struct PopupState {
    open: Option<(u32, Instant, Option<OwnedFd>, mpsc::Sender<()>)>,
    closed: Option<Instant>,
    /// the activation token the tray host handed over for the latest click on the icon
    token: Option<(String, Instant)>,
}

static POPUP: Mutex<PopupState> = Mutex::new(PopupState {
    open: None,
    closed: None,
    token: None,
});

/// Opens the panel, or closes it if it's open. `at`: where the tray icon was clicked.
fn toggle_popup(at: Option<(i32, i32)>) {
    let mut p = POPUP.lock().unwrap();
    if let Some((_, opened, pidfd, close)) = p.open.as_ref() {
        // the second click of a double-click: it was meant to open the panel, which it did
        if opened.elapsed() < Duration::from_millis(400) {
            return;
        }
        let signalled = if let Some(fd) = pidfd {
            // SAFETY: fd is a pidfd for this exact child process, so PID reuse cannot redirect it.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                ) == 0
            }
        } else {
            false
        };
        if !signalled {
            // The waiter owns the Child and can safely signal it before reaping it, even on
            // kernels without pidfd support.
            let _ = close.send(());
        }
        p.open = None;
        return;
    }
    // A click on the icon while the panel is open first takes focus from the panel, which
    // closes itself, and then arrives here: that click was meant to close it, not reopen it.
    if p.closed
        .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
    {
        return;
    }
    let mut c = Command::new(core::self_command());
    c.arg("--popup");
    if let Some((x, y)) = at {
        c.args([x.to_string(), y.to_string()]);
    }
    // the token that came with this click (a token works once, and only soon after)
    if let Some((token, when)) = p.token.take()
        && when.elapsed() < Duration::from_secs(2)
    {
        c.env("XDG_ACTIVATION_TOKEN", token);
    }
    let Ok(mut child) = c.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return;
    };
    let pid = child.id();
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    let pidfd = if raw_fd >= 0 {
        // SAFETY: pidfd_open returned a new owned file descriptor.
        Some(unsafe { OwnedFd::from_raw_fd(raw_fd as i32) })
    } else {
        None
    };
    let (close_tx, close_rx) = mpsc::channel();
    p.open = Some((pid, Instant::now(), pidfd, close_tx));
    drop(p);
    thread::spawn(move || {
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
            match close_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(()) => {
                    // This thread alone reaps the child. If it is still running, its PID cannot
                    // have been reused; use SIGTERM so the popup can unload its desktop script.
                    if matches!(child.try_wait(), Ok(None)) {
                        unsafe {
                            libc::kill(pid as libc::pid_t, libc::SIGTERM);
                        }
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        let _ = child.wait();
        let mut p = POPUP.lock().unwrap();
        if p.open
            .as_ref()
            .is_some_and(|(open_pid, _, _, _)| *open_pid == pid)
        {
            p.open = None;
            // closed by itself (focus moved away); one closed by a click above isn't recorded,
            // so clicking again right away reopens it
            p.closed = Some(Instant::now());
        }
    });
}

impl Tray {
    fn overall(&self) -> Phase {
        let ps: Vec<Phase> = self.accs.iter().map(|a| a.status().phase).collect();
        if self.client_problem.is_some()
            || ps.iter().any(|p| {
                matches!(
                    p,
                    Phase::Error
                        | Phase::NeedsResync
                        | Phase::SignedOut
                        | Phase::BigDelete
                        | Phase::Stalled
                )
            })
        {
            Phase::Error
        } else if ps
            .iter()
            .any(|p| matches!(p, Phase::Syncing | Phase::Resyncing | Phase::Unknown))
        {
            Phase::Syncing
        } else if ps.is_empty()
            || ps
                .iter()
                .all(|p| matches!(p, Phase::Off | Phase::Stopped | Phase::Paused))
        {
            Phase::Off
        } else {
            Phase::UpToDate
        }
    }

    /// One line per account: its status at a glance; a click opens its settings (where any
    /// fix is).
    fn account_line(&self, idx: usize) -> MenuItem<Self> {
        let a = &self.accs[idx];
        let st = a.status();
        let k = a.info.key();
        StandardItem {
            // a single _ would turn into a keyboard shortcut and vanish
            label: crate::tr!("{} — {}", a.info.name, st.text(&a.prefs)).replace('_', "__"),
            icon_name: st.phase.icon().into(),
            activate: Box::new(move |_: &mut Self| open_settings(Some(&k))),
            ..Default::default()
        }
        .into()
    }

    fn sync_now(&mut self, idx: usize) {
        // the menu can be clicked after an account it was built with has gone
        let Some(a) = self.accs.get_mut(idx) else {
            return;
        };
        let info = a.info.clone();
        a.unit.starting = true;
        a.unit.active = true;
        a.last_error = None;
        thread::spawn(move || {
            if let Err(e) = core::sync_now(&info) {
                share::notify(&format!("OneDrive: {}", info.name), &e);
            }
        });
    }

    /// Pauses for `secs` seconds (-1 = until 06:00 tomorrow, 0 = resume).
    fn pause(&mut self, idx: usize, secs: i64) {
        let Some(a) = self.accs.get_mut(idx) else {
            return;
        };
        let info = a.info.clone();
        a.last_error = None;
        thread::spawn(move || {
            if let Err(e) = core::pause(&info, secs) {
                share::notify(&format!("OneDrive: {}", info.name), &e);
            }
        });
    }
}

impl ksni::Tray for Tray {
    // a click opens the panel; the menu is for a right-click
    const MENU_ON_ACTIVATE: bool = false;

    fn provide_xdg_activation_token(&mut self, token: String) {
        POPUP.lock().unwrap().token = Some((token, Instant::now()));
    }

    fn activate(&mut self, x: i32, y: i32) {
        // some trays don't say where (0, 0): the panel then goes by the mouse
        toggle_popup((x != 0 || y != 0).then_some((x, y)));
    }

    fn id(&self) -> String {
        core::APP_ID.into()
    }
    fn title(&self) -> String {
        "OneDrive".into()
    }
    fn icon_name(&self) -> String {
        self.overall().icon().into()
    }
    fn icon_theme_path(&self) -> String {
        core::icon_theme_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
    fn status(&self) -> ksni::Status {
        if self.overall() == Phase::Error {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let lines: Vec<String> = self
            .accs
            .iter()
            .map(|a| crate::tr!("{}: {}", a.info.name, a.status().text(&a.prefs)))
            .collect();
        let description = if let Some(p) = &self.client_problem {
            crate::tr!("{}. Open Settings to fix it.", p)
        } else if lines.is_empty() {
            crate::tr!("No accounts yet — open Settings to add one")
        } else {
            lines.join("\n")
        };
        ksni::ToolTip {
            title: "OneDrive".into(),
            description,
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut m: Vec<MenuItem<Self>> = Vec::new();
        if let Some(v) = &self.update {
            m.push(
                StandardItem {
                    label: crate::tr!("Update to version {} now", v),
                    icon_name: "system-software-update".into(),
                    activate: Box::new(|_: &mut Self| crate::update::update_now()),
                    ..Default::default()
                }
                .into(),
            );
            m.push(MenuItem::Separator);
        }
        if let Some(p) = &self.client_problem {
            m.push(
                StandardItem {
                    label: p.clone(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            m.push(
                StandardItem {
                    label: crate::tr!("How to fix this…"),
                    icon_name: "dialog-warning".into(),
                    activate: Box::new(|_: &mut Self| open_settings(None)),
                    ..Default::default()
                }
                .into(),
            );
            m.push(MenuItem::Separator);
        }
        m.extend((0..self.accs.len()).map(|i| self.account_line(i)));
        if self.accs.is_empty() {
            m.push(
                StandardItem {
                    label: crate::tr!("No accounts yet"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        m.push(MenuItem::Separator);
        m.push(
            StandardItem {
                label: crate::tr!("Show activity…"),
                icon_name: "view-list-details".into(),
                activate: Box::new(|_: &mut Self| toggle_popup(None)),
                ..Default::default()
            }
            .into(),
        );
        let syncable: Vec<usize> = (0..self.accs.len())
            .filter(|&i| {
                let a = &self.accs[i];
                a.info.signed_in && !a.resyncing && self.client_problem.is_none()
            })
            .collect();
        {
            let which = syncable.clone();
            m.push(
                StandardItem {
                    label: crate::tr!("Sync now"),
                    icon_name: "view-refresh".into(),
                    enabled: !which.is_empty(),
                    activate: Box::new(move |t: &mut Self| {
                        for &i in &which {
                            t.sync_now(i);
                        }
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        // pausing applies to the accounts syncing in the background
        let pausable: Vec<usize> = syncable
            .iter()
            .copied()
            .filter(|&i| self.accs[i].unit.enabled)
            .collect();
        let paused: Vec<usize> = pausable
            .iter()
            .copied()
            .filter(|&i| self.accs[i].prefs.paused())
            .collect();
        if !paused.is_empty() {
            m.push(
                StandardItem {
                    label: crate::tr!("Resume syncing"),
                    icon_name: "media-playback-start".into(),
                    activate: Box::new(move |t: &mut Self| {
                        for &i in &paused {
                            t.pause(i, 0);
                        }
                    }),
                    ..Default::default()
                }
                .into(),
            );
        } else if !pausable.is_empty() {
            let submenu = [
                (crate::tr!("For 1 hour"), 3600),
                (crate::tr!("For 4 hours"), 4 * 3600),
                (crate::tr!("Until tomorrow morning"), -1),
            ]
            .into_iter()
            .map(|(label, secs)| {
                let which = pausable.clone();
                StandardItem {
                    label,
                    activate: Box::new(move |t: &mut Self| {
                        for &i in &which {
                            t.pause(i, secs);
                        }
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
            m.push(
                SubMenu {
                    label: crate::tr!("Pause syncing"),
                    icon_name: "media-playback-pause".into(),
                    submenu,
                    ..Default::default()
                }
                .into(),
            );
        }
        match self.accs.len() {
            0 => {}
            1 => m.push(
                StandardItem {
                    label: crate::tr!("Open folder"),
                    icon_name: "folder-open".into(),
                    activate: Box::new(|t: &mut Self| {
                        if let Some(a) = t.accs.first() {
                            core::open_path(&a.info.sync_dir);
                        }
                    }),
                    ..Default::default()
                }
                .into(),
            ),
            n => {
                let submenu = (0..n)
                    .map(|i| {
                        StandardItem {
                            label: self.accs[i].info.name.replace('_', "__"),
                            activate: Box::new(move |t: &mut Self| {
                                if let Some(a) = t.accs.get(i) {
                                    core::open_path(&a.info.sync_dir);
                                }
                            }),
                            ..Default::default()
                        }
                        .into()
                    })
                    .collect();
                m.push(
                    SubMenu {
                        label: crate::tr!("Open folder"),
                        icon_name: "folder-open".into(),
                        submenu,
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        m.push(MenuItem::Separator);
        m.push(
            StandardItem {
                label: crate::tr!("Settings and accounts…"),
                icon_name: "configure".into(),
                activate: Box::new(|_: &mut Self| open_settings(None)),
                ..Default::default()
            }
            .into(),
        );
        m.push(
            StandardItem {
                label: if self.stop_on_quit {
                    crate::tr!("Quit and stop syncing")
                } else {
                    crate::tr!("Quit tray icon")
                },
                icon_name: "application-exit".into(),
                activate: Box::new(|_| {
                    // read again: the setting may have changed since the menu was built
                    let settings = AppSettings::load();
                    if settings.stop_sync_on_quit {
                        core::stop_all_units(&settings);
                    }
                    std::process::exit(0)
                }),
                ..Default::default()
            }
            .into(),
        );
        m
    }
}

/// Applies one log event to the account using `unit`. Returns Some(false) once that account is gone.
/// Applies log events to the account in one tray update (each update redraws the menu).
fn apply_events(
    handle: &ksni::blocking::Handle<Tray>,
    unit: &str,
    events: Vec<(DateTime<Local>, LogEvent)>,
) -> Option<bool> {
    handle.update(|t: &mut Tray| {
        let Some(a) = t
            .accs
            .iter_mut()
            .find(|a| a.info.unit.as_deref() == Some(unit))
        else {
            return false;
        };
        for (when, ev) in events {
            apply_one(a, when, ev);
        }
        publish_badges(t);
        true
    })
}

/// Whether this journal event changes the follower's set of active transfers. Repeated progress
/// percentages for the same file don't need another tray update.
fn track_follower_event(moving: &mut HashSet<String>, ev: &LogEvent) -> bool {
    match ev {
        LogEvent::Progress { path, done, .. } => {
            if *done {
                moving.remove(path)
            } else {
                moving.insert(path.clone())
            }
        }
        LogEvent::File { path, .. } => {
            moving.remove(path);
            true
        }
        LogEvent::Complete | LogEvent::ClientStart | LogEvent::Error(_) => {
            moving.clear();
            true
        }
        _ => true,
    }
}

fn clear_moving(handle: &ksni::blocking::Handle<Tray>, unit: &str) {
    handle.update(|t: &mut Tray| {
        if let Some(a) = t
            .accs
            .iter_mut()
            .find(|a| a.info.unit.as_deref() == Some(unit))
            && !a.moving.is_empty()
        {
            a.moving.clear();
            publish_badges(t);
        }
    });
}

struct FollowControl {
    stop: AtomicBool,
    done: AtomicBool,
    /// When the unit's log last said anything, in awake_secs(), so time spent suspended
    /// doesn't count as silence
    heard: AtomicU64,
}

impl FollowControl {
    fn new() -> Self {
        FollowControl {
            stop: AtomicBool::new(false),
            done: AtomicBool::new(false),
            heard: AtomicU64::new(awake_secs()),
        }
    }
}

#[derive(Debug, PartialEq)]
enum StallStep {
    /// Not stuck (or no longer)
    Fine,
    /// Stuck, and already reported
    Still,
    /// Newly stuck, and ours: restart it once
    Restart(String),
    /// Newly stuck, and restarting didn't help or isn't ours to do: show and tell
    Report,
}

/// What to do about an account whose log has been silent for `silent` awake seconds. `limit`
/// (which reads the client's config) is only asked once the silence is long enough to matter.
fn stall_step(a: &Acc, silent: u64, min: u64, limit: impl FnOnce() -> u64) -> StallStep {
    let running = a.unit.active && !a.unit.starting && !a.unit.retrying;
    let claims_fine = matches!(
        a.base_phase(),
        Phase::UpToDate | Phase::Syncing | Phase::Unknown
    );
    if !(running && claims_fine && silent >= min && silent >= limit()) {
        return StallStep::Fine;
    }
    if a.stalled_since.is_some() {
        return StallStep::Still;
    }
    // a restart only counts as tried until a sync completes after it
    let tried = a
        .stall_restart
        .is_some_and(|r| a.last_complete.is_none_or(|c| c < r));
    match &a.info.unit {
        Some(u) if a.info.managed && !tried => StallStep::Restart(u.clone()),
        _ => StallStep::Report,
    }
}

/// End-to-end tests can't wait half an hour (test builds only).
fn test_stall_secs() -> Option<u64> {
    #[cfg(feature = "test-driver")]
    if let Some(secs) = std::env::var("DRIVEDECK_TEST_STALL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Some(secs);
    }
    None
}

/// Nothing counts as stuck sooner than this, whatever the account's monitor_interval
const STALL_MIN_SECS: u64 = 30 * 60;

/// Seconds the computer has been awake since the tray started (Instant doesn't count suspend).
fn awake_secs() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs()
}

struct FollowDone(Arc<FollowControl>);

impl Drop for FollowDone {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::Release);
    }
}

fn prune_followers(
    following: &mut HashMap<String, Arc<FollowControl>>,
    present_units: &HashSet<String>,
) {
    following.retain(|unit, control| {
        if !present_units.contains(unit) {
            control.stop.store(true, Ordering::Release);
        }
        !control.done.load(Ordering::Acquire)
    });
}

fn prune_phase_history(last_phase: &mut HashMap<String, Phase>, present: &HashSet<String>) {
    last_phase.retain(|key, _| present.contains(key));
}

/// Waits for journal output with a short timeout so a removed account can stop a quiet follower.
fn journal_ready(out: &BufReader<std::process::ChildStdout>, control: &FollowControl) -> bool {
    while !control.stop.load(Ordering::Acquire) {
        let mut fd = libc::pollfd {
            fd: out.get_ref().as_raw_fd(),
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: `fd` points to one initialized pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut fd, 1, 1000) };
        if ready > 0 {
            return true;
        }
        if ready < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
    false
}

/// Hands the file manager badges the accounts' current state.
fn publish_badges(t: &Tray) {
    if !crate::badges::enabled() {
        // switched off: every file answers "no badge"
        crate::badges::publish(Vec::new());
        return;
    }
    crate::badges::publish(
        t.accs
            .iter()
            .filter(|a| a.info.signed_in)
            .map(|a| crate::badges::AccState {
                root: a.info.sync_dir.clone(),
                phase: a.status().phase,
                moving: a.moving.clone(),
                blocked: a.blocked.iter().map(|b| b.path.clone()).collect(),
            })
            .collect(),
    );
}

/// When the tray started: log lines from before then were already there, not news.
fn tray_started() -> DateTime<Local> {
    static STARTED: std::sync::OnceLock<DateTime<Local>> = std::sync::OnceLock::new();
    *STARTED.get_or_init(Local::now)
}

fn apply_one(a: &mut Acc, when: DateTime<Local>, ev: LogEvent) {
    core::track_blocked(&mut a.blocked, &ev);
    match &ev {
        LogEvent::Progress { path, done, .. } if !done => {
            a.moving.insert(path.clone());
        }
        LogEvent::Progress { path, .. } | LogEvent::File { path, .. } => {
            a.moving.remove(path);
        }
        // a finished run or a new one: nothing is half-moved any more
        LogEvent::Complete | LogEvent::ClientStart | LogEvent::Error(_) => a.moving.clear(),
        _ => {}
    }
    match &ev {
        LogEvent::Blocked { path, reason }
            if reason.needs_attention()
                && when.timestamp() >= tray_started().timestamp()
                && !a.told_blocked.contains(path) =>
        {
            if a.told_blocked_order.len() >= core::MAX_TRACKED_BLOCKED
                && let Some(oldest) = a.told_blocked_order.pop_front()
            {
                a.told_blocked.remove(&oldest);
            }
            a.told_blocked.insert(path.clone());
            a.told_blocked_order.push_back(path.clone());
            if a.fresh_blocked.len() >= core::MAX_TRACKED_BLOCKED {
                a.fresh_blocked.remove(0);
            }
            a.fresh_blocked.push(core::Blocked {
                path: path.clone(),
                reason: *reason,
            });
        }
        LogEvent::File { path, .. } => {
            a.told_blocked.remove(path);
            a.told_blocked_order.retain(|notified| notified != path);
        }
        _ => {}
    }
    match ev {
        LogEvent::Syncing => a.log_phase = Phase::Syncing,
        LogEvent::Complete => {
            a.told_big_delete = false;
            a.log_phase = Phase::UpToDate;
            a.last_complete = Some(when);
            a.last_error = None;
        }
        LogEvent::Error(e) => {
            a.log_phase = Phase::Error;
            a.last_error = Some(e);
        }
        LogEvent::NeedsResync => a.log_phase = Phase::NeedsResync,
        LogEvent::NeedsLogin => {
            a.log_phase = Phase::SignedOut;
            a.last_error = Some(crate::tr!("Microsoft needs you to sign in again"));
        }
        LogEvent::BigDelete => a.log_phase = Phase::BigDelete,
        // a new run checks again; until it says otherwise, the old big delete no longer applies
        LogEvent::ClientStart if a.log_phase == Phase::BigDelete => a.log_phase = Phase::Unknown,
        // news only when it happens while the tray watches (the window shows older ones)
        LogEvent::WatchLimit => {
            a.fresh_watch_limit |= when.timestamp() >= tray_started().timestamp();
        }
        LogEvent::Blocked { .. } | LogEvent::ClientStart | LogEvent::Progress { .. } => {}
        LogEvent::File { .. } => {}
    }
}

/// Follows one unit's journal for as long as the tray runs.
fn follow(handle: ksni::blocking::Handle<Tray>, unit: String, control: Arc<FollowControl>) {
    let _done = FollowDone(control.clone());
    // the status lines first, so "last synced" is right even when the recent log is busy
    let status: Vec<_> = core::journal_status_lines(&unit)
        .iter()
        .filter_map(|line| {
            let (when, msg) = core::split_journal_line(line)?;
            Some((when, core::parse_log(msg)?))
        })
        .collect();
    if apply_events(&handle, &unit, status) != Some(true) {
        return;
    }
    // files blocked before the recent log, and not synced since
    let blocked = core::blocked_files(&unit);
    let found = handle.update(|t: &mut Tray| {
        if let Some(a) = t
            .accs
            .iter_mut()
            .find(|a| a.info.unit.as_deref() == Some(unit.as_str()))
        {
            a.blocked = blocked;
            true
        } else {
            false
        }
    });
    if found != Some(true) {
        return;
    }
    loop {
        if control.stop.load(Ordering::Acquire) {
            return;
        }
        let child = Command::new("journalctl")
            .args([
                "--user",
                "-u",
                &unit,
                "-f",
                "-n",
                "400",
                "-o",
                "short-unix",
                "--no-pager",
                "-q",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            for _ in 0..10 {
                if control.stop.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
            continue;
        };
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut raw = Vec::new();
        let mut batch = Vec::new();
        let mut moving = HashSet::new();
        // Lines are read as bytes: a file name that isn't valid UTF-8 must not end the follow.
        // While more lines are already waiting (a busy sync), events are collected and applied
        // together, so a flood of file lines doesn't redraw the menu for every one of them.
        loop {
            if out.buffer().is_empty() && !journal_ready(&out, &control) {
                break;
            }
            raw.clear();
            match out.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            control.heard.store(awake_secs(), Ordering::Relaxed);
            let line = String::from_utf8_lossy(&raw);
            if let Some((when, msg)) = core::split_journal_line(line.trim_end())
                && let Some(ev) = core::parse_log(msg)
                && track_follower_event(&mut moving, &ev)
            {
                batch.push((when, ev));
            }
            if batch.is_empty() || (!out.buffer().is_empty() && batch.len() < 500) {
                continue;
            }
            if apply_events(&handle, &unit, std::mem::take(&mut batch)) != Some(true) {
                let _ = child.kill();
                let _ = child.wait();
                return; // account was removed
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        moving.clear();
        clear_moving(&handle, &unit);
        for _ in 0..5 {
            if control.stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn set_device_held(confdir: &std::path::Path, held: bool) {
    let mut s = AppSettings::load();
    s.prefs_mut(confdir).device_held = held;
    let _ = s.save();
}

/// Every few seconds: re-read accounts and settings, poll systemd, and apply schedules.
fn poll(handle: ksni::blocking::Handle<Tray>) {
    let mut following: HashMap<String, Arc<FollowControl>> = HashMap::new();
    let mut last_phase: HashMap<String, Phase> = HashMap::new();
    tray_started();
    let mut replaced_seen = false;
    let mut had_stalls = false;
    let mut round = 0u32;
    // first look for a new version a minute in (not in the rush of logging in), then daily
    let mut next_update_check = Instant::now() + Duration::from_secs(60);
    // storage warnings: a few minutes in, then every six hours
    let mut next_quota_check = Instant::now() + Duration::from_secs(180);
    loop {
        if UPDATE_CHECK_FAILED.swap(false, Ordering::Relaxed) {
            // offline at login, or GitHub busy: try again within the hour
            next_update_check = Instant::now() + Duration::from_secs(3600);
        }
        if Instant::now() >= next_update_check {
            next_update_check = Instant::now() + Duration::from_secs(24 * 3600);
            if AppSettings::load().check_updates.unwrap_or(true) {
                let h = handle.clone();
                thread::spawn(move || check_for_update(&h));
            }
        }
        if Instant::now() >= next_quota_check {
            next_quota_check = Instant::now() + Duration::from_secs(6 * 3600);
            thread::spawn(check_storage);
        }
        if round.is_multiple_of(20) {
            let problem = match core::check_client() {
                core::ClientCheck::Ok { .. } => None,
                core::ClientCheck::Missing => {
                    Some(crate::tr!("The OneDrive Client for Linux isn't installed").to_string())
                }
                core::ClientCheck::TooOld { version, .. } => Some(crate::tr!(
                    "The OneDrive Client for Linux ({}) is too old",
                    version
                )),
            };
            let fixed = handle
                .update(|t: &mut Tray| {
                    let was = t.client_problem.take();
                    t.client_problem = problem;
                    was.is_some() && t.client_problem.is_none()
                })
                .unwrap_or(false);
            if fixed {
                // the client was just installed: point the units at it and get them going
                core::repair_managed_units();
                core::start_idle_units(&AppSettings::load());
            }
        }
        round = round.wrapping_add(1);
        let settings = AppSettings::load();
        // a window that found a new version tells us at once, without waiting for the daily look
        if let Some(v) =
            crate::update::hinted_version().filter(|_| settings.check_updates.unwrap_or(true))
        {
            let _ = handle.update(|t: &mut Tray| {
                if t.update.as_deref() != Some(v.as_str()) {
                    t.update = Some(v);
                }
            });
        }
        // The lookup may use desktop D-Bus; do it on this polling thread once per round so
        // ksni callbacks only ever read the cache.
        let hold = core::device_hold();
        let (infos, mut unit_states) = core::discover_with_states(&settings);
        let present_units: HashSet<String> = infos.iter().filter_map(|i| i.unit.clone()).collect();
        prune_followers(&mut following, &present_units);
        let states: Vec<(UnitState, bool)> = infos
            .iter()
            .map(|i| {
                let st = i
                    .unit
                    .as_deref()
                    .map(|u| core::cached_unit_state(&mut unit_states, u, core::unit_state))
                    .unwrap_or_default();
                let resync_unit = core::resync_unit(&i.confdir);
                let resyncing =
                    core::cached_unit_state(&mut unit_states, &resync_unit, core::unit_state)
                        .active;
                (st, resyncing)
            })
            .collect();

        // schedules and pauses: start or stop enabled services to match
        let mut expired_pause = false;
        for (info, (st, resyncing)) in infos.iter().zip(&states) {
            let prefs = settings.prefs(&info.confdir);
            let Some(unit) = &info.unit else { continue };
            let managed_by_time = prefs.schedule.enabled
                || prefs.pause_until > 0
                || hold.is_some()
                || prefs.device_held;
            if !managed_by_time
                || !st.enabled
                || *resyncing
                || !info.signed_in
                || core::account_busy(&info.confdir)
                || core::unit_held(unit)
            {
                continue;
            }
            let allowed = prefs.sync_allowed();
            if allowed && !st.active && !st.failed {
                expired_pause |= prefs.pause_until > 0 && !prefs.paused();
                if prefs.device_held {
                    set_device_held(&info.confdir, false);
                }
                core::run_detached("systemctl", &["--user", "start", unit]);
            } else if !allowed && st.active {
                // remember a stop for the network or battery, so it restarts when that's over
                if hold.is_some() && !prefs.device_held {
                    set_device_held(&info.confdir, true);
                }
                core::run_detached("systemctl", &["--user", "stop", unit]);
            } else if prefs.device_held && hold.is_none() && !st.active && st.failed {
                set_device_held(&info.confdir, false);
            }
        }

        if expired_pause {
            let mut s = AppSettings::load();
            for p in s.accounts.values_mut() {
                if p.pause_until > 0 && !p.paused() {
                    p.pause_until = 0;
                }
            }
            let _ = s.save();
        }

        for i in &infos {
            if let Some(u) = &i.unit
                && !following.contains_key(u)
            {
                let control = Arc::new(FollowControl::new());
                following.insert(u.clone(), control.clone());
                let (h, u) = (handle.clone(), u.clone());
                thread::spawn(move || follow(h, u, control));
            }
        }

        handle.update(|t: &mut Tray| {
            t.stop_on_quit = settings.stop_sync_on_quit;
            // switched off since one was found: stop offering it
            if !settings.check_updates.unwrap_or(true) {
                t.update = None;
            }
            let mut old: Vec<Acc> = std::mem::take(&mut t.accs);
            for (info, (st, resyncing)) in infos.into_iter().zip(states) {
                let prefs = settings.prefs(&info.confdir);
                let mut a = match old.iter().position(|o| o.info.confdir == info.confdir) {
                    Some(p) => old.remove(p),
                    None => Acc::new(info.clone(), prefs.clone()),
                };
                if st.starting && !a.unit.starting {
                    a.log_phase = Phase::Unknown; // a fresh process: the old "complete" no longer applies
                }
                a.info = info;
                a.prefs = prefs;
                a.unit = st;
                a.resyncing = resyncing;
                t.accs.push(a);
            }
        });
        // A running client that has said nothing for far too long is stuck. One we manage is
        // restarted once quietly; if it gets stuck again before finishing a sync, or isn't ours
        // to restart, the account shows "Not responding" and the user is told.
        let now_awake = awake_secs();
        let test_stall = test_stall_secs();
        let silence: HashMap<String, u64> = following
            .iter()
            .map(|(u, c)| {
                let heard = c.heard.load(Ordering::Relaxed);
                (u.clone(), now_awake.saturating_sub(heard))
            })
            .collect();
        let restart: Vec<String> = handle
            .update(|t: &mut Tray| {
                let mut restart = Vec::new();
                for a in &mut t.accs {
                    let silent = a
                        .info
                        .unit
                        .as_ref()
                        .and_then(|u| silence.get(u).copied())
                        .unwrap_or(0);
                    let confdir = a.info.confdir.clone();
                    let limit =
                        || test_stall.unwrap_or_else(|| core::stall_limit(&confdir).as_secs());
                    match stall_step(a, silent, test_stall.unwrap_or(STALL_MIN_SECS), limit) {
                        StallStep::Fine => a.stalled_since = None,
                        StallStep::Still => {}
                        StallStep::Restart(unit) => {
                            a.stall_restart = Some(Local::now());
                            restart.push(unit);
                        }
                        StallStep::Report => {
                            let silent = chrono::Duration::seconds(silent as i64);
                            a.stalled_since = Some(Local::now() - silent);
                        }
                    }
                }
                restart
            })
            .unwrap_or_default();
        for unit in restart {
            if let Some(c) = following.get(&unit) {
                c.heard.store(awake_secs(), Ordering::Relaxed);
            }
            core::run_detached("systemctl", &["--user", "restart", &unit]);
        }
        let stalls: Vec<(std::path::PathBuf, DateTime<Local>)> = handle
            .update(|t: &mut Tray| {
                t.accs
                    .iter()
                    .filter_map(|a| Some((a.info.confdir.clone(), a.stalled_since?)))
                    .collect()
            })
            .unwrap_or_default();
        if had_stalls || !stalls.is_empty() {
            core::write_stalls(&stalls);
        }
        had_stalls = !stalls.is_empty();
        // tell the user when an account newly needs attention
        let phases: Vec<(String, String, Phase, Option<String>, bool)> = handle
            .update(|t: &mut Tray| {
                t.accs
                    .iter()
                    .map(|a| {
                        (
                            a.info.key(),
                            a.info.name.clone(),
                            a.status().phase,
                            a.info.unit.clone(),
                            a.unit.retrying,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let present_phase_keys = phases.iter().map(|(key, ..)| key.clone()).collect();
        prune_phase_history(&mut last_phase, &present_phase_keys);
        for (key, name, phase, unit, retrying) in &phases {
            if *phase == Phase::BigDelete
                && *retrying
                && let Some(u) = unit
            {
                // each restart would stop again (and the client may notify each time); only
                // while it waits to restart, as a run that just started may have been fixed
                core::hold_for_big_delete(u);
            }
            let before = last_phase.insert(key.clone(), *phase);
            let bad = |p: Phase| {
                matches!(
                    p,
                    Phase::Error
                        | Phase::NeedsResync
                        | Phase::SignedOut
                        | Phase::BigDelete
                        | Phase::Stalled
                )
            };
            // A big delete is told about once until a sync finishes (the log decides, so a
            // quick fix-and-repeat between two polls isn't missed); "Sync now" while it's still
            // there stops again without telling again.
            let big_delete_news = *phase == Phase::BigDelete
                && !handle
                    .update(|t: &mut Tray| {
                        let a = t.accs.iter_mut().find(|a| a.info.key() == *key)?;
                        Some(std::mem::replace(&mut a.told_big_delete, true))
                    })
                    .flatten()
                    .unwrap_or(true);
            let newly_bad =
                *phase != Phase::BigDelete && before.is_some_and(|b| bad(*phase) && !bad(b));
            if big_delete_news || newly_bad {
                let what = match (phase, unit) {
                    (Phase::BigDelete, Some(u)) => core::big_delete_what(&core::big_delete(u)),
                    _ => String::new(),
                };
                let big_delete_body = crate::tr!(
                    "{} were deleted on this computer, so syncing stopped before deleting them online. Click the OneDrive icon to choose what to do.",
                    what
                );
                let body = match phase {
                    Phase::BigDelete => big_delete_body,
                    Phase::SignedOut => crate::tr!(
                        "Microsoft needs you to sign in again. Open the tray menu to fix it."
                    ),
                    Phase::NeedsResync => crate::tr!(
                        "The sync engine needs a full resync. Open Settings to review and apply it."
                    ),
                    Phase::Stalled => crate::tr!(
                        "Syncing has stopped making progress, though it is still running. Click the OneDrive icon and choose Sync now to restart it."
                    ),
                    _ => crate::tr!("Syncing hit an error. Open the tray menu for details."),
                };
                share::notify(&format!("OneDrive: {name}"), &body);
            }
        }
        // tell the user about each file that newly couldn't sync (not ones from before the tray started)
        let fresh: Vec<(String, Vec<core::Blocked>)> = handle
            .update(|t: &mut Tray| {
                t.accs
                    .iter_mut()
                    .filter(|a| !a.fresh_blocked.is_empty())
                    .map(|a| (a.info.name.clone(), std::mem::take(&mut a.fresh_blocked)))
                    .collect()
            })
            .unwrap_or_default();
        for (name, list) in fresh {
            share::notify(&format!("OneDrive: {name}"), &core::blocked_text(&list));
        }
        let out_of_watches: Vec<String> = handle
            .update(|t: &mut Tray| {
                t.accs
                    .iter_mut()
                    .filter_map(|a| {
                        std::mem::take(&mut a.fresh_watch_limit).then(|| a.info.name.clone())
                    })
                    .collect()
            })
            .unwrap_or_default();
        for name in out_of_watches {
            share::notify(
                &format!("OneDrive: {name}"),
                &crate::tr!(
                    "Linux's limit on folders watched for changes ran out, so some changes on this computer won't sync right away. Open Settings to raise it."
                ),
            );
        }
        // after an update, hand over to the new version (once the new file has stayed put for a
        // whole round, so we don't catch an install half-way)
        if core::program_file_replaced() {
            if replaced_seen {
                restart_into_new_version();
            }
            replaced_seen = true;
        } else {
            replaced_seen = false;
        }
        // pauses, schedules and stopped services change badges without any log line
        handle.update(|t: &mut Tray| publish_badges(t));
        thread::sleep(Duration::from_secs(3));
    }
}

/// The tray runs with nowhere to print (started at login or by the window), so a panic in one of
/// its threads would vanish while that part silently stops. Each one is added to
/// ~/.local/share/drivedeck/logs/tray-errors.log (kept small) for bug reports.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let file = core::data_home()
            .join(core::APP_ID)
            .join("logs/tray-errors.log");
        if let Some(dir) = file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if fs::metadata(&file).is_ok_and(|m| m.len() > 256 * 1024) {
            let _ = fs::rename(&file, file.with_extension("log.old"));
        }
        let thread = thread::current().name().unwrap_or("unnamed").to_string();
        let line = format!(
            "{} {} (thread {thread}): {info}\n{}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            env!("CARGO_PKG_VERSION"),
            std::backtrace::Backtrace::force_capture()
        );
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&file) {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }));
}

static UPDATE_CHECK_FAILED: AtomicBool = AtomicBool::new(false);

/// Warns once when an account's storage is nearly full (90%) and again when it's full, as the
/// Windows app does. Full storage stops uploads.
fn check_storage() {
    let settings = AppSettings::load();
    for info in core::discover(&settings) {
        if !info.signed_in {
            continue;
        }
        let Ok(tok) = crate::graph::token(&info.confdir) else {
            continue;
        };
        let Ok(q) = crate::graph::quota(&tok, info.drive_id.as_deref()) else {
            continue;
        };
        let warned = settings.prefs(&info.confdir).quota_warned;
        let Some(level) = core::quota_warning(q.used, q.total, warned) else {
            continue;
        };
        let mut s = AppSettings::load();
        s.prefs_mut(&info.confdir).quota_warned = level;
        let _ = s.save();
        let amount = format!(
            "{} of {}",
            crate::graph::human_size(q.used),
            crate::graph::human_size(q.total)
        );
        let body = match level {
            100 => crate::tr!(
                "{} is full ({}). New and changed files won't upload until you free up space or get more storage.",
                info.name,
                amount
            ),
            90 => crate::tr!(
                "{} is almost full ({}). Free up space or get more storage before uploads stop.",
                info.name,
                amount
            ),
            _ => continue,
        };
        share::notify(&format!("OneDrive: {}", info.name), &body);
    }
}

/// Looks for a newer version; shows it in the menu, and tells the user once per version.
fn check_for_update(handle: &ksni::blocking::Handle<Tray>) {
    check_for_client_update();
    let latest = match crate::update::latest_version() {
        Ok(Some(v)) => v,
        // nothing to offer: look again tomorrow, as usual
        Ok(None) => return,
        Err(()) => {
            UPDATE_CHECK_FAILED.store(true, Ordering::Relaxed);
            return;
        }
    };
    if !crate::update::is_newer(&latest, env!("CARGO_PKG_VERSION")) {
        return;
    }
    let v = latest;
    handle.update(|t: &mut Tray| t.update = Some(v.clone()));
    let mut s = AppSettings::load();
    if s.update_notified.as_deref() != Some(v.as_str()) {
        s.update_notified = Some(v.clone());
        let _ = s.save();
        share::notify(
            &crate::tr!("OneDrive: a new version is out"),
            &crate::tr!(
                "Version {} of this app is available. Click the OneDrive tray icon (or right-click it) and choose Update.",
                v
            ),
        );
    }
}

/// Tells once per release when the OneDrive Client for Linux has a newer version out.
fn check_for_client_update() {
    let Some((installed, latest)) = crate::update::client_news() else {
        return;
    };
    let mut s = AppSettings::load();
    if s.client_update_notified.as_deref() == Some(latest.as_str()) {
        return;
    }
    s.client_update_notified = Some(latest.clone());
    let _ = s.save();
    share::notify(
        &crate::tr!("OneDrive: the sync engine has an update"),
        &crate::tr!(
            "Version {} of the OneDrive Client for Linux is out (you have {}). Open Settings to update it.",
            latest,
            installed
        ),
    );
}

/// Set on the tray we start after an update, so it waits for this one to leave.
pub const RESTART_ENV: &str = "DRIVEDECK_TRAY_RESTART";

fn restart_into_new_version() {
    let started = Command::new(core::self_command())
        .arg("--tray")
        .env(RESTART_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if started.is_ok() {
        std::process::exit(0);
    }
}

pub fn run() {
    // if an interrupted preview can't be put right, keep the tray (so the user still sees what's
    // going on) but start nothing: recovery keeps that account marked busy meanwhile
    let recovered = crate::ui::recover_interrupted_preview();
    if let Err(e) = &recovered {
        share::notify(
            &crate::tr!("Preview recovery needs attention"),
            &crate::tr!(
                "Sync services weren't started because preview settings couldn't be put back: {}",
                e
            ),
        );
    }
    // an AppImage update doesn't touch the Dolphin actions it once added for this user
    share::refresh_service_menu();
    share::refresh_user_icons();
    // answers KRunner's searches while the user has that switched on
    crate::runner::start();
    core::repair_managed_units();
    if recovered.is_ok() {
        core::start_idle_units(&AppSettings::load());
    }
    // At login we can start before the panel is ready, and on GNOME there may be no tray at
    // all. Keep running either way: the icon appears whenever a tray shows up, and schedules
    // and pauses are enforced meanwhile.
    let handle = match (Tray {
        accs: Vec::new(),
        client_problem: None,
        stop_on_quit: AppSettings::load().stop_sync_on_quit,
        update: None,
    })
    .assume_sni_available(true)
    .spawn()
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Couldn't start the tray icon service ({e}).");
            std::process::exit(1);
        }
    };
    crate::badges::serve();
    crate::badges::install_user_files();
    log_panics();
    let h = handle.clone();
    thread::spawn(move || poll(h));
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc() -> Acc {
        Acc::new(
            AccountInfo {
                name: "Work".into(),
                confdir: "/c".into(),
                unit: Some("onedrive-work".into()),
                managed: true,
                sync_dir: "/s".into(),
                drive_id: None,
                kind: "business".into(),
                signed_in: true,
            },
            AccountPrefs::default(),
        )
    }

    fn running(a: &mut Acc) {
        a.unit.exists = true;
        a.unit.active = true;
        a.unit.enabled = true;
        a.log_phase = Phase::UpToDate;
    }

    #[test]
    fn a_quiet_client_is_restarted_once_then_reported() {
        let mut a = acc();
        running(&mut a);
        let hour = 3600;
        // quiet, but not for long enough
        assert_eq!(
            stall_step(&a, 10 * 60, STALL_MIN_SECS, || 1800),
            StallStep::Fine
        );
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 2 * hour),
            StallStep::Fine
        );
        // stuck: ours, so restarted first
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Restart("onedrive-work".into())
        );
        a.stall_restart = Some(Local::now());
        // stuck again with no sync completed since: report, then stay reported
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Report
        );
        a.stalled_since = Some(Local::now());
        assert_eq!(a.status().phase, Phase::Stalled);
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Still
        );
        // a sync completed after the restart: next time, restart again
        a.stalled_since = None;
        a.last_complete = Some(Local::now() + chrono::Duration::seconds(1));
        assert!(matches!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Restart(_)
        ));
        // talking again clears it
        assert_eq!(stall_step(&a, 5, STALL_MIN_SECS, || 1800), StallStep::Fine);
    }

    #[test]
    fn only_a_running_client_that_claims_all_is_well_can_be_stuck() {
        let hour = 3600;
        let mut a = acc();
        running(&mut a);
        a.info.managed = false;
        // a service someone else set up is never restarted for them
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Report
        );
        // a known problem, a stop or a restart in progress already says more
        a.log_phase = Phase::Error;
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Fine
        );
        a.log_phase = Phase::UpToDate;
        a.unit.active = false;
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Fine
        );
        a.unit.active = true;
        a.unit.starting = true;
        assert_eq!(
            stall_step(&a, hour, STALL_MIN_SECS, || 1800),
            StallStep::Fine
        );
        // the limit is only read once the silence is long enough to matter
        a.unit.starting = false;
        assert_eq!(
            stall_step(&a, 60, STALL_MIN_SECS, || panic!(
                "read the config too early"
            )),
            StallStep::Fine
        );
    }

    fn blocked(path: &str, reason: core::BlockReason) -> LogEvent {
        LogEvent::Blocked {
            path: path.into(),
            reason,
        }
    }

    #[test]
    fn follower_tracks_transfer_changes_without_repeating_percent_updates() {
        let mut moving = HashSet::new();
        let progress = |done| LogEvent::Progress {
            up: true,
            path: "large.zip".into(),
            percent: 40,
            eta: None,
            done,
        };
        assert!(track_follower_event(&mut moving, &progress(false)));
        assert!(!track_follower_event(&mut moving, &progress(false)));
        assert!(moving.contains("large.zip"));
        assert!(track_follower_event(&mut moving, &progress(true)));
        assert!(moving.is_empty());
    }

    #[test]
    fn removed_quiet_followers_are_cancelled_then_pruned() {
        let control = Arc::new(FollowControl::new());
        let mut following = HashMap::from([("onedrive-a.service".into(), control.clone())]);
        prune_followers(&mut following, &HashSet::new());
        assert!(control.stop.load(Ordering::Acquire));
        assert!(following.contains_key("onedrive-a.service"));

        control.done.store(true, Ordering::Release);
        prune_followers(&mut following, &HashSet::new());
        assert!(following.is_empty());
    }

    #[test]
    fn removed_accounts_drop_phase_history() {
        let mut history = HashMap::from([
            ("account-a".into(), Phase::Error),
            ("account-b".into(), Phase::UpToDate),
        ]);
        prune_phase_history(&mut history, &HashSet::from(["account-b".into()]));
        assert_eq!(
            history,
            HashMap::from([("account-b".into(), Phase::UpToDate)])
        );
    }

    #[test]
    fn big_delete_phase() {
        let mut a = acc();
        let now = tray_started();
        apply_one(&mut a, now, LogEvent::Syncing);
        apply_one(&mut a, now, LogEvent::BigDelete);
        assert_eq!(a.log_phase, Phase::BigDelete);
        // a new run: no longer known until it says
        apply_one(&mut a, now, LogEvent::ClientStart);
        assert_eq!(a.log_phase, Phase::Unknown);
        // a client start doesn't touch other phases
        apply_one(&mut a, now, LogEvent::Complete);
        apply_one(&mut a, now, LogEvent::ClientStart);
        assert_eq!(a.log_phase, Phase::UpToDate);
    }

    #[test]
    fn blocked_notifications() {
        use crate::core::BlockReason::*;
        let mut a = acc();
        let old = tray_started() - chrono::Duration::hours(1);
        let now = tray_started() + chrono::Duration::seconds(5);
        // from before the tray started: listed, not announced
        apply_one(&mut a, old, blocked("old.docx", Locked));
        assert_eq!(a.blocked.len(), 1);
        assert!(a.fresh_blocked.is_empty());
        // new: announced once
        apply_one(&mut a, now, blocked("a.docx", Locked));
        assert_eq!(a.fresh_blocked.len(), 1);
        a.fresh_blocked.clear();
        // the client restarts and reports it again: still listed, not announced again
        apply_one(&mut a, now, LogEvent::ClientStart);
        assert!(a.blocked.is_empty());
        apply_one(&mut a, now, blocked("a.docx", Locked));
        assert_eq!(a.blocked.len(), 1);
        assert!(a.fresh_blocked.is_empty());
        // view-only files never notify
        apply_one(&mut a, now, blocked("v.pdf", NoDownload));
        assert!(a.fresh_blocked.is_empty());
        // once it uploads, a later block is news again
        apply_one(
            &mut a,
            now,
            LogEvent::File {
                kind: "↑",
                path: "a.docx".into(),
            },
        );
        apply_one(&mut a, now, blocked("a.docx", Locked));
        assert_eq!(a.fresh_blocked.len(), 1);
    }

    #[test]
    fn blocked_session_history_is_bounded() {
        use crate::core::BlockReason::Locked;
        let mut a = acc();
        let now = tray_started() + chrono::Duration::seconds(5);
        for n in 0..=core::MAX_TRACKED_BLOCKED {
            apply_one(&mut a, now, blocked(&format!("file-{n}"), Locked));
        }
        assert_eq!(a.blocked.len(), core::MAX_TRACKED_BLOCKED);
        assert_eq!(a.fresh_blocked.len(), core::MAX_TRACKED_BLOCKED);
        assert_eq!(a.told_blocked.len(), core::MAX_TRACKED_BLOCKED);
        assert!(!a.told_blocked.contains("file-0"));
        assert!(
            a.told_blocked
                .contains(&format!("file-{}", core::MAX_TRACKED_BLOCKED))
        );
    }
}
