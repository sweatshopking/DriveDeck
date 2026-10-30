include!(concat!(env!("OUT_DIR"), "/translations.rs"));

pub fn language() -> &'static str {
    let values = [
        std::env::var("LANGUAGE").ok(),
        std::env::var("LC_ALL").ok(),
        std::env::var("LC_MESSAGES").ok(),
        std::env::var("LANG").ok(),
    ];
    language_from_values(values.iter().filter_map(Option::as_deref))
}

fn language_from_values<'a>(values: impl IntoIterator<Item = &'a str>) -> &'static str {
    for value in values {
        for preference in value.split(':') {
            let locale = preference.split(['.', '@']).next().unwrap_or_default();
            let mut candidate = locale.replace('-', "_");
            loop {
                if AVAILABLE_LANGUAGES.contains(&candidate.as_str()) {
                    return AVAILABLE_LANGUAGES
                        .iter()
                        .copied()
                        .find(|language| *language == candidate)
                        .unwrap_or("en");
                }
                let Some(separator) = candidate.rfind('_') else {
                    break;
                };
                candidate.truncate(separator);
            }
        }
    }
    "en"
}

pub fn select_bundled() {
    let language = language();
    if let Err(error) = slint::select_bundled_translation(language) {
        eprintln!("Couldn't select the {language} translation: {error}");
    }
}

pub fn translate(message: &str) -> &str {
    lookup(language(), message).unwrap_or(message)
}

pub fn translate_plural(singular: &str, plural: &str, count: i64) -> String {
    lookup_plural(language(), singular, count)
        .unwrap_or(if count == 1 { singular } else { plural })
        .replace("{n}", &count.to_string())
}

pub fn format_plural(
    singular: &str,
    plural: &str,
    count: i64,
    values: &[&dyn std::fmt::Display],
) -> String {
    format(&translate_plural(singular, plural, count), values)
}

pub fn format(message: &str, values: &[&dyn std::fmt::Display]) -> String {
    let template = translate(message);
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    let mut next_value = 0;
    while let Some(character) = chars.next() {
        if character == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                output.push('{');
                continue;
            }
            let mut field = String::new();
            while chars.peek().is_some_and(|c| *c != '}') {
                field.push(chars.next().unwrap());
            }
            if chars.peek() == Some(&'}') {
                chars.next();
                let index = if field.is_empty() {
                    let index = next_value;
                    next_value += 1;
                    Some(index)
                } else {
                    field.parse::<usize>().ok()
                };
                if let Some(value) = index.and_then(|index| values.get(index)) {
                    output.push_str(&value.to_string());
                } else {
                    output.push('{');
                    output.push_str(&field);
                    output.push('}');
                }
                continue;
            }
            output.push('{');
            output.push_str(&field);
            continue;
        }
        if character == '}' && chars.peek() == Some(&'}') {
            chars.next();
            output.push('}');
            continue;
        }
        output.push(character);
    }
    output
}

#[macro_export]
macro_rules! tr {
    ($singular:literal | $plural:literal % $count:expr, $($value:expr),+ $(,)?) => {
        $crate::i18n::format_plural(
            $singular,
            $plural,
            $count as i64,
            &[$(&$value as &dyn std::fmt::Display),*],
        )
    };
    ($singular:literal | $plural:literal % $count:expr) => {
        $crate::i18n::translate_plural($singular, $plural, $count as i64)
    };
    ($message:literal, $($value:expr),+ $(,)?) => {
        $crate::i18n::format($message, &[$(&$value as &dyn std::fmt::Display),*])
    };
    ($message:literal) => {
        $crate::i18n::translate($message)
    };
}

#[cfg(test)]
#[path = "../build_support/po.rs"]
mod po;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn placeholders(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let after_open = &rest[open + 1..];
            let Some(close) = after_open.find('}') else {
                break;
            };
            let placeholder = &after_open[..close];
            if !placeholder.is_empty() || rest.as_bytes().get(open + 1) == Some(&b'}') {
                found.push(placeholder.to_string());
            }
            rest = &after_open[close + 1..];
        }
        found.sort();
        found
    }

    #[test]
    fn every_translation_covers_the_catalog_and_keeps_placeholders() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let sources =
            po::parse(&std::fs::read_to_string(root.join("po/drivedeck.pot")).unwrap()).unwrap();
        for language in AVAILABLE_LANGUAGES {
            let path = root
                .join("po")
                .join(language)
                .join("LC_MESSAGES/drivedeck.po");
            let catalog = po::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
            let translations: BTreeMap<_, _> = catalog
                .iter()
                .map(|entry| (entry.id.as_str(), entry))
                .collect();
            let mut problems = Vec::new();
            for source in sources.iter().filter(|entry| !entry.id.is_empty()) {
                let Some(translation) = translations.get(source.id.as_str()) else {
                    problems.push(format!("missing {:?}", source.id));
                    continue;
                };
                if let Some(plural) = source.plural.as_deref() {
                    let singular = translation
                        .plural_translations
                        .iter()
                        .find(|(index, _)| *index == 0)
                        .map(|(_, text)| text.as_str());
                    match singular {
                        Some(translated)
                            if !translated.is_empty()
                                && placeholders(&source.id) == placeholders(translated) => {}
                        _ => problems
                            .push(format!("singular translation missing for {:?}", source.id)),
                    }
                    let translated_plural = translation
                        .plural_translations
                        .iter()
                        .find(|(index, _)| *index == 1)
                        .map(|(_, text)| text.as_str());
                    match translated_plural {
                        Some(translated)
                            if !translated.is_empty()
                                && placeholders(plural) == placeholders(translated) => {}
                        _ => problems.push(format!("plural translation missing for {plural:?}")),
                    }
                } else {
                    match translation.translation.as_deref() {
                        Some(translated)
                            if !translated.is_empty()
                                && placeholders(&source.id) == placeholders(translated) => {}
                        Some(_) => {
                            problems.push(format!("placeholder mismatch for {:?}", source.id))
                        }
                        _ => problems.push(format!("no translation for {:?}", source.id)),
                    }
                }
            }
            assert!(
                problems.is_empty(),
                "{language} catalog problems:\n{}",
                problems.join("\n")
            );
        }
    }

    #[test]
    fn language_selection_uses_environment_precedence_and_locale_fallback() {
        assert_eq!(language_from_values(["fr_FR.UTF-8", "de_DE.UTF-8"]), "de");
        assert_eq!(language_from_values(["de_DE@euro"]), "de");
        assert_eq!(language_from_values(["fr_FR", "en_US"]), "en");
    }

    #[test]
    fn english_messages_fall_back_to_the_source_text() {
        assert_eq!(translate("untranslated source"), "untranslated source");
    }

    #[test]
    fn generated_german_lookup_includes_plural_forms() {
        assert_eq!(lookup("de", "Accounts"), Some("Konten"));
        assert_eq!(
            lookup_plural("de", "1 account needs attention", 3),
            Some("{n} Konten benötigen Aufmerksamkeit")
        );
    }

    #[test]
    fn formatting_keeps_positional_and_escaped_braces() {
        assert_eq!(
            format("{{{} }} {1}", &[&"first", &"second"]),
            "{first } second"
        );
        assert_eq!(translate_plural("1 file", "{n} files", 4), "4 files");
    }
}
