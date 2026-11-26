//! Folders that are on this computer but not in OneDrive yet, for the Folders tab: the tree lists
//! them next to the online folders so they can be ticked, and ticking one adds it to sync_list,
//! which makes the client upload it.
//!
//! Read only. Nothing here follows a symbolic link, and nothing walks deeper than one level at a
//! time (the tree lists a folder's insides when it is opened). A folder the client would skip
//! anyway (skip_dir, skip_dotfiles, .nosync, a link, a name OneDrive refuses, an account that only
//! downloads) is listed with the reason instead of a tick box that does nothing. The rules copy the
//! onedrive client's own (2.5.x: clientSideFiltering.d isDirNameExcluded, util.d wild2regex,
//! isValidName, isDotFile; sync.d pathIsProtectedByNoSync).

use std::fs;
use std::path::{Path, PathBuf};

use crate::core::ClientConfig;
use crate::graph;

/// Past this many folders in one place, the rest aren't looked inside (shown as having folders in
/// them; opening one finds out). Every folder is still listed: one left off the list could upload
/// without being asked for when a left-out folder around it is opened up.
const MAX_LOOKED_INTO: usize = 2000;

/// The account settings that decide whether the client would upload a local folder.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub skip_dir: String,
    pub skip_dir_strict: bool,
    pub skip_dotfiles: bool,
    pub check_nosync: bool,
    pub download_only: bool,
}

impl Rules {
    pub fn load(confdir: &Path) -> Self {
        let cfg = ClientConfig::load(confdir);
        Rules {
            skip_dir: cfg.get_joined("skip_dir").unwrap_or_default(),
            skip_dir_strict: cfg.get_bool("skip_dir_strict_match"),
            skip_dotfiles: cfg.get_bool("skip_dotfiles"),
            check_nosync: cfg.get_bool("check_nosync"),
            download_only: cfg.get_bool("download_only"),
        }
    }
}

/// Why the client wouldn't upload a local folder even when it's ticked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    DownloadOnly,
    SkipDir,
    Hidden,
    NoSync,
    Link,
    BadName,
}

impl Blocked {
    pub fn text(self) -> String {
        match self {
            Blocked::DownloadOnly => crate::tr!("won't upload: this account only downloads"),
            Blocked::SkipDir => {
                crate::tr!("won't upload: left out by \"Never sync folders named\" (Options)")
            }
            Blocked::Hidden => crate::tr!("won't upload: hidden folders are skipped"),
            Blocked::NoSync => crate::tr!("won't upload: it has a .nosync file"),
            Blocked::Link => crate::tr!("won't upload: it's a link to another place"),
            Blocked::BadName => crate::tr!("won't upload: OneDrive doesn't allow this name"),
        }
    }
}

/// One row's worth of a folder in the Folders tab: online, or only on this computer.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub has_children: bool,
    pub size: Option<u64>,
    /// On this computer, not in OneDrive (yet)
    pub local_only: bool,
    pub blocked: Option<Blocked>,
}

impl Entry {
    fn online(f: &graph::Folder) -> Self {
        Entry {
            name: f.name.clone(),
            has_children: f.has_children,
            size: f.size,
            local_only: false,
            blocked: None,
        }
    }
}

/// A folder found on this computer.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalDir {
    pub name: String,
    pub has_children: bool,
    pub blocked: Option<Blocked>,
}

/// The folders directly inside `path` ("/" for the top, "/A/B" below it) of the OneDrive folder
/// `sync_dir`, without following links. A folder that isn't on this computer gives an empty list.
pub fn list(sync_dir: &Path, path: &str, rules: &Rules) -> Vec<LocalDir> {
    let dir = local_path(sync_dir, path);
    // the way down must be real folders inside the OneDrive folder, not links out of it
    if !is_real_dir_below(sync_dir, &dir) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    // a folder the client skips is never walked into, so nothing inside it uploads either
    let above = blocked_above(sync_dir, path, rules);
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let is_link = kind.is_symlink();
        // a link only counts when it points at a folder (checked without going there)
        if !kind.is_dir() && !(is_link && entry.path().is_dir()) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue; // not UTF-8: OneDrive can't take it and sync_list can't name it
        };
        let rel = join(path, &name);
        let blocked = if is_link {
            Some(Blocked::Link)
        } else {
            above.or_else(|| blocked(sync_dir, &rel, rules))
        };
        let has_children =
            !is_link && (out.len() >= MAX_LOOKED_INTO || has_subfolder(&entry.path()));
        out.push(LocalDir {
            name,
            has_children,
            blocked,
        });
    }
    out.sort_by_key(|d| d.name.to_lowercase());
    out
}

/// The online folders, with the folders that are only on this computer added in name order. OneDrive
/// ignores case, so a local "docs" next to an online "Docs" is the same folder, not a new one.
pub fn merge(online: &[graph::Folder], local: Vec<LocalDir>) -> Vec<Entry> {
    let known: std::collections::HashSet<String> =
        online.iter().map(|f| f.name.to_lowercase()).collect();
    let mut out: Vec<Entry> = online.iter().map(Entry::online).collect();
    out.extend(
        local
            .into_iter()
            .filter(|l| !known.contains(&l.name.to_lowercase()))
            .map(|l| Entry {
                name: l.name,
                has_children: l.has_children,
                size: None,
                local_only: true,
                blocked: l.blocked,
            }),
    );
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// Why the client would skip the local folder at `rel` ("/A/B"), if it would.
pub fn blocked(sync_dir: &Path, rel: &str, rules: &Rules) -> Option<Blocked> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if rules.download_only {
        return Some(Blocked::DownloadOnly);
    }
    if !valid_name(rel) {
        return Some(Blocked::BadName);
    }
    if rules.skip_dotfiles && name.starts_with('.') {
        return Some(Blocked::Hidden);
    }
    if skip_dir_matches(&rules.skip_dir, rules.skip_dir_strict, rel) {
        return Some(Blocked::SkipDir);
    }
    if rules.check_nosync && has_nosync(sync_dir, rel) {
        return Some(Blocked::NoSync);
    }
    None
}

/// Why the client would skip the folder at `path` or one above it, if it would (the top folder
/// "/" itself never is).
fn blocked_above(sync_dir: &Path, path: &str, rules: &Rules) -> Option<Blocked> {
    let mut at = String::new();
    for part in path.split('/').filter(|s| !s.is_empty()) {
        at.push('/');
        at.push_str(part);
        if let Some(b) = blocked(sync_dir, &at, rules) {
            return Some(b);
        }
    }
    None
}

/// Whether the client's skip_dir (`|`-separated wildcards, case ignored) leaves out the folder at
/// `rel`: the whole path first, then (unless strict matching is on) each part of it on its own.
pub fn skip_dir_matches(mask: &str, strict: bool, rel: &str) -> bool {
    let patterns: Vec<&str> = mask.split('|').collect();
    if mask.split('|').all(|p| p.trim().is_empty()) {
        return false;
    }
    let matches = |s: &str| patterns.iter().any(|p| wildcard(p, s));
    let bare = rel.trim_start_matches('/');
    let rooted = format!("/{bare}");
    for c in [bare, rooted.as_str()] {
        if matches(c) || matches(&format!("{c}/")) {
            return true;
        }
    }
    !strict && bare.split('/').filter(|s| !s.is_empty()).any(matches)
}

/// The client's wildcards: `*` any run of characters (`/` too), `?` one character, a space any one
/// whitespace character, everything else itself; case ignored; the whole text must match.
fn wildcard(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        let one = |pc: char, tc: char| pc == '?' || pc == tc || (pc == ' ' && tc.is_whitespace());
        if pi < p.len() && p[pi] != '*' && one(p[pi], t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// OneDrive's rules for names (the client's isValidName), for the last part of `rel`.
pub fn valid_name(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let lower = name.to_lowercase();
    const RESERVED: [&str; 26] = [
        ".lock",
        "desktop.ini",
        "con",
        "prn",
        "aux",
        "nul",
        "com0",
        "com1",
        "com2",
        "com3",
        "com4",
        "com5",
        "com6",
        "com7",
        "com8",
        "com9",
        "lpt0",
        "lpt1",
        "lpt2",
        "lpt3",
        "lpt4",
        "lpt5",
        "lpt6",
        "lpt7",
        "lpt8",
        "lpt9",
    ];
    if name.is_empty()
        || RESERVED.contains(&lower.as_str())
        || lower.starts_with("~$")
        || lower.contains("_vti_")
        || name.starts_with(char::is_whitespace)
        || name.ends_with(char::is_whitespace)
        || name.ends_with('.')
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*/\\".contains(c))
    {
        return false;
    }
    // "forms" can't be a folder at the top
    let top = rel.trim_start_matches('/').split('/').count() == 1;
    !(top && lower == "forms")
}

/// Whether the folder at `rel`, or one above it up to the OneDrive folder itself, has a .nosync file.
fn has_nosync(sync_dir: &Path, rel: &str) -> bool {
    let mut dir = local_path(sync_dir, rel);
    loop {
        if fs::symlink_metadata(dir.join(".nosync")).is_ok() {
            return true;
        }
        if dir == sync_dir || !dir.pop() || !dir.starts_with(sync_dir) {
            return false;
        }
    }
}

/// Whether a folder has a folder (or a link to one) directly inside it.
fn has_subfolder(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_type()
            .is_ok_and(|k| k.is_dir() || (k.is_symlink() && e.path().is_dir()))
    })
}

/// Whether every step from `root` down to `dir` is a real folder (no links), so listing it can't
/// wander outside the OneDrive folder.
fn is_real_dir_below(root: &Path, dir: &Path) -> bool {
    let Ok(rest) = dir.strip_prefix(root) else {
        return false;
    };
    if !fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() || m.file_type().is_symlink()) {
        return false; // the OneDrive folder itself may be a link (that's the user's own setup)
    }
    let mut at = root.to_path_buf();
    for part in rest.components() {
        let std::path::Component::Normal(p) = part else {
            return false;
        };
        at.push(p);
        if !fs::symlink_metadata(&at).is_ok_and(|m| m.is_dir()) {
            return false;
        }
    }
    true
}

fn local_path(sync_dir: &Path, rel: &str) -> PathBuf {
    let mut p = sync_dir.to_path_buf();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    p
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" || dir.is_empty() {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "drivedeck-localfolders-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn online(name: &str) -> graph::Folder {
        graph::Folder {
            name: name.into(),
            has_children: false,
            size: Some(1),
        }
    }

    #[test]
    fn wildcards_match_the_way_the_client_does() {
        assert!(wildcard("Temp", "temp"));
        assert!(wildcard("*.cache", "/a/b.cache"));
        assert!(wildcard("Old?", "Old1"));
        assert!(!wildcard("Old?", "Old"));
        assert!(wildcard("My Stuff", "My\tStuff"));
        assert!(!wildcard("Temp", "Temporary"));
        assert!(wildcard("Temp*", "Temporary"));
        assert!(wildcard("*", ""));
        assert!(wildcard("a*b*c", "aXXbYYc"));
        assert!(!wildcard("a*b*c", "aXXbYY"));
        assert!(wildcard("[2023]", "[2023]"));
    }

    #[test]
    fn skip_dir_full_paths_then_parts() {
        assert!(!skip_dir_matches("", false, "/A"));
        assert!(!skip_dir_matches(" | ", false, "/A"));
        assert!(skip_dir_matches("Temp", false, "/Temp"));
        assert!(skip_dir_matches("Temp", false, "/Work/Temp"));
        assert!(
            skip_dir_matches("Temp", false, "/Temp/Inside"),
            "a part above counts"
        );
        assert!(
            !skip_dir_matches("Temp", true, "/Work/Temp"),
            "strict: whole path only"
        );
        assert!(skip_dir_matches("Work/Temp", true, "/Work/Temp"));
        assert!(skip_dir_matches("/Work/Temp", true, "/Work/Temp"));
        assert!(skip_dir_matches(
            "Cache|node_modules",
            false,
            "/p/node_modules"
        ));
        assert!(!skip_dir_matches("Cache|node_modules", false, "/p/src"));
    }

    #[test]
    fn names_onedrive_refuses() {
        for bad in [
            "/CON",
            "/con",
            "/a:b",
            "/x?",
            "/trail.",
            "/ lead",
            "/trail ",
            "/~$x",
            "/a_vti_b",
            "/Forms",
            "/desktop.ini",
            "/tab\there",
        ] {
            assert!(!valid_name(bad), "{bad:?} should be refused");
        }
        for good in [
            "/Documents",
            "/A/Forms",
            "/Photos [2023]",
            "/.hidden",
            "/Ünïcødé",
        ] {
            assert!(valid_name(good), "{good:?} should be allowed");
        }
    }

    #[test]
    fn merge_adds_local_folders_ignoring_case() {
        let local = vec![
            LocalDir {
                name: "docs".into(),
                has_children: false,
                blocked: None,
            },
            LocalDir {
                name: "New".into(),
                has_children: true,
                blocked: None,
            },
        ];
        let out = merge(&[online("Docs"), online("Zed")], local);
        let names: Vec<(&str, bool)> = out
            .iter()
            .map(|e| (e.name.as_str(), e.local_only))
            .collect();
        assert_eq!(names, [("Docs", false), ("New", true), ("Zed", false)]);
        assert_eq!(out[1].size, None);
    }

    #[test]
    fn lists_folders_and_why_some_wont_upload() {
        let root = tmp("list");
        for d in ["Plain/Sub", "Skipped", ".hidden", "Quiet/Inner", "Bad."] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("Quiet/.nosync"), "").unwrap();
        fs::write(root.join("file.txt"), "").unwrap();
        let outside = tmp("list-outside");
        fs::create_dir_all(outside.join("Secret")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("Link")).unwrap();
        std::os::unix::fs::symlink(root.join("file.txt"), root.join("FileLink")).unwrap();
        let rules = Rules {
            skip_dir: "Skipped".into(),
            skip_dotfiles: true,
            check_nosync: true,
            ..Default::default()
        };
        let found = list(&root, "/", &rules);
        let got: Vec<(&str, bool, Option<Blocked>)> = found
            .iter()
            .map(|d| (d.name.as_str(), d.has_children, d.blocked))
            .collect();
        assert_eq!(
            got,
            [
                (".hidden", false, Some(Blocked::Hidden)),
                ("Bad.", false, Some(Blocked::BadName)),
                ("Link", false, Some(Blocked::Link)),
                ("Plain", true, None),
                ("Quiet", true, Some(Blocked::NoSync)),
                ("Skipped", false, Some(Blocked::SkipDir)),
            ]
        );
        // a .nosync above counts for what's inside
        assert_eq!(
            blocked(&root, "/Quiet/Inner", &rules),
            Some(Blocked::NoSync)
        );
        // so does any reason the folder above is skipped: the client never walks into it
        fs::create_dir_all(root.join("Skipped/Deeper")).unwrap();
        fs::create_dir_all(root.join(".hidden/Deeper")).unwrap();
        let strict = Rules {
            skip_dir_strict: true,
            ..rules.clone()
        };
        assert_eq!(
            list(&root, "/Skipped", &strict)[0].blocked,
            Some(Blocked::SkipDir)
        );
        assert_eq!(
            list(&root, "/.hidden", &rules)[0].blocked,
            Some(Blocked::Hidden)
        );
        assert_eq!(list(&root, "/Plain", &rules)[0].blocked, None);
        // .nosync only matters when the client checks for it
        let plain = Rules::default();
        assert_eq!(blocked(&root, "/Quiet", &plain), None);
        // never lists through a link, even one inside the OneDrive folder
        assert!(list(&root, "/Link", &rules).is_empty());
        // a folder that isn't on this computer is just empty
        assert!(list(&root, "/Nope", &rules).is_empty());
        assert_eq!(list(&root, "/Plain", &rules)[0].name, "Sub");
        let dl = Rules {
            download_only: true,
            ..Default::default()
        };
        assert_eq!(blocked(&root, "/Plain", &dl), Some(Blocked::DownloadOnly));
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn nosync_at_the_top_counts_but_not_above_it() {
        let base = tmp("nosync-top");
        let root = base.join("OneDrive");
        fs::create_dir_all(root.join("A")).unwrap();
        fs::write(base.join(".nosync"), "").unwrap();
        let rules = Rules {
            check_nosync: true,
            ..Default::default()
        };
        assert_eq!(
            blocked(&root, "/A", &rules),
            None,
            "outside the OneDrive folder"
        );
        fs::write(root.join(".nosync"), "").unwrap();
        assert_eq!(blocked(&root, "/A", &rules), Some(Blocked::NoSync));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn lists_every_folder_even_past_the_limit() {
        let root = tmp("limit");
        for i in 0..MAX_LOOKED_INTO + 3 {
            fs::create_dir(root.join(format!("d{i}"))).unwrap();
        }
        let found = list(&root, "/", &Rules::default());
        assert_eq!(found.len(), MAX_LOOKED_INTO + 3, "none left off");
        // the first ones were looked into (empty), the rest are assumed to have folders
        assert_eq!(found.iter().filter(|d| d.has_children).count(), 3);
        fs::remove_dir_all(&root).unwrap();
    }
}
