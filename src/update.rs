//! Telling the user when a newer version is out: a once-a-day look at the project's latest
//! release on GitHub. Nothing is downloaded or installed; the user gets a link to the release.
//! Switched off with "Tell me when a new version is out" in App settings.

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
    #[cfg(feature = "test-driver")]
    let url = std::env::var("LODG_TEST_RELEASES").unwrap_or_else(|_| API_URL.into());
    #[cfg(not(feature = "test-driver"))]
    let url = API_URL.to_string();
    latest_release(&url)
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

#[cfg(test)]
mod tests {
    use super::*;

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
