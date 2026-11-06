//! Searching OneDrive from KRunner (Plasma's Alt+Space search): the tray answers KRunner's
//! `org.kde.krunner1` D-Bus calls by asking Microsoft to search each account, so files that
//! aren't on this computer turn up too. Opening one goes to its local copy when it's synced,
//! otherwise to the OneDrive website.
//!
//! Off unless the user turns it on: what's typed in KRunner (three letters or more) is sent to
//! Microsoft. Turning it on adds KRunner's plugin description to the user's own folder; turning
//! it off takes it away again.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedValue, Value};

use crate::core::{self, AccountInfo, AppSettings};
use crate::graph;

pub const SERVICE: &str = "io.github.sweatshopking.DriveDeck.Runner";
const PATH: &str = "/runner";
/// Shorter searches would ask Microsoft after every other keystroke and match nearly everything
const MIN_LETTERS: usize = 3;
/// KRunner asks again after each keystroke; only the last one of a burst is worth a request
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(300);
/// The answer is waited for this long in all (KRunner drops slow runners' results anyway)
const WAIT: Duration = Duration::from_secs(4);
const PER_ACCOUNT: usize = 8;

// ---------------------------------------------------------------- the plugin description

/// Where KRunner looks for D-Bus runners the user added
pub fn plugin_path() -> PathBuf {
    core::data_home()
        .join("krunner/dbusplugins")
        .join(format!("{}.desktop", core::APP_ID))
}

fn plugin_body() -> String {
    format!(
        "[Desktop Entry]\n\
         Name={}\n\
         Comment={}\n\
         Type=Service\n\
         Icon={}\n\
         X-KDE-PluginInfo-Author=DriveDeck\n\
         X-KDE-PluginInfo-Name={}\n\
         X-KDE-PluginInfo-Version=1.0\n\
         X-KDE-PluginInfo-License=GPL\n\
         X-KDE-PluginInfo-EnabledByDefault=true\n\
         X-Plasma-API=DBus\n\
         X-Plasma-DBusRunner-Service={SERVICE}\n\
         X-Plasma-DBusRunner-Path={PATH}\n\
         X-Plasma-Request-Actions-Once=true\n",
        crate::tr!("OneDrive files"),
        crate::tr!("Search your OneDrive, including files that aren't on this computer"),
        core::APP_ID,
        core::APP_ID,
    )
}

/// Makes sure KRunner's description of us is there; true if it had to be (re)written.
fn install_plugin() -> bool {
    let path = plugin_path();
    let body = plugin_body();
    if std::fs::read_to_string(&path).is_ok_and(|old| old == body) {
        return false;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    core::atomic_write(&path, body.as_bytes()).is_ok()
}

/// Takes the description away again, if it's ours; true if it was there.
fn remove_plugin() -> bool {
    let path = plugin_path();
    let ours = std::fs::read_to_string(&path).is_ok_and(|t| t.contains(SERVICE));
    ours && std::fs::remove_file(&path).is_ok()
}

/// KRunner reads its plugin list when it starts, and starts again by itself when next used.
/// Only asked to quit if it's running: the quit request would otherwise start it first.
fn restart_krunner() {
    if core::which("kquitapp6").is_some()
        && crate::dbus::name_owner_pid("org.kde.krunner").is_some()
    {
        core::run_detached("kquitapp6", &["krunner"]);
    }
}

// ---------------------------------------------------------------- what a search returns

/// KRunner's `RemoteMatch`: id, text, icon, category relevance, relevance, properties
pub(crate) type Match = (
    String,
    String,
    String,
    i32,
    f64,
    HashMap<String, OwnedValue>,
);

/// KRunner's own scale for how well a whole category fits (Moderate)
const CATEGORY_RELEVANCE: i32 = 20;

fn text_value(s: impl Into<String>) -> OwnedValue {
    Value::from(s.into())
        .try_to_owned()
        .unwrap_or_else(|_| OwnedValue::from(false))
}

pub(crate) fn icon_for(name: &str, folder: bool) -> &'static str {
    if folder {
        return "folder";
    }
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    match ext.as_str() {
        "doc" | "docx" | "docm" | "odt" | "rtf" | "txt" | "md" => "x-office-document",
        "xls" | "xlsx" | "xlsm" | "ods" | "csv" => "x-office-spreadsheet",
        "ppt" | "pptx" | "pptm" | "odp" => "x-office-presentation",
        "pdf" => "application-pdf",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "bmp" | "svg" | "tif" | "tiff" => {
            "image-x-generic"
        }
        "mp3" | "flac" | "ogg" | "m4a" | "wav" | "opus" => "audio-x-generic",
        "mp4" | "mkv" | "mov" | "avi" | "webm" => "video-x-generic",
        "zip" | "7z" | "rar" | "tar" | "gz" => "application-x-archive",
        _ => "text-x-generic",
    }
}

/// A result's id is what Run gets back: its web address, then (after a newline) the path of its
/// synced copy if there is one.
fn encode_id(url: &str, local: Option<&Path>) -> String {
    match local {
        Some(p) => format!("{url}\n{}", p.display()),
        None => url.to_string(),
    }
}

fn decode_id(id: &str) -> (&str, Option<&Path>) {
    match id.split_once('\n') {
        Some((url, local)) if !local.is_empty() => (url, Some(Path::new(local))),
        Some((url, _)) => (url, None),
        None => (id, None),
    }
}

fn to_match(info: &AccountInfo, hit: &graph::Hit, rank: usize, query: &str) -> Match {
    let local = (!hit.path.is_empty())
        .then(|| info.sync_dir.join(&hit.path))
        .filter(|p| p.exists());
    let subtext = match hit.parent.as_deref() {
        // Microsoft didn't say which folder: just the account
        None => info.name.clone(),
        Some("") => format!("{} · {}", info.name, crate::tr!("Top of OneDrive")),
        Some(p) => format!("{} · {p}", info.name),
    };
    let mut props = HashMap::new();
    props.insert("subtext".to_string(), text_value(subtext));
    props.insert("category".to_string(), text_value("OneDrive"));
    (
        encode_id(&hit.web_url, local.as_deref()),
        hit.name.clone(),
        icon_for(&hit.name, hit.folder).to_string(),
        CATEGORY_RELEVANCE,
        // Microsoft's order, best first; a name that has every word typed beats a match on
        // the contents alone, whichever account it's in
        (0.6 - 0.02 * rank as f64).max(0.1)
            + if name_matches(&hit.name, query) {
                0.3
            } else {
                0.0
            },
        props,
    )
}

/// Whether every word of the query is in the name (case ignored)
fn name_matches(name: &str, query: &str) -> bool {
    let name = name.to_lowercase();
    let mut words = query.split_whitespace().peekable();
    words.peek().is_some() && words.all(|w| name.contains(&w.to_lowercase()))
}

/// Whether a query is long enough to be worth asking Microsoft about
pub(crate) fn worth_asking(query: &str) -> bool {
    query.trim().chars().count() >= MIN_LETTERS
}

// ---------------------------------------------------------------- asking Microsoft

fn accounts() -> Vec<AccountInfo> {
    static CACHE: Mutex<Option<(Instant, Vec<AccountInfo>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(30)
    {
        return list.clone();
    }
    let list: Vec<AccountInfo> = core::discover_with_states(&AppSettings::load())
        .0
        .into_iter()
        .filter(|a| a.signed_in)
        .collect();
    *cache = Some((Instant::now(), list.clone()));
    list
}

/// A sign-in token for the account, reused until shortly before it runs out
fn token_for(info: &AccountInfo) -> Result<graph::Token, String> {
    static TOKENS: Mutex<Option<HashMap<PathBuf, graph::Token>>> = Mutex::new(None);
    {
        let tokens = TOKENS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = tokens.as_ref().and_then(|m| m.get(&info.confdir))
            && t.expires > Instant::now() + Duration::from_secs(30)
        {
            return Ok(t.clone());
        }
    }
    let t = graph::token(&info.confdir)?;
    TOKENS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(info.confdir.clone(), t.clone());
    Ok(t)
}

fn search_account(info: &AccountInfo, query: &str) -> Result<Vec<graph::Hit>, String> {
    let t = token_for(info)?;
    graph::search(&t, info.drive_id.as_deref(), query, PER_ACCOUNT)
}

// ---------------------------------------------------------------- names on this computer

/// Microsoft's search misses files it hasn't indexed (often recent ones), so file and folder
/// names in the synced folders are searched too: a list kept in memory, read again in the
/// background once it's older than this.
const INDEX_AGE: Duration = Duration::from_secs(10 * 60);
/// Reading a huge folder stops here; what was read so far is still searched
const INDEX_MAX_ENTRIES: usize = 500_000;
const INDEX_MAX_TIME: Duration = Duration::from_secs(20);

struct NameIndex {
    built: Instant,
    /// (lower-case name, full path, is a folder)
    entries: Vec<(String, PathBuf, bool)>,
}

type Indexes = HashMap<PathBuf, (Option<Arc<NameIndex>>, bool)>;

fn indexes() -> &'static Mutex<Indexes> {
    static I: std::sync::OnceLock<Mutex<Indexes>> = std::sync::OnceLock::new();
    I.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Every name under `dir`, skipping hidden entries (.git and the like), common build folders
/// and symbolic links, within the size and time limits.
fn read_names(dir: &Path, max: usize, time: Duration) -> Vec<(String, PathBuf, bool)> {
    let until = Instant::now() + time;
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(list) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in list.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "__pycache__") {
                continue;
            }
            let Ok(t) = e.file_type() else { continue };
            if t.is_symlink() {
                continue;
            }
            if t.is_dir() {
                todo.push(e.path());
            }
            out.push((name.to_lowercase(), e.path(), t.is_dir()));
            if out.len() >= max || Instant::now() > until {
                return out;
            }
        }
    }
    out
}

/// The names in `dir`: read now the first time, then refreshed in the background
fn name_index(dir: &Path) -> Arc<NameIndex> {
    let build = |dir: &Path| {
        Arc::new(NameIndex {
            built: Instant::now(),
            entries: read_names(dir, INDEX_MAX_ENTRIES, INDEX_MAX_TIME),
        })
    };
    let mut map = indexes().lock().unwrap_or_else(|e| e.into_inner());
    let (current, refreshing) = map.entry(dir.to_path_buf()).or_insert((None, false));
    match current.clone() {
        Some(idx) => {
            if idx.built.elapsed() > INDEX_AGE && !*refreshing {
                *refreshing = true;
                let dir = dir.to_path_buf();
                std::thread::spawn(move || {
                    let fresh = build(&dir);
                    indexes()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(dir, (Some(fresh), false));
                });
            }
            idx
        }
        None => {
            drop(map);
            let idx = build(dir);
            indexes()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(dir.to_path_buf(), (Some(idx.clone()), false));
            idx
        }
    }
}

/// Files and folders in the account's synced folder whose names have every word typed:
/// names starting with what was typed first, then shorter names.
fn local_hits(info: &AccountInfo, query: &str, limit: usize) -> Vec<(PathBuf, bool)> {
    if !info.sync_dir.is_dir() {
        return Vec::new();
    }
    let idx = name_index(&info.sync_dir);
    best_names(&idx.entries, query, limit)
}

fn best_names(
    entries: &[(String, PathBuf, bool)],
    query: &str,
    limit: usize,
) -> Vec<(PathBuf, bool)> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let first = words.join(" ");
    let mut found: Vec<&(String, PathBuf, bool)> = entries
        .iter()
        .filter(|(n, ..)| words.iter().all(|w| n.contains(w.as_str())))
        .collect();
    found.sort_by_key(|(n, ..)| (!n.starts_with(&first), n.len()));
    found
        .into_iter()
        .take(limit)
        .map(|(_, p, d)| (p.clone(), *d))
        .collect()
}

fn local_match(info: &AccountInfo, path: &Path, folder: bool, rank: usize) -> Match {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path
        .parent()
        .and_then(|p| p.strip_prefix(&info.sync_dir).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let subtext = if parent.is_empty() {
        format!("{} · {}", info.name, crate::tr!("Top of OneDrive"))
    } else {
        format!("{} · {parent}", info.name)
    };
    let mut props = HashMap::new();
    props.insert("subtext".to_string(), text_value(subtext));
    props.insert("category".to_string(), text_value("OneDrive"));
    (
        encode_id("", Some(path)),
        name.clone(),
        icon_for(&name, folder).to_string(),
        CATEGORY_RELEVANCE,
        // a name match on this computer is as good as it gets
        (0.95 - 0.01 * rank as f64).max(0.5),
        props,
    )
}

/// Every signed-in account searched at once; whoever answers in time is included
pub(crate) fn search_all(query: &str) -> Vec<Match> {
    let accounts = accounts();
    let (tx, rx) = std::sync::mpsc::channel();
    for (n, info) in accounts.iter().cloned().enumerate() {
        let (tx, query) = (tx.clone(), query.to_string());
        std::thread::spawn(move || {
            let found = search_account(&info, &query);
            let _ = tx.send((n, info, found));
        });
    }
    drop(tx);
    let deadline = Instant::now() + WAIT;
    let mut answers = Vec::new();
    while answers.len() < accounts.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(a) => answers.push(a),
            Err(_) => break,
        }
    }
    let mut out = Vec::new();
    // every account's own names, then whatever Microsoft sent back in time
    for (n, info) in accounts.iter().enumerate() {
        let found = answers
            .iter()
            .find(|(m, ..)| *m == n)
            .and_then(|(_, _, f)| f.as_ref().ok());
        let local = local_hits(info, query, PER_ACCOUNT);
        let shown: std::collections::HashSet<&PathBuf> = local.iter().map(|(p, _)| p).collect();
        out.extend(
            local
                .iter()
                .enumerate()
                .map(|(rank, (p, folder))| local_match(info, p, *folder, rank)),
        );
        // Microsoft's own results (contents, files not on this computer), minus ones listed above
        out.extend(
            found
                .into_iter()
                .flatten()
                .filter(|hit| {
                    hit.path.is_empty() || !shown.contains(&info.sync_dir.join(&hit.path))
                })
                .enumerate()
                .map(|(rank, hit)| to_match(info, hit, rank, query)),
        );
    }
    out
}

// ---------------------------------------------------------------- opening a result

#[derive(Debug, PartialEq)]
pub(crate) enum Open {
    Url(String),
    File(PathBuf),
    /// A file found on this computer: its web address is asked for when it's wanted
    Lookup(PathBuf),
}

/// What Run should open. KRunner hands back the id we gave it, but anything on the session bus
/// can call Run, so only a https address, or a file inside one of the sync folders, is opened.
pub(crate) fn open_for(id: &str, action: &str, sync_dirs: &[PathBuf]) -> Option<Open> {
    let (url, local) = decode_id(id);
    let online = url
        .starts_with("https://")
        .then(|| Open::Url(url.to_string()));
    let inside = |p: &Path| {
        !p.components().any(|c| c == Component::ParentDir)
            && sync_dirs.iter().any(|d| p.starts_with(d))
            && p.exists()
    };
    if action == "online" {
        return match (online, local) {
            (Some(o), _) => Some(o),
            (None, Some(p)) if inside(p) => Some(Open::Lookup(p.to_path_buf())),
            _ => None,
        };
    }
    match local {
        Some(p) if inside(p) => Some(Open::File(p.to_path_buf())),
        _ => online,
    }
}

pub(crate) fn run(id: &str, action: &str) {
    let dirs: Vec<PathBuf> = accounts().into_iter().map(|a| a.sync_dir).collect();
    match open_for(id, action, &dirs) {
        Some(Open::Url(u)) => core::run_detached("xdg-open", &[&u]),
        Some(Open::File(p)) => core::open_path(&p),
        Some(Open::Lookup(p)) => crate::share::view_online(&p),
        None => {}
    }
}

// ---------------------------------------------------------------- the D-Bus service

/// A one-shot hand-over from a worker thread to an async D-Bus handler. While the handler waits
/// it holds no thread, so KRunner's next keystroke reaches us (and replaces this search).
struct Slot<T> {
    value: Option<T>,
    waker: Option<Waker>,
    closed: bool,
}

pub(crate) struct Sender<T>(Arc<Mutex<Slot<T>>>);
pub(crate) struct Receiver<T>(Arc<Mutex<Slot<T>>>);

pub(crate) fn hand_over<T>() -> (Sender<T>, Receiver<T>) {
    let slot = Arc::new(Mutex::new(Slot {
        value: None,
        waker: None,
        closed: false,
    }));
    (Sender(slot.clone()), Receiver(slot))
}

impl<T> Sender<T> {
    pub(crate) fn send(self, value: T) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).value = Some(value);
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.closed = true;
        if let Some(w) = slot.waker.take() {
            w.wake();
        }
    }
}

/// Resolves to the sent value, or to the default if the sender went away without one
impl<T: Default> Future for Receiver<T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = slot.value.take() {
            return Poll::Ready(v);
        }
        if slot.closed {
            return Poll::Ready(T::default());
        }
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

struct Runner {
    /// Counts searches as KRunner asks for them; a worker that finds a newer number is stale
    newest: Arc<AtomicU64>,
}

#[zbus::interface(name = "org.kde.krunner1")]
impl Runner {
    #[zbus(name = "Match")]
    async fn matches(&self, query: String) -> Vec<Match> {
        let mine = self.newest.fetch_add(1, Ordering::SeqCst) + 1;
        let newest = self.newest.clone();
        let (tx, rx) = hand_over();
        std::thread::spawn(move || {
            std::thread::sleep(DEBOUNCE);
            let current = |n: &AtomicU64| n.load(Ordering::SeqCst) == mine;
            let mut found = Vec::new();
            if current(&newest) && worth_asking(&query) {
                found = search_all(query.trim());
            }
            // typed on while Microsoft was answering: that search has taken over
            tx.send(if current(&newest) { found } else { Vec::new() });
        });
        rx.await
    }

    fn run(&self, match_id: String, action_id: String) {
        run(&match_id, &action_id);
    }

    fn actions(&self) -> Vec<(String, String, String)> {
        vec![(
            "online".into(),
            crate::tr!("Open on the OneDrive website"),
            "internet-web-browser".into(),
        )]
    }
}

fn serve() -> Option<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()
        .ok()?
        .name(SERVICE)
        .ok()?
        .serve_at(
            PATH,
            Runner {
                newest: Arc::new(AtomicU64::new(0)),
            },
        )
        .ok()?
        .build()
        .ok()
}

/// Runs for as long as the tray does: while the setting is on it answers KRunner and keeps the
/// plugin description in place; when it's turned off, both go away.
pub fn start() {
    std::thread::spawn(|| {
        let mut service = None;
        loop {
            let want = AppSettings::load().krunner_search == Some(true)
                && core::which("krunner").is_some();
            if want {
                if service.is_none() {
                    service = serve();
                }
                if service.is_some() && install_plugin() {
                    restart_krunner();
                }
            } else {
                // dropping the connection ends the service
                service = None;
                if remove_plugin() {
                    restart_krunner();
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = Box::pin(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn icons_follow_the_kind_of_file() {
        assert_eq!(icon_for("Budget.XLSX", false), "x-office-spreadsheet");
        assert_eq!(icon_for("notes.docx", false), "x-office-document");
        assert_eq!(icon_for("a.PDF", false), "application-pdf");
        assert_eq!(icon_for("photo.jpeg", false), "image-x-generic");
        assert_eq!(icon_for("README", false), "text-x-generic");
        assert_eq!(icon_for("Taxes.2025", true), "folder");
    }

    #[test]
    fn ids_carry_the_address_and_the_synced_copy() {
        let id = encode_id(
            "https://1drv.ms/x",
            Some(Path::new("/home/a/OneDrive/f.txt")),
        );
        assert_eq!(
            decode_id(&id),
            (
                "https://1drv.ms/x",
                Some(Path::new("/home/a/OneDrive/f.txt"))
            )
        );
        assert_eq!(decode_id("https://1drv.ms/x"), ("https://1drv.ms/x", None));
        assert_eq!(
            decode_id("https://1drv.ms/x\n"),
            ("https://1drv.ms/x", None)
        );
    }

    #[test]
    fn only_https_and_synced_files_are_opened() {
        let dir = std::env::temp_dir().join(format!("dd-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let dirs = vec![dir.clone()];
        let id = encode_id("https://1drv.ms/x", Some(&file));
        // Enter: the synced copy; the menu entry: the website
        assert_eq!(open_for(&id, "", &dirs), Some(Open::File(file.clone())));
        assert_eq!(
            open_for(&id, "online", &dirs),
            Some(Open::Url("https://1drv.ms/x".into()))
        );
        // a copy that's gone, or outside the sync folders, falls back to the website
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            open_for(&id, "", &dirs),
            Some(Open::Url("https://1drv.ms/x".into()))
        );
        let outside = encode_id("https://1drv.ms/x", Some(Path::new("/etc/passwd")));
        assert_eq!(
            open_for(&outside, "", &dirs),
            Some(Open::Url("https://1drv.ms/x".into()))
        );
        let sneaky = encode_id("https://1drv.ms/x", Some(&dir.join("../../etc/passwd")));
        assert_eq!(
            open_for(&sneaky, "", &dirs),
            Some(Open::Url("https://1drv.ms/x".into()))
        );
        // anything that isn't an https address is never opened
        assert_eq!(open_for("file:///etc/passwd", "", &dirs), None);
        assert_eq!(open_for("http://x", "online", &dirs), None);
        assert_eq!(open_for("javascript:alert(1)", "", &dirs), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_queries_are_not_sent_to_microsoft() {
        assert!(!worth_asking(""));
        assert!(!worth_asking("  ab "));
        assert!(worth_asking(" abc "));
        // letters, not bytes
        assert!(worth_asking("日本語"));
    }

    #[test]
    fn the_plugin_description_names_our_service() {
        let body = plugin_body();
        assert!(body.contains("X-Plasma-API=DBus"));
        assert!(body.contains(&format!("X-Plasma-DBusRunner-Service={SERVICE}")));
        assert!(body.contains("X-Plasma-DBusRunner-Path=/runner"));
        assert!(body.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn a_result_shows_its_account_and_folder() {
        let info = AccountInfo {
            name: "Work".into(),
            confdir: "/c".into(),
            unit: None,
            managed: false,
            sync_dir: "/nonexistent/OneDrive".into(),
            drive_id: None,
            kind: "business".into(),
            signed_in: true,
        };
        let hit = graph::Hit {
            name: "Plan.pptx".into(),
            web_url: "https://x.sharepoint.com/p".into(),
            folder: false,
            path: "Docs/Plan.pptx".into(),
            parent: Some("Docs".into()),
        };
        let (id, text, icon, cat, rel, props) = to_match(&info, &hit, 0, "budget");
        assert_eq!(
            (id.as_str(), text.as_str()),
            ("https://x.sharepoint.com/p", "Plan.pptx")
        );
        assert_eq!(
            (icon.as_str(), cat),
            ("x-office-presentation", CATEGORY_RELEVANCE)
        );
        assert!(rel > to_match(&info, &hit, 5, "budget").4);
        assert!(props.contains_key("subtext") && props.contains_key("category"));
        // the file named after the search comes before content matches, even ranked lower
        assert!(to_match(&info, &hit, 5, "plan PPTX").4 > rel);
        assert!(!name_matches("Plan.pptx", "plan budget") && !name_matches("Plan.pptx", "  "));
        // folder not known: the account alone, never a made-up "top"
        let unknown = graph::Hit {
            parent: None,
            path: String::new(),
            ..hit
        };
        let sub = &to_match(&info, &unknown, 0, "x").5["subtext"];
        assert_eq!(sub, &text_value("Work"));
    }

    #[test]
    fn names_on_this_computer_are_found_by_every_word() {
        let dir = std::env::temp_dir().join(format!("drivedeck-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for f in [
            "Afuna tikhale onyezimira.docx",
            "Baha'i/Afuna Tikhale Chichewa.docx",
            "Baha'i/Other.txt",
            ".git/afuna-object",
            "node_modules/afuna.js",
        ] {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "x").unwrap();
        }
        let names = read_names(&dir, 1000, Duration::from_secs(5));
        // hidden folders and build folders aren't read
        assert!(
            names
                .iter()
                .all(|(n, ..)| !n.contains("object") && n != "afuna.js")
        );
        let found = best_names(&names, "AFUNA", 8);
        assert_eq!(found.len(), 2);
        // both start with what was typed, so the shorter name comes first
        assert_eq!(found[0].0, dir.join("Baha'i/Afuna Tikhale Chichewa.docx"));
        // a name starting with the words beats one that only contains them
        let other = best_names(&names, "chichewa", 8);
        assert_eq!(other.len(), 1);
        assert_eq!(best_names(&names, "tikhale chichewa", 8).len(), 1);
        assert!(best_names(&names, "  ", 8).is_empty());
        assert_eq!(best_names(&names, "afuna", 1).len(), 1);
        // a local result opens the file, and its web address is looked up for "online"
        let id = encode_id("", Some(&found[0].0));
        let dirs = [dir.clone()];
        assert_eq!(
            open_for(&id, "", &dirs),
            Some(Open::File(found[0].0.clone()))
        );
        assert_eq!(
            open_for(&id, "online", &dirs),
            Some(Open::Lookup(found[0].0.clone()))
        );
        // ...but never for a path outside the synced folders
        assert_eq!(
            open_for(
                &encode_id("", Some(Path::new("/etc/passwd"))),
                "online",
                &dirs
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hand_over_gives_the_value_or_the_default() {
        let (tx, rx) = hand_over::<Vec<u8>>();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(vec![1, 2]);
        });
        assert_eq!(block_on(rx), vec![1, 2]);
        t.join().unwrap();
        // a worker that goes away without answering must not leave KRunner waiting
        let (tx, rx) = hand_over::<Vec<u8>>();
        drop(tx);
        assert!(block_on(rx).is_empty());
    }
}
