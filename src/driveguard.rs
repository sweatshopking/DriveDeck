//! Keeps the client away from a OneDrive folder whose drive isn't there: a USB drive that was
//! unplugged, a network share that didn't mount. The client would see an empty folder and could
//! delete everything online, or download it all onto the disk underneath the mount point.
//!
//! The unit this app writes names the drive's mount point (when the OneDrive folder lives on any
//! mount other than the root one or the home folder's):
//!   ConditionPathIsMountPoint=<point>   it doesn't start while the drive is absent
//!   BindsTo=<point>.mount + After=      systemd stops it the moment the drive goes
//! (checked on a real user manager, which tracks every mount as a .mount unit: an unmount stopped
//! the service within a second, cleanly, without a restart; a start while absent did nothing).
//! The tray starts it again when the drive is back, if sync is otherwise allowed.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub point: PathBuf,
    pub fstype: String,
}

/// The mount table, as /proc/self/mountinfo lists it (later entries are mounted on top).
pub fn mounts() -> Vec<Mount> {
    std::fs::read_to_string("/proc/self/mountinfo")
        .map(|t| parse_mountinfo(&t))
        .unwrap_or_default()
}

/// mountinfo lines: "36 35 98:0 /root /mnt/a\040b rw,noatime master:1 - ext3 /dev/sda1 rw"
pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let point = left.split(' ').nth(4)?;
            let fstype = right.split(' ').next()?;
            Some(Mount {
                point: PathBuf::from(unescape(point)),
                fstype: fstype.to_string(),
            })
        })
        .collect()
}

/// The kernel writes space, tab, newline and backslash in mount points as \040, \011, \012, \134
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mount holding `path` (which need not exist): the deepest mount point above it; of several at
/// the same point, the last (the one on top).
fn holder<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    let path = normal(path);
    mounts
        .iter()
        .enumerate()
        .filter(|(_, m)| path.starts_with(&m.point))
        .max_by_key(|(i, m)| (m.point.components().count(), *i))
        .map(|(_, m)| m)
}

/// `path` without "." parts, repeated or trailing slashes (no link is followed: the drive may be absent)
fn normal(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

pub fn is_mount_point(point: &Path, mounts: &[Mount]) -> bool {
    let point = normal(point);
    mounts.iter().any(|m| m.point == point)
}

/// The mount point the unit for an account syncing `sync_dir` should wait for, if any: the mount
/// holding the OneDrive folder, when that isn't the root one or the home folder's. `previous` is the
/// one the unit named before: kept while that drive is absent (the folder then looks as if it were
/// on the root disk, and the guard is exactly what's needed). Automounted points are left alone: they
/// unmount when idle, which would stop syncing.
pub fn guard_mount(
    sync_dir: &Path,
    home: &Path,
    mounts: &[Mount],
    previous: Option<&Path>,
) -> Option<PathBuf> {
    if !sync_dir.is_absolute() {
        return None;
    }
    let sync_dir = normal(sync_dir);
    if let Some(prev) = previous {
        let prev = normal(prev);
        if prev != Path::new("/") && sync_dir.starts_with(&prev) && !is_mount_point(&prev, mounts) {
            return Some(prev);
        }
    }
    let m = holder(&sync_dir, mounts)?;
    let home_point = holder(home, mounts).map(|h| h.point.clone());
    let automounted = mounts
        .iter()
        .any(|o| o.point == m.point && o.fstype == "autofs");
    (m.point != Path::new("/") && Some(&m.point) != home_point.as_ref() && !automounted)
        .then(|| m.point.clone())
}

/// systemd's name for the mount unit at `point` (as `systemd-escape --path --suffix=mount`)
pub fn mount_unit(point: &Path) -> String {
    let s = normal(point).to_string_lossy().into_owned();
    let trimmed = s.trim_matches('/');
    if trimmed.is_empty() {
        return "-.mount".into();
    }
    let mut out = String::new();
    for (i, b) in trimmed.bytes().enumerate() {
        match b {
            b'/' => out.push('-'),
            b'.' if i == 0 => out.push_str("\\x2e"),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out + ".mount"
}

const CONDITION: &str = "ConditionPathIsMountPoint=";

/// The [Unit] lines that make a unit wait for the drive at `point`
pub fn unit_lines(point: &Path) -> String {
    let unit = mount_unit(point);
    format!(
        "{CONDITION}{}\nBindsTo={unit}\nAfter={unit}",
        point.to_string_lossy().replace('%', "%%")
    )
}

/// Whether a unit line is one of ours for the drive at `point` (rewritten together); other lines,
/// even other mounts' After=, are left as they are
pub fn is_guard_line(line: &str, point: Option<&Path>) -> bool {
    let Some(point) = point else { return false };
    let unit = mount_unit(point);
    line.starts_with(CONDITION)
        || line.strip_prefix("BindsTo=") == Some(unit.as_str())
        || line.strip_prefix("After=") == Some(unit.as_str())
}

/// The mount point a unit waits for (its ConditionPathIsMountPoint=)
pub fn from_unit(text: &str) -> Option<PathBuf> {
    text.lines()
        .find_map(|l| l.strip_prefix(CONDITION))
        .map(|p| PathBuf::from(p.trim().replace("%%", "%")))
}

/// The drive an account's unit waits for, when it isn't there now
pub fn missing_drive(unit_text: &str, mounts: &[Mount]) -> Option<PathBuf> {
    from_unit(unit_text).filter(|p| !is_mount_point(p, mounts))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "\
22 1 0:21 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw
23 22 0:22 /@home /home rw,relatime shared:2 - btrfs /dev/nvme0n1p2 rw
40 22 8:17 / /mnt/barracuda rw,relatime shared:9 - btrfs /dev/sdb1 rw
41 22 0:50 / /run/media/josh/My\\040Drive rw,nosuid - exfat /dev/sdc1 rw
42 22 0:51 / /mnt/share rw - nfs4 server:/export rw
43 22 0:52 / /mnt/auto rw - autofs systemd-1 rw
44 43 0:53 / /mnt/auto rw - cifs //nas/x rw
";

    fn m() -> Vec<Mount> {
        parse_mountinfo(INFO)
    }

    #[test]
    fn the_mount_table_is_read_with_escaped_names() {
        let ms = m();
        assert_eq!(ms.len(), 7);
        assert_eq!(ms[3].point, PathBuf::from("/run/media/josh/My Drive"));
        assert_eq!(ms[3].fstype, "exfat");
        assert_eq!(unescape("a\\134b\\011c"), "a\\b\tc");
        assert_eq!(unescape("end\\04"), "end\\04"); // a cut-off escape stays as it was
    }

    #[test]
    fn only_folders_on_other_drives_are_guarded() {
        let home = Path::new("/home/josh");
        let ms = m();
        // the home folder's mount and the root one: nothing to wait for
        assert_eq!(
            guard_mount(Path::new("/home/josh/OneDrive"), home, &ms, None),
            None
        );
        assert_eq!(
            guard_mount(Path::new("/srv/OneDrive"), home, &ms, None),
            None
        );
        // a second disk, a USB drive (spaces in the name), a network share
        assert_eq!(
            guard_mount(Path::new("/mnt/barracuda/OneDrive"), home, &ms, None),
            Some(PathBuf::from("/mnt/barracuda"))
        );
        assert_eq!(
            guard_mount(Path::new("/run/media/josh/My Drive/OD/"), home, &ms, None),
            Some(PathBuf::from("/run/media/josh/My Drive"))
        );
        assert_eq!(
            guard_mount(Path::new("/mnt/share"), home, &ms, None),
            Some(PathBuf::from("/mnt/share"))
        );
        // automounted (unmounts when idle): left alone
        assert_eq!(
            guard_mount(Path::new("/mnt/auto/OD"), home, &ms, None),
            None
        );
        // a relative path is never guessed at
        assert_eq!(guard_mount(Path::new("OneDrive"), home, &ms, None), None);
    }

    #[test]
    fn a_guard_is_kept_while_its_drive_is_absent() {
        let home = Path::new("/home/josh");
        let ms = m();
        let gone = Path::new("/run/media/josh/Stick");
        // unplugged: the folder now looks as if it were on the root disk; the old guard stays
        assert_eq!(
            guard_mount(Path::new("/run/media/josh/Stick/OD"), home, &ms, Some(gone)),
            Some(gone.to_path_buf())
        );
        // the folder moved home: the old guard goes
        assert_eq!(
            guard_mount(Path::new("/home/josh/OD"), home, &ms, Some(gone)),
            None
        );
        // the drive is back under another guard-worthy mount: the current one wins
        assert_eq!(
            guard_mount(
                Path::new("/mnt/barracuda/OD"),
                home,
                &ms,
                Some(Path::new("/mnt/barracuda"))
            ),
            Some(PathBuf::from("/mnt/barracuda"))
        );
    }

    #[test]
    fn mount_unit_names_match_systemd_escape() {
        // as `systemd-escape -p --suffix=mount`
        assert_eq!(
            mount_unit(Path::new("/mnt/barracuda")),
            "mnt-barracuda.mount"
        );
        assert_eq!(
            mount_unit(Path::new("/home/josh/dd-guard/my-drive")),
            "home-josh-dd\\x2dguard-my\\x2ddrive.mount"
        );
        assert_eq!(
            mount_unit(Path::new("/run/media/josh/My Drive/")),
            "run-media-josh-My\\x20Drive.mount"
        );
        assert_eq!(mount_unit(Path::new("/.hidden/x")), "\\x2ehidden-x.mount");
        assert_eq!(mount_unit(Path::new("/")), "-.mount");
    }

    #[test]
    fn unit_lines_round_trip_and_tell_whether_the_drive_is_there() {
        let point = Path::new("/run/media/josh/100% Drive");
        let lines = unit_lines(point);
        assert!(lines.contains("ConditionPathIsMountPoint=/run/media/josh/100%% Drive\n"));
        assert!(lines.contains("BindsTo=run-media-josh-100\\x25\\x20Drive.mount\n"));
        assert!(lines.lines().all(|l| is_guard_line(l, Some(point))));
        assert!(!is_guard_line("After=network-online.target", Some(point)));
        assert!(!is_guard_line("After=other.mount", Some(point)));
        assert!(!is_guard_line(&lines, None));
        let unit = format!("[Unit]\nDescription=x\n{lines}\n[Service]\n");
        assert_eq!(from_unit(&unit), Some(point.to_path_buf()));
        assert_eq!(missing_drive(&unit, &m()), Some(point.to_path_buf()));
        let here = format!("[Unit]\n{}\n", unit_lines(Path::new("/mnt/share")));
        assert_eq!(missing_drive(&here, &m()), None);
        assert_eq!(missing_drive("[Unit]\n", &m()), None);
    }
}
