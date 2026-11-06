//! Search provider for the GNOME Shell overview.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{OwnedValue, Value};

use crate::core::{self, AppSettings};
use crate::doctor::percent_encode;
use crate::runner::{self, Match};

const SERVICE: &str = "io.github.sweatshopking.DriveDeck.GnomeSearch";
const PATH: &str = "/io/github/sweatshopking/DriveDeck/GnomeSearch";
const PROVIDER_FILE: &str =
    "/usr/share/gnome-shell/search-providers/io.github.sweatshopking.DriveDeck.search-provider.ini";

fn provider_file_exists_at(path: &Path) -> bool {
    path.is_file()
}

pub fn provider_file_exists() -> bool {
    provider_file_exists_at(Path::new(PROVIDER_FILE))
}

fn text_value(s: impl Into<String>) -> OwnedValue {
    Value::from(s.into())
        .try_to_owned()
        .unwrap_or_else(|_| OwnedValue::from(false))
}

fn terms_text(terms: &[String]) -> String {
    terms.join(" ")
}

fn office_url(terms: &[String]) -> String {
    format!(
        "https://www.office.com/search?q={}",
        percent_encode(&terms_text(terms))
    )
}

fn activation_allowed(identifier: &str, sync_dirs: &[PathBuf]) -> bool {
    runner::open_for(identifier, "", sync_dirs).is_some()
}

fn meta_for(id: &str, cached: Option<&Meta>) -> HashMap<String, OwnedValue> {
    let mut meta = HashMap::new();
    let (name, description, icon) = cached
        .map(|m| (m.name.clone(), m.description.clone(), m.icon.clone()))
        .unwrap_or_else(|| {
            let path = id
                .split_once('\n')
                .and_then(|(_, p)| (!p.is_empty()).then_some(Path::new(p)));
            let name = path
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.to_string());
            let description = path
                .and_then(Path::parent)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let icon = runner::icon_for(&name, path.is_some_and(Path::is_dir)).to_string();
            (name, description, icon)
        });
    meta.insert("id".into(), text_value(id));
    meta.insert("name".into(), text_value(name));
    meta.insert("description".into(), text_value(description));
    meta.insert("gicon".into(), text_value(icon));
    meta
}

#[derive(Clone)]
struct Meta {
    name: String,
    description: String,
    icon: String,
}

fn match_meta(item: &Match) -> (String, Meta) {
    let (id, name, icon, _, _, props) = item;
    let description = props
        .get("subtext")
        .and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    (
        id.clone(),
        Meta {
            name: name.clone(),
            description,
            icon: icon.clone(),
        },
    )
}

struct GnomeSearch {
    metas: Arc<Mutex<HashMap<String, Meta>>>,
    /// Counts searches as GNOME asks for them; a worker that finds a newer number is stale
    newest: Arc<AtomicU64>,
}

impl GnomeSearch {
    /// The search runs on its own thread so the D-Bus thread stays free (GNOME asks again after
    /// every keystroke); a search that a newer one has replaced answers with nothing.
    async fn results(&self, terms: Vec<String>) -> Vec<String> {
        let mine = self.newest.fetch_add(1, Ordering::SeqCst) + 1;
        let (newest, metas) = (self.newest.clone(), self.metas.clone());
        let (tx, rx) = runner::hand_over();
        std::thread::spawn(move || {
            std::thread::sleep(runner::DEBOUNCE);
            let current = |n: &AtomicU64| n.load(Ordering::SeqCst) == mine;
            let query = terms_text(&terms);
            let mut ids = Vec::new();
            if current(&newest) && runner::worth_asking(&query) {
                let found = runner::search_all(&query);
                let mut metas = metas.lock().unwrap_or_else(|e| e.into_inner());
                // only the newest search's results are ever asked about
                metas.clear();
                ids = found
                    .iter()
                    .map(|item| {
                        let (id, meta) = match_meta(item);
                        metas.insert(id.clone(), meta);
                        id
                    })
                    .collect();
            }
            tx.send(if current(&newest) { ids } else { Vec::new() });
        });
        rx.await
    }
}

#[zbus::interface(name = "org.gnome.Shell.SearchProvider2")]
impl GnomeSearch {
    async fn get_initial_result_set(&self, terms: Vec<String>) -> Vec<String> {
        self.results(terms).await
    }

    async fn get_subsearch_result_set(
        &self,
        _previous_results: Vec<String>,
        terms: Vec<String>,
    ) -> Vec<String> {
        self.results(terms).await
    }

    fn get_result_metas(&self, identifiers: Vec<String>) -> Vec<HashMap<String, OwnedValue>> {
        let metas = self.metas.lock().unwrap_or_else(|e| e.into_inner());
        identifiers
            .iter()
            .map(|id| meta_for(id, metas.get(id)))
            .collect()
    }

    fn activate_result(&self, identifier: String, _terms: Vec<String>, _timestamp: u32) {
        // may look up an address online: not on the D-Bus thread
        std::thread::spawn(move || {
            let dirs: Vec<PathBuf> = crate::core::discover(&AppSettings::load())
                .into_iter()
                .map(|a| a.sync_dir)
                .collect();
            if activation_allowed(&identifier, &dirs) {
                runner::run(&identifier, "");
            }
        });
    }

    fn launch_search(&self, terms: Vec<String>, _timestamp: u32) {
        let url = office_url(&terms);
        core::run_detached("xdg-open", &[&url]);
    }
}

fn serve() -> Option<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()
        .ok()?
        .name(SERVICE)
        .ok()?
        .serve_at(
            PATH,
            GnomeSearch {
                metas: Arc::new(Mutex::new(HashMap::new())),
                newest: Arc::new(AtomicU64::new(0)),
            },
        )
        .ok()?
        .build()
        .ok()
}

pub fn start() {
    std::thread::spawn(|| {
        let mut service = None;
        loop {
            let want = AppSettings::load().gnome_search == Some(true) && provider_file_exists();
            if want && service.is_none() {
                service = serve();
            } else if !want {
                service = None;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

pub fn test_service() {
    let _connection = serve();
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_are_joined_with_spaces() {
        assert_eq!(terms_text(&["my".into(), "report".into()]), "my report");
    }

    #[test]
    fn office_search_url_encodes_terms() {
        assert_eq!(
            office_url(&["my".into(), "report".into()]),
            "https://www.office.com/search?q=my%20report"
        );
    }

    #[test]
    fn metadata_has_all_keys_for_cached_file_folder_and_web_hits() {
        let cached = Meta {
            name: "notes.txt".into(),
            description: "Work · Docs".into(),
            icon: "text-x-generic".into(),
        };
        for id in [
            "file\n/tmp/notes.txt",
            "folder\n/tmp/Docs",
            "https://example.test/x",
        ] {
            let meta = meta_for(id, Some(&cached));
            for key in ["id", "name", "description", "gicon"] {
                assert!(meta.contains_key(key), "{key} missing for {id}");
            }
        }
    }

    #[test]
    fn provider_file_decision_matches_the_installed_file() {
        let dir =
            std::env::temp_dir().join(format!("drivedeck-gnome-provider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("provider test directory");
        assert!(!provider_file_exists_at(&dir.join("missing.ini")));
        let file = dir.join("provider.ini");
        std::fs::write(&file, "provider").expect("provider test file");
        assert!(provider_file_exists_at(&file));
        std::fs::remove_dir_all(dir).expect("provider test cleanup");
    }

    #[test]
    fn activation_rejects_non_https_and_outside_paths() {
        let dir = std::env::temp_dir().join(format!("drivedeck-gnome-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("open test directory");
        let inside = dir.join("notes.txt");
        std::fs::write(&inside, "notes").expect("open test file");
        let dirs = vec![dir.clone()];
        assert!(activation_allowed(
            &format!("\n{}", inside.display()),
            &dirs
        ));
        assert!(!activation_allowed("http://example.test", &dirs));
        assert!(!activation_allowed("file:///etc/passwd", &dirs));
        assert!(!activation_allowed(
            &format!("\n{}", dir.join("../outside.txt").display()),
            &dirs
        ));
        std::fs::remove_dir_all(dir).expect("open test cleanup");
    }
}
