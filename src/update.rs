//! Telling the user when a newer version is out: a once-a-day look at the project's latest
//! release on GitHub. Nothing is downloaded or installed until the user presses "Update now";
//! then the right file for this install (AppImage, .deb or .rpm) is fetched, checked against the
//! release's SHA256SUMS.txt and put in place. Anything else (the AUR, a build directory) gets
//! the release page instead. Switched off with "Tell me when a new version is out".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const RELEASES_URL: &str = "https://github.com/sweatshopking/DriveDeck/releases/latest";
const API_URL: &str = "https://api.github.com/repos/sweatshopking/DriveDeck/releases/latest";
/// The OneDrive Client for Linux's own releases (abraunegg/onedrive)
const CLIENT_API_URL: &str = "https://api.github.com/repos/abraunegg/onedrive/releases/latest";

/// "1.2.3" or "v1.2.3" as numbers; anything else (a pre-release tag, say) is None.
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().strip_prefix('v').unwrap_or(v.trim());
    let mut it = v.split('.');
    let t = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    it.next().is_none().then_some(t)
}

/// Whether `latest` is a newer version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

/// The latest release's version ("0.3.6"); Ok(None) when GitHub answered but has no ordinary
/// release to offer (none yet, or a pre-release); Err when it couldn't be asked (offline, busy).
pub fn latest_version() -> Result<Option<String>, ()> {
    latest_release(&api_url())
}

fn api_url() -> String {
    #[cfg(feature = "test-driver")]
    let url = std::env::var("LODG_TEST_RELEASES").unwrap_or_else(|_| API_URL.into());
    #[cfg(not(feature = "test-driver"))]
    let url = API_URL.to_string();
    url
}

/// The OneDrive Client for Linux's latest release, the same way as latest_version
pub fn latest_client_version() -> Result<Option<String>, ()> {
    #[cfg(feature = "test-driver")]
    let url = std::env::var("LODG_TEST_CLIENT_RELEASES").unwrap_or_else(|_| CLIENT_API_URL.into());
    #[cfg(not(feature = "test-driver"))]
    let url = CLIENT_API_URL.to_string();
    latest_release(&url)
}

/// A newer client release than the one installed: (installed, latest). None when it's current,
/// not installed, or GitHub couldn't be asked.
pub fn client_news() -> Option<(String, String)> {
    let crate::core::ClientCheck::Ok { version, .. } = crate::core::check_client() else {
        return None;
    };
    let latest = latest_client_version().ok().flatten()?;
    client_is_older(&version, &latest).then_some((version, latest))
}

/// Compares the client's own version words ("2.5.12", "v2.5.12-1+np1") by their numbers
fn client_is_older(installed: &str, latest: &str) -> bool {
    use crate::core::parse_client_version;
    matches!(
        (parse_client_version(installed), parse_client_version(latest)),
        (Some((i, _)), Some((l, _))) if i < l
    )
}

fn latest_release(url: &str) -> Result<Option<String>, ()> {
    Ok(release_json(url)?.and_then(|v| {
        v["tag_name"]
            .as_str()
            .and_then(parse)
            .map(|(a, b, c)| format!("{a}.{b}.{c}"))
    }))
}

/// The client authors' own "important" notes for their latest release, if they wrote any
pub fn latest_client_notes() -> Option<String> {
    #[cfg(feature = "test-driver")]
    let url = std::env::var("LODG_TEST_CLIENT_RELEASES").unwrap_or_else(|_| CLIENT_API_URL.into());
    #[cfg(not(feature = "test-driver"))]
    let url = CLIENT_API_URL.to_string();
    let release = release_json(&url).ok().flatten()?;
    let notes = important_notes(release["body"].as_str()?);
    (!notes.is_empty()).then_some(notes)
}

/// GitHub marks a release's warnings as quoted blocks starting "> [!IMPORTANT]" (or CAUTION,
/// WARNING). The first IMPORTANT one, as plain text, cut short: for the update offer to say
/// "this will take a while" before the user presses Update.
fn important_notes(body: &str) -> String {
    let mut lines = body.lines().skip_while(|l| !l.contains("[!IMPORTANT]"));
    lines.next();
    let text: Vec<&str> = lines
        .take_while(|l| l.trim_start().starts_with('>'))
        .map(|l| l.trim_start().trim_start_matches('>').trim())
        .collect();
    let text = text
        .join(" ")
        .replace("**", "")
        .replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const MAX: usize = 520;
    if text.chars().count() <= MAX {
        return text;
    }
    // cut at the end of a sentence if there is one in reach, else at a word
    let cut: String = text.chars().take(MAX).collect();
    let end = cut.rfind(". ").map(|i| i + 1).or_else(|| cut.rfind(' '));
    match end {
        Some(i) if i > MAX / 2 => format!(
            "{}{}",
            cut[..i].trim_end(),
            if cut[..i].ends_with('.') { "" } else { "…" }
        ),
        _ => format!("{cut}…"),
    }
}

fn release_json(url: &str) -> Result<Option<serde_json::Value>, ()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(url)
        // GitHub asks for one; the version tells nothing about the user
        .header("User-Agent", "drivedeck")
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|_| ())?;
    match resp.status().as_u16() {
        200 => {}
        // no release yet
        404 => return Ok(None),
        _ => return Err(()),
    }
    let v: serde_json::Value = resp.body_mut().read_json().map_err(|_| ())?;
    // drafts and pre-releases aren't offered
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return Ok(None);
    }
    Ok(Some(v))
}

/// The newer version, if there is one (None also when GitHub couldn't be asked).
pub fn newer_version() -> Option<String> {
    latest_version()
        .ok()
        .flatten()
        .filter(|l| is_newer(l, env!("CARGO_PKG_VERSION")))
}

// ---------------------------------------------------------------- updating this app

/// How the new version gets onto this computer
#[derive(Debug, PartialEq)]
enum Route {
    /// Replace this AppImage file (no password: it's the user's own file)
    AppImage(PathBuf),
    /// Install the package with this command (through pkexec), the file's path added last
    Package { asset: Asset, install: Vec<String> },
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Asset {
    Deb,
    Rpm,
}

/// What pressing "Update now" ended in
pub enum Outcome {
    /// The new version is in place
    Updated(String),
    /// Nothing to do here: the release page was opened instead
    OpenedPage,
    /// The user closed the password prompt
    Cancelled,
    /// This is already the newest version
    UpToDate,
    Failed(String),
}

static UPDATING: AtomicBool = AtomicBool::new(false);

/// "Update now", from the tray, the popup or the window: does it in the background and says
/// how it went in a notification. A second press while one is running does nothing.
pub fn update_now() {
    if UPDATING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let outcome = install_latest();
        UPDATING.store(false, Ordering::SeqCst);
        tell(&outcome);
    });
}

fn tell(outcome: &Outcome) {
    use crate::share::notify;
    match outcome {
        Outcome::Updated(v) => notify(
            &crate::tr!("DriveDeck updated"),
            &crate::tr!(
                "Version {} is installed. The tray icon restarts on it by itself; reopen any DriveDeck window that was open.",
                v
            ),
        ),
        Outcome::Failed(e) => {
            open_page();
            notify(
                &crate::tr!("Couldn't update DriveDeck"),
                &crate::tr!("{} The download page is open in your browser instead.", e),
            )
        }
        Outcome::OpenedPage | Outcome::Cancelled | Outcome::UpToDate => {}
    }
}

fn open_page() {
    crate::core::run_detached("xdg-open", &[RELEASES_URL]);
}

/// The whole update, start to finish; blocking (also used by `drivedeck --update`).
pub fn install_latest() -> Outcome {
    let Some(route) = route() else {
        open_page();
        return Outcome::OpenedPage;
    };
    let release = match release_json(&api_url()) {
        Ok(Some(r)) => r,
        _ => {
            return Outcome::Failed(crate::tr!("Couldn't ask GitHub about the new version."));
        }
    };
    let Some(version) = release["tag_name"]
        .as_str()
        .and_then(parse)
        .map(|(a, b, c)| format!("{a}.{b}.{c}"))
    else {
        return Outcome::Failed(crate::tr!("Couldn't ask GitHub about the new version."));
    };
    if !is_newer(&version, env!("CARGO_PKG_VERSION")) {
        return Outcome::UpToDate;
    }
    let assets = assets(&release);
    let arch = std::env::consts::ARCH;
    let wanted = match &route {
        Route::AppImage(_) => asset_name_appimage(&version, arch),
        Route::Package { asset, .. } => package_name(&assets, *asset, &version, arch),
    };
    let (Some(name), Some(sums_url)) = (
        wanted,
        assets
            .iter()
            .find(|(n, _)| n == "SHA256SUMS.txt")
            .map(|(_, u)| u.clone()),
    ) else {
        open_page();
        return Outcome::OpenedPage;
    };
    let Some(url) = assets
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, u)| u.clone())
    else {
        open_page();
        return Outcome::OpenedPage;
    };
    let sums = match download_text(&sums_url) {
        Ok(t) => t,
        Err(e) => return Outcome::Failed(e),
    };
    let Some(expected) = sum_for(&sums, &name) else {
        return Outcome::Failed(crate::tr!(
            "The release doesn't list a checksum for {}, so it wasn't installed.",
            name
        ));
    };
    match route {
        Route::AppImage(target) => replace_appimage(&target, &url, &expected, &version),
        Route::Package { install, .. } => {
            install_package(install, &url, &name, &expected, &version)
        }
    }
}

/// How this copy was installed decides how it's updated
fn route() -> Option<Route> {
    use crate::uninstall::Installed;
    match crate::uninstall::installed() {
        Installed::AppImage(p) => Some(Route::AppImage(p)),
        Installed::Package(remove) => route_for_package(&remove),
        Installed::PackageManual | Installed::BuildDirectory => None,
    }
}

/// From the removal command uninstall.rs worked out ("pkexec apt-get remove -y drivedeck"),
/// the matching install command; pacman (the AUR's drivedeck-bin) is updated through the AUR.
fn route_for_package(remove: &[String]) -> Option<Route> {
    let tool = remove.get(1)?.as_str();
    let (asset, install): (Asset, &[&str]) = match tool {
        "apt-get" => (Asset::Deb, &["pkexec", "apt-get", "install", "-y"]),
        "dnf" => (Asset::Rpm, &["pkexec", "dnf", "install", "-y"]),
        "zypper" => (
            Asset::Rpm,
            &[
                "pkexec",
                "zypper",
                "--non-interactive",
                "install",
                "--allow-unsigned-rpm",
            ],
        ),
        "rpm" => (Asset::Rpm, &["pkexec", "rpm", "-U"]),
        _ => return None,
    };
    Some(Route::Package {
        asset,
        install: install.iter().map(|s| s.to_string()).collect(),
    })
}

/// (name, download address) of every file in the release
fn assets(release: &serde_json::Value) -> Vec<(String, String)> {
    release["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some((
                        a["name"].as_str()?.to_string(),
                        a["browser_download_url"]
                            .as_str()
                            .filter(|u| u.starts_with("https://") || cfg!(feature = "test-driver"))?
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn asset_name_appimage(version: &str, arch: &str) -> Option<String> {
    matches!(arch, "x86_64" | "aarch64").then(|| format!("DriveDeck-{version}-{arch}.AppImage"))
}

/// drivedeck_1.5.0_amd64.deb; drivedeck-1.5.0-<release>.x86_64.rpm (the release number can change)
fn package_name(
    assets: &[(String, String)],
    asset: Asset,
    version: &str,
    arch: &str,
) -> Option<String> {
    match asset {
        Asset::Deb => {
            let a = match arch {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                _ => return None,
            };
            Some(format!("{}_{version}_{a}.deb", crate::core::APP_ID))
        }
        Asset::Rpm => {
            let (pre, post) = (
                format!("{}-{version}-", crate::core::APP_ID),
                format!(".{arch}.rpm"),
            );
            assets
                .iter()
                .map(|(n, _)| n)
                .find(|n| n.starts_with(&pre) && n.ends_with(&post))
                .cloned()
        }
    }
}

/// The checksum SHA256SUMS.txt gives for `name` ("<hex>  <name>" or "<hex> *<name>")
fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_lowercase())
    })
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

fn get(url: &str, timeout: Duration) -> Result<ureq::http::Response<ureq::Body>, String> {
    let resp = agent(timeout)
        .get(url)
        .header("User-Agent", "drivedeck")
        .call()
        .map_err(|e| crate::tr!("Couldn't download the update: {}", e))?;
    if resp.status().as_u16() != 200 {
        return Err(crate::tr!(
            "Couldn't download the update: {}",
            resp.status()
        ));
    }
    Ok(resp)
}

fn download_text(url: &str) -> Result<String, String> {
    get(url, Duration::from_secs(60))?
        .body_mut()
        .read_to_string()
        .map_err(|e| crate::tr!("Couldn't download the update: {}", e))
}

/// Downloads to `dest` (created only for us) and checks the checksum; the file is removed
/// again if anything is wrong.
fn download_checked(url: &str, dest: &Path, expected: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let fail = |e: String| {
        let _ = std::fs::remove_file(dest);
        e
    };
    let _ = std::fs::remove_file(dest);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)
        .map_err(|e| crate::tr!("Couldn't download the update: {}", e))?;
    let mut resp = get(url, Duration::from_secs(30 * 60)).map_err(fail)?;
    let mut body = resp.body_mut().with_config().limit(1 << 30).reader();
    std::io::copy(&mut body, &mut file)
        .map_err(|e| fail(crate::tr!("Couldn't download the update: {}", e)))?;
    drop(file);
    if sha256_of(dest).as_deref() != Some(expected) {
        return Err(fail(crate::tr!(
            "The download didn't match the release's checksum, so it wasn't installed."
        )));
    }
    Ok(())
}

/// sha256sum is part of coreutils, so on every system this runs on
fn sha256_of(path: &Path) -> Option<String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let hash = text.split_whitespace().next()?.to_lowercase();
    (out.status.success() && hash.len() == 64).then_some(hash)
}

/// Downloads next to the AppImage, then swaps it in with one rename: the running copy keeps
/// its (old) file, and the tray notices the new one and restarts on it.
fn replace_appimage(target: &Path, url: &str, expected: &str, version: &str) -> Outcome {
    let Some(dir) = target.parent() else {
        open_page();
        return Outcome::OpenedPage;
    };
    let tmp = dir.join(format!(
        ".{}.update-{}",
        crate::core::APP_ID,
        std::process::id()
    ));
    // can't write next to it (an AppImage in a system folder, say): the page instead
    let probe = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp);
    if probe.is_err() && !tmp.exists() {
        open_page();
        return Outcome::OpenedPage;
    }
    drop(probe);
    if let Err(e) = download_checked(url, &tmp, expected) {
        return Outcome::Failed(e);
    }
    use std::os::unix::fs::PermissionsExt;
    let done = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .and_then(|_| std::fs::rename(&tmp, target));
    match done {
        Ok(()) => Outcome::Updated(version.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Outcome::Failed(crate::tr!("Couldn't put the new version in place: {}", e))
        }
    }
}

/// Downloads the package to our private folder and installs it with the system's package
/// manager (one password prompt).
fn install_package(
    mut install: Vec<String>,
    url: &str,
    name: &str,
    expected: &str,
    version: &str,
) -> Outcome {
    let dir = crate::core::runtime_dir();
    let file = dir.join(name);
    if let Err(e) = download_checked(url, &file, expected) {
        return Outcome::Failed(e);
    }
    install.push(file.to_string_lossy().into_owned());
    let status = Command::new(&install[0])
        .args(&install[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = std::fs::remove_file(&file);
    match status.map(|s| s.code()) {
        Ok(Some(0)) => Outcome::Updated(version.to_string()),
        // pkexec: the prompt was closed (126) or not allowed (127)
        Ok(Some(126 | 127)) => Outcome::Cancelled,
        Ok(_) | Err(_) => Outcome::Failed(crate::tr!(
            "The package manager couldn't install version {}.",
            version
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_right_file_is_picked_for_each_kind_of_install() {
        let assets: Vec<(String, String)> = [
            "drivedeck-1.5.0-1.aarch64.rpm",
            "drivedeck-1.5.0-1.x86_64.rpm",
            "DriveDeck-1.5.0-x86_64.AppImage",
            "drivedeck_1.5.0_amd64.deb",
            "SHA256SUMS.txt",
        ]
        .iter()
        .map(|n| (n.to_string(), format!("https://example/{n}")))
        .collect();
        assert_eq!(
            asset_name_appimage("1.5.0", "x86_64").as_deref(),
            Some("DriveDeck-1.5.0-x86_64.AppImage")
        );
        assert_eq!(asset_name_appimage("1.5.0", "riscv64"), None);
        assert_eq!(
            package_name(&assets, Asset::Deb, "1.5.0", "x86_64").as_deref(),
            Some("drivedeck_1.5.0_amd64.deb")
        );
        assert_eq!(
            package_name(&assets, Asset::Rpm, "1.5.0", "x86_64").as_deref(),
            Some("drivedeck-1.5.0-1.x86_64.rpm")
        );
        assert_eq!(package_name(&assets, Asset::Rpm, "1.5.1", "x86_64"), None);
    }

    #[test]
    fn package_installs_follow_the_package_manager() {
        let r =
            |v: &[&str]| route_for_package(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            r(&["pkexec", "apt-get", "remove", "-y", "drivedeck"]),
            Some(Route::Package {
                asset: Asset::Deb,
                install: vec![
                    "pkexec".into(),
                    "apt-get".into(),
                    "install".into(),
                    "-y".into()
                ]
            })
        );
        assert!(matches!(
            r(&["pkexec", "dnf", "remove", "-y", "drivedeck"]),
            Some(Route::Package {
                asset: Asset::Rpm,
                ..
            })
        ));
        // the AUR's package is updated through the AUR
        assert_eq!(
            r(&["pkexec", "pacman", "-R", "--noconfirm", "drivedeck-bin"]),
            None
        );
    }

    #[test]
    fn checksums_are_read_from_the_sums_file() {
        let h = "a".repeat(64);
        let sums = format!(
            "{h}  DriveDeck-1.5.0-x86_64.AppImage\n{}  *drivedeck_1.5.0_amd64.deb\n",
            "B".repeat(64)
        );
        assert_eq!(sum_for(&sums, "DriveDeck-1.5.0-x86_64.AppImage"), Some(h));
        assert_eq!(
            sum_for(&sums, "drivedeck_1.5.0_amd64.deb"),
            Some("b".repeat(64))
        );
        assert_eq!(sum_for(&sums, "other"), None);
        assert_eq!(sum_for("xyz  DriveDeck", "DriveDeck"), None);
    }

    #[test]
    fn versions() {
        assert!(is_newer("v0.3.6", "0.3.5"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("v0.3.5", "0.3.5"));
        assert!(!is_newer("0.3.4", "0.3.5"));
        assert!(!is_newer("v0.4.0-beta", "0.3.5"));
        assert!(!is_newer("nonsense", "0.3.5"));
        assert!(!is_newer("0.4", "0.3.5"));
    }

    #[test]
    fn client_versions_compare_by_their_numbers() {
        assert!(client_is_older("v2.5.11", "2.5.12"));
        assert!(client_is_older("2.5.9-1+np1", "2.5.12"));
        assert!(!client_is_older("v2.5.12", "2.5.12"));
        assert!(!client_is_older("2.6.0", "2.5.12"));
        assert!(!client_is_older("unknown", "2.5.12"));
    }

    #[test]
    fn the_important_block_of_release_notes_is_read_as_plain_text() {
        let body = "## What's new\n\n> [!IMPORTANT]\n> **Client v2.5.12 increments the item database version from 18 to 19.** \
This change is intentional.\n>\n> When upgrading, the client will `recreate` the tables. This may take a long time.\n\n> [!CAUTION]\n> Not this one.\n";
        assert_eq!(
            important_notes(body),
            "Client v2.5.12 increments the item database version from 18 to 19. This change is intentional. When upgrading, the client will recreate the tables. This may take a long time."
        );
        assert_eq!(important_notes("nothing special here"), "");
        // long ones are cut at a sentence
        let long = format!(
            "> [!IMPORTANT]\n> {}",
            "A sentence that goes on. ".repeat(40)
        );
        let cut = important_notes(&long);
        assert!(cut.chars().count() <= 521 && cut.ends_with('.'), "{cut}");
    }
}
