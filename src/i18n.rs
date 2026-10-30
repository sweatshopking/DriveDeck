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
                // English is the source text: wanted first, it beats any translation after it
                if candidate == "en" || candidate == "C" || candidate == "POSIX" {
                    return "en";
                }
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
    let language = language();
    lookup_plural_forms(language, singular)
        .and_then(|forms| {
            let i = plural_index(language, count);
            forms.get(i).or(forms.last()).copied()
        })
        .unwrap_or(if count == 1 { singular } else { plural })
        .replace("{n}", &count.to_string())
}

/// Which msgstr[n] a count takes, by each language's own rule (the Plural-Forms the .po
/// headers must state: GNU gettext's standard ones)
fn plural_index(language: &str, n: i64) -> usize {
    let n = n.unsigned_abs();
    let (n10, n100) = (n % 10, n % 100);
    let few = (2..=4).contains(&n10) && !(12..=14).contains(&n100);
    match language.split('_').next().unwrap_or_default() {
        // one form for every count
        "ja" | "zh" | "ko" => 0,
        // 0 counts as one
        "fr" => usize::from(n > 1),
        "pt" if language == "pt_BR" => usize::from(n > 1),
        "pl" => {
            if n == 1 {
                0
            } else if few {
                1
            } else {
                2
            }
        }
        "ru" | "uk" => {
            if n10 == 1 && n100 != 11 {
                0
            } else if few {
                1
            } else {
                2
            }
        }
        "cs" => match n {
            1 => 0,
            2..=4 => 1,
            _ => 2,
        },
        // de, es, it, nl, sv, tr, pt and English
        _ => usize::from(n != 1),
    }
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
    // always an owned String, like the forms with values, so the three can share a match
    ($message:literal) => {
        $crate::i18n::translate($message).to_string()
    };
    ($message:expr) => {
        $crate::i18n::translate("Couldn't find where to save that version.").to_string()
    };
}

#[cfg(test)]
#[path = "../build_support/po.rs"]
mod po;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// How many msgstr[n] forms a language needs (matches plural_index)
    fn nplurals(language: &str) -> usize {
        match language.split('_').next().unwrap_or_default() {
            "ja" | "zh" | "ko" => 1,
            "ru" | "uk" | "pl" | "cs" => 3,
            _ => 2,
        }
    }

    /// A plural form's placeholders other than {n}: a form may show the count or leave it out
    /// ("1 Datei"), so {n} isn't compared; every other one must be there.
    fn plural_placeholders(text: &str) -> Vec<String> {
        placeholders(text)
            .into_iter()
            .filter(|p| p != "n")
            .collect()
    }

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

    /// Problems in every shipped catalog. A string still untranslated (empty) is only a problem
    /// when `complete` is asked for: until a translation round is finished it shows in English.
    fn catalog_problems(complete: bool) -> Vec<String> {
        let mut all = Vec::new();
        assert_eq!(
            AVAILABLE_LANGUAGES,
            [
                "de", "es", "fr", "it", "ja", "ko", "nl", "pt_BR", "ru", "tr", "zh_CN"
            ]
        );
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
                let untranslated = translation.translation.as_deref().is_none_or(str::is_empty)
                    && translation
                        .plural_translations
                        .iter()
                        .all(|(_, form)| form.is_empty());
                if untranslated && !complete {
                    continue;
                }
                if let Some(plural) = source.plural.as_deref() {
                    if translation.plural_translations.len() < nplurals(language) {
                        problems.push(format!("plural forms missing for {:?}", source.id));
                    }
                    for (index, translated) in &translation.plural_translations {
                        let source_form = if *index == 0 { &source.id } else { plural };
                        if translated.is_empty()
                            || plural_placeholders(source_form) != plural_placeholders(translated)
                        {
                            problems.push(format!(
                                "placeholder mismatch in plural form {index} for {:?}",
                                source.id
                            ));
                        }
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
            if !problems.is_empty() {
                all.push(format!(
                    "{language} catalog problems:\n{}",
                    problems.join("\n")
                ));
            }
        }
        all
    }

    #[test]
    fn translations_keep_their_placeholders() {
        let problems = catalog_problems(false);
        assert!(problems.is_empty(), "{}", problems.join("\n\n"));
    }

    // On when the translation round is finished (cargo test -- --ignored); see po/README.md.
    #[test]
    #[ignore = "needs every string translated in every language"]
    fn every_language_is_complete() {
        let problems = catalog_problems(true);
        assert!(problems.is_empty(), "{}", problems.join("\n\n"));
    }

    #[test]
    fn language_selection_uses_environment_precedence_and_locale_fallback() {
        assert_eq!(language_from_values(["ar_EG.UTF-8", "de_DE.UTF-8"]), "de");
        assert_eq!(language_from_values(["fr_FR.UTF-8", "de_DE.UTF-8"]), "fr");
        assert_eq!(language_from_values(["pt_PT", "de"]), "de");
        assert_eq!(language_from_values(["de_DE@euro"]), "de");
        assert_eq!(language_from_values(["ar_EG", "en_US"]), "en");
    }

    #[test]
    fn english_messages_fall_back_to_the_source_text() {
        assert_eq!(translate("untranslated source"), "untranslated source");
    }

    #[test]
    fn generated_german_lookup_includes_plural_forms() {
        assert_eq!(lookup("de", "Accounts"), Some("Konten"));
        assert_eq!(
            lookup_plural_forms("de", "1 account needs attention").map(|f| f[1]),
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

    #[test]
    fn plural_forms_follow_each_language() {
        let at = |lang: &str, ns: &[i64]| {
            ns.iter()
                .map(|&n| plural_index(lang, n))
                .collect::<Vec<_>>()
        };
        assert_eq!(at("de", &[0, 1, 2, 5]), [1, 0, 1, 1]);
        assert_eq!(at("fr", &[0, 1, 2]), [0, 0, 1]);
        assert_eq!(at("pt_BR", &[0, 1, 2]), [0, 0, 1]);
        assert_eq!(at("ja", &[0, 1, 7]), [0, 0, 0]);
        assert_eq!(at("pl", &[1, 2, 5, 12, 22, 25]), [0, 1, 2, 2, 1, 2]);
        assert_eq!(at("ru", &[1, 2, 5, 11, 21, 22, 111]), [0, 1, 2, 2, 0, 1, 2]);
        assert_eq!(at("cs", &[1, 3, 5]), [0, 1, 2]);
    }

    #[test]
    fn english_first_beats_a_later_translation() {
        assert_eq!(language_from_values(["en_US:de"]), "en");
        assert_eq!(language_from_values(["C"]), "en");
        assert_eq!(language_from_values(["de_AT.UTF-8"]), "de");
    }
}
