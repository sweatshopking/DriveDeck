#[path = "build_support/po.rs"]
mod po;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let po_root = manifest.join("po");
    println!("cargo:rerun-if-changed={}", po_root.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("build_support/po.rs").display()
    );
    let mut catalogs = std::collections::BTreeMap::new();
    for language in fs::read_dir(&po_root).expect("couldn't read translation directory") {
        let language = language.expect("couldn't read translation directory entry");
        if !language
            .file_type()
            .expect("couldn't read translation entry type")
            .is_dir()
        {
            continue;
        }
        let locale = language.file_name().to_string_lossy().to_string();
        let catalog = language.path().join("LC_MESSAGES/drivedeck.po");
        println!("cargo:rerun-if-changed={}", catalog.display());
        if !catalog.is_file() {
            continue;
        }
        let text = fs::read_to_string(&catalog)
            .unwrap_or_else(|error| panic!("couldn't read {}: {error}", catalog.display()));
        let messages = po::parse(&text)
            .unwrap_or_else(|error| panic!("couldn't parse {}: {error}", catalog.display()));
        catalogs.insert(locale, messages);
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("translations.rs");
    let languages = catalogs.keys().map(|locale| format!("{locale:?}"));
    let mut generated = format!(
        "pub const AVAILABLE_LANGUAGES: &[&str] = &[{}];\n",
        languages.collect::<Vec<_>>().join(", ")
    );
    generated.push_str(
        "pub fn lookup(language: &str, message: &str) -> Option<&'static str> {\n    match language {\n",
    );
    for (locale, messages) in &catalogs {
        generated.push_str(&format!("        {locale:?} => match message {{\n"));
        for entry in messages {
            if let Some(translation) = entry.translation.as_ref().filter(|value| !value.is_empty())
            {
                generated.push_str(&format!(
                    "            {:?} => Some({:?}),\n",
                    entry.id, translation
                ));
            }
        }
        generated.push_str("            _ => None,\n        },\n");
    }
    generated.push_str("        _ => None,\n    }\n}\n");
    generated.push_str(
        "pub fn lookup_plural(language: &str, message: &str, count: i64) -> Option<&'static str> {\n    match language {\n",
    );
    for (locale, messages) in &catalogs {
        generated.push_str(&format!("        {locale:?} => match message {{\n"));
        for entry in messages {
            if let Some(plural) = &entry.plural {
                let translations: std::collections::BTreeMap<_, _> = entry
                    .plural_translations
                    .iter()
                    .map(|(i, s)| (*i, s))
                    .collect();
                generated.push_str(&format!(
                    "            {:?} => Some(if count == 1 {{ {:?} }} else {{ {:?} }}),\n",
                    entry.id,
                    translations.get(&0).copied().unwrap_or(&entry.id),
                    translations.get(&1).copied().unwrap_or(plural)
                ));
            }
        }
        generated.push_str("            _ => None,\n        },\n");
    }
    generated.push_str("        _ => None,\n    }\n}\n");
    fs::write(out, generated).expect("couldn't write translation lookup table");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("po")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI failed to compile");
}
