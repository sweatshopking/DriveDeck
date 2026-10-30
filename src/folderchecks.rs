//! Checks on an account's own folder that the client doesn't make: Git repositories inside it,
//! and a folder on a Windows (NTFS) drive that Windows' OneDrive app may also be syncing.
//! Read only; each finding is a short note for the account page.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Folders looked at before giving up on finding repositories (a big tree stays quick)
const MAX_DIRS: usize = 20_000;

pub fn notes(sync_dir: &Path, skip_dirs: &str, skip_dotfiles: bool) -> Vec<String> {
    notes_named(sync_dir, skip_dirs, skip_dotfiles, true)
}

/// As `notes`; `names` false leaves the folders' names out (for a problem report)
pub fn notes_named(
    sync_dir: &Path,
    skip_dirs: &str,
    skip_dotfiles: bool,
    names: bool,
) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(note) = windows_drive_note(sync_dir) {
        notes.push(note);
    }
    if !skip_dotfiles && !skips_git(skip_dirs) {
        let found = git_repos(sync_dir, 3);
        if !found.is_empty() {
            let names_list: Vec<String> = found
                .iter()
                .map(|p| {
                    let rel = p.strip_prefix(sync_dir).unwrap_or(p).display().to_string();
                    if rel.is_empty() {
                        "the top folder".into()
                    } else {
                        rel
                    }
                })
                .collect();
            let which = if names {
                names_list.join(", ")
            } else {
                match found.len() {
                    n if n >= 3 => format!("{n} or more"),
                    n => n.to_string(),
                }
            };
            notes.push(format!(
                "There are Git repositories in this folder ({which}). Syncing their .git folders can \
                 damage them, because the files arrive one at a time and out of order. Skip .git \
                 folders in Options, or keep repositories outside OneDrive."
            ));
        }
    }
    notes
}

/// Whether the client's skip_dir (`|`-separated names or paths) already leaves out .git
fn skips_git(skip_dirs: &str) -> bool {
    skip_dirs
        .split('|')
        .map(|s| s.trim().trim_start_matches("./").trim_end_matches('/'))
        .any(|s| s == ".git" || s.ends_with("/.git"))
}

/// Up to `max` folders under `root` (itself included) that are Git working copies: they hold a
/// `.git` folder, or a `.git` file (worktrees, submodules). Doesn't follow symbolic links or
/// look inside a repository once found.
fn git_repos(root: &Path, max: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = todo.pop() {
        seen += 1;
        if seen > MAX_DIRS || found.len() >= max {
            break;
        }
        if dir.join(".git").symlink_metadata().is_ok() {
            found.push(dir);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(e.path());
            }
        }
    }
    found.sort();
    found
}

/// Kernel and FUSE names of NTFS mounts (ntfs-3g shows as fuseblk)
fn is_windows_fs(fstype: &str) -> bool {
    matches!(fstype, "ntfs" | "ntfs3" | "fuseblk")
}

fn windows_drive_note(sync_dir: &Path) -> Option<String> {
    let out = Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "--target"])
        .arg(sync_dir)
        .output()
        .ok()?;
    let fstype = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !is_windows_fs(&fstype) {
        return None;
    }
    Some(if windows_onedrive_folder(sync_dir) {
        "This folder is on a Windows drive, and Windows' OneDrive app uses it too. Files that \
         Windows keeps online-only can't be opened from Linux, and the two apps can undo each \
         other's changes. A separate folder for Linux is safer."
            .into()
    } else {
        "This folder is on a Windows (NTFS) drive. If Windows' OneDrive app also syncs it, files \
         that Windows keeps online-only can't be opened from Linux, and the two apps can undo \
         each other's changes. A separate folder for Linux is safer."
            .into()
    })
}

/// Signs Windows' OneDrive app leaves in the top of its folder: its hidden marker file, or a
/// desktop.ini that names OneDrive (Windows writes it in UTF-16, so both encodings are checked).
fn windows_onedrive_folder(dir: &Path) -> bool {
    if dir.join(".849C9593-D756-4E56-8D6E-42412F2A707B").exists() {
        return true;
    }
    let Ok(bytes) = std::fs::read(dir.join("desktop.ini")) else {
        return false;
    };
    let utf16: Vec<u8> = "OneDrive".bytes().flat_map(|b| [b, 0]).collect();
    bytes.windows(8).any(|w| w == b"OneDrive") || bytes.windows(16).any(|w| w == utf16)
}

/// Where safety copies of `sync_dir` go: beside it (reflinks only work within one drive), in
/// a hidden folder, never inside it (that would sync)
fn safety_dir(sync_dir: &Path) -> Option<PathBuf> {
    Some(sync_dir.parent()?.join(".drivedeck-safety"))
}

/// Safety copies of one folder kept at a time; older ones are removed when a new one is made
const SAFETY_KEEP: usize = 2;

/// Before a full resync: a copy of `sync_dir` made with reflinks (btrfs, XFS), which share the
/// files' data with the originals, so it's quick and takes no space until files change. Ok(None)
/// when the drive can't do that (nothing is copied the slow way). `others` are the other
/// accounts' folders: a copy is never put inside one of them.
pub fn safety_copy(sync_dir: &Path, others: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    let Some(base) = safety_dir(sync_dir) else {
        return Ok(None);
    };
    if others.iter().any(|o| base.starts_with(o)) || !sync_dir.is_dir() {
        return Ok(None);
    }
    let name = sync_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "OneDrive".into());
    std::fs::create_dir_all(&base).map_err(|e| format!("Couldn't make {}: {e}", base.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S");
    let dest = base.join(format!("{name} {stamp}"));
    let ok = Command::new("cp")
        .args(["-a", "--reflink=always", "-T", "--"])
        .arg(sync_dir)
        .arg(&dest)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        // no reflinks here (or it went wrong part way): leave nothing half-made behind
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_dir(&base);
        return Ok(None);
    }
    prune_safety_copies(&base, &name);
    Ok(Some(dest))
}

/// Keeps the newest SAFETY_KEEP copies of `name` (their names sort by date)
fn prune_safety_copies(base: &Path, name: &str) {
    let prefix = format!("{name} ");
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let mut copies: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_prefix(&prefix).is_some_and(is_stamp) && e.file_type().is_ok_and(|t| t.is_dir())
        })
        .map(|e| e.path())
        .collect();
    copies.sort();
    let old = copies.len().saturating_sub(SAFETY_KEEP);
    for p in &copies[..old] {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// "2026-10-30 14.05.09"
fn is_stamp(s: &str) -> bool {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H.%M.%S").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dd-fc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_repositories_but_not_inside_them() {
        let d = tmp("git");
        std::fs::create_dir_all(d.join("Projects/app/.git/objects")).unwrap();
        std::fs::create_dir_all(d.join("Projects/app/vendor/lib/.git")).unwrap();
        std::fs::create_dir_all(d.join("Projects/wt")).unwrap();
        std::fs::write(d.join("Projects/wt/.git"), "gitdir: /elsewhere").unwrap();
        std::fs::create_dir_all(d.join("Documents")).unwrap();
        let found = git_repos(&d, 10);
        assert_eq!(found, vec![d.join("Projects/app"), d.join("Projects/wt")]);
        let n = notes(&d, "", false);
        assert!(
            n.iter().any(|t| t.contains("Projects/app, Projects/wt")),
            "{n:?}"
        );
        // skipped already: nothing to say
        assert!(notes(&d, "node_modules|.git", false).is_empty());
        assert!(notes(&d, "", true).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_skip_dir_the_way_the_client_writes_it() {
        assert!(skips_git(".git"));
        assert!(skips_git("node_modules | .git"));
        assert!(skips_git("Projects/.git/"));
        assert!(!skips_git(".github|git"));
        assert!(!skips_git(""));
    }

    #[test]
    fn knows_windows_onedrive_folders() {
        let d = tmp("win");
        assert!(!windows_onedrive_folder(&d));
        let ini: Vec<u8> =
            "[.ShellClassInfo]\r\nIconResource=C:\\Program Files\\Microsoft OneDrive\\OneDrive.exe"
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect();
        std::fs::write(d.join("desktop.ini"), ini).unwrap();
        assert!(windows_onedrive_folder(&d));
        assert!(is_windows_fs("ntfs3") && is_windows_fs("fuseblk") && !is_windows_fs("btrfs"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn safety_copies_are_pruned_to_the_newest_two_of_ours() {
        let d = tmp("safety");
        for n in [
            "OneDrive 2026-10-01 10.00.00",
            "OneDrive 2026-10-02 10.00.00",
            "OneDrive 2026-10-03 10.00.00",
            "OneDrive my own folder",
            "Work 2026-09-01 10.00.00",
        ] {
            std::fs::create_dir_all(d.join(n)).unwrap();
        }
        prune_safety_copies(&d, "OneDrive");
        let mut left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "OneDrive 2026-10-02 10.00.00",
                "OneDrive 2026-10-03 10.00.00",
                "OneDrive my own folder",
                "Work 2026-09-01 10.00.00"
            ]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_safety_copy_is_never_made_inside_another_account() {
        let d = tmp("nested");
        std::fs::create_dir_all(d.join("Work/Personal")).unwrap();
        assert_eq!(
            safety_copy(&d.join("Work/Personal"), &[d.join("Work")]),
            Ok(None)
        );
        assert!(!d.join("Work/.drivedeck-safety").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
