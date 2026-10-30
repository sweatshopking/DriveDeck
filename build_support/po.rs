#[derive(Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub plural: Option<String>,
    pub translation: Option<String>,
    pub plural_translations: Vec<(usize, String)>,
    pub fuzzy: bool,
}

pub fn parse(input: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut current = Entry::default();
    let mut active = None;
    for (line_number, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            if !current.id.is_empty() || current.translation.is_some() {
                entries.push(std::mem::take(&mut current));
            }
            active = None;
            continue;
        }
        if line.starts_with('#') {
            if line.starts_with("#,")
                && line
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .any(|f| f == "fuzzy")
            {
                current.fuzzy = true;
            }
            continue;
        }
        let (field, value) = if let Some(value) = line.strip_prefix("msgid_plural ") {
            ("msgid_plural", value)
        } else if let Some(value) = line.strip_prefix("msgid ") {
            ("msgid", value)
        } else if let Some(value) = line.strip_prefix("msgstr[") {
            let (index, value) = value
                .split_once("] ")
                .ok_or_else(|| format!("invalid plural translation on line {}", line_number + 1))?;
            let index = index
                .parse::<usize>()
                .map_err(|_| format!("invalid plural index on line {}", line_number + 1))?;
            let value = parse_quoted(value, line_number + 1)?;
            current.plural_translations.push((index, value));
            active = Some(Active::Plural(index));
            continue;
        } else if let Some(value) = line.strip_prefix("msgstr ") {
            ("msgstr", value)
        } else if line.starts_with('"') {
            let value = parse_quoted(line, line_number + 1)?;
            append(&mut current, active, value)?;
            continue;
        } else {
            return Err(format!(
                "unsupported PO directive on line {}",
                line_number + 1
            ));
        };
        let value = parse_quoted(value, line_number + 1)?;
        active = match field {
            "msgid" => {
                current.id = value;
                Some(Active::Id)
            }
            "msgid_plural" => {
                current.plural = Some(value);
                Some(Active::PluralId)
            }
            "msgstr" => {
                current.translation = Some(value);
                Some(Active::Translation)
            }
            _ => unreachable!(),
        };
    }
    if !current.id.is_empty() || current.translation.is_some() {
        entries.push(current);
    }
    Ok(entries)
}

#[derive(Clone, Copy)]
enum Active {
    Id,
    PluralId,
    Translation,
    Plural(usize),
}

fn append(entry: &mut Entry, active: Option<Active>, value: String) -> Result<(), String> {
    match active {
        Some(Active::Id) => entry.id.push_str(&value),
        Some(Active::PluralId) => entry.plural.get_or_insert_default().push_str(&value),
        Some(Active::Translation) => entry.translation.get_or_insert_default().push_str(&value),
        Some(Active::Plural(index)) => {
            let translation = entry
                .plural_translations
                .iter_mut()
                .find(|(found, _)| *found == index)
                .ok_or_else(|| format!("missing plural translation index {index}"))?;
            translation.1.push_str(&value);
        }
        None => return Err("continued string has no preceding PO field".into()),
    }
    Ok(())
}

fn parse_quoted(input: &str, line_number: usize) -> Result<String, String> {
    let input = input
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| format!("expected quoted string on line {line_number}"))?;
    let mut output = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut encoded = [0; 4];
            output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| format!("unfinished escape on line {line_number}"))?;
        match escaped {
            'a' => output.push(b'\x07'),
            'n' => output.push(b'\n'),
            'r' => output.push(b'\r'),
            't' => output.push(b'\t'),
            'b' => output.push(b'\x08'),
            'f' => output.push(b'\x0c'),
            'v' => output.push(b'\x0b'),
            '"' => output.push(b'"'),
            '\\' => output.push(b'\\'),
            'x' => {
                let mut digits = String::new();
                while chars.peek().is_some_and(|digit| digit.is_ascii_hexdigit()) {
                    digits.push(chars.next().unwrap());
                }
                let value = u8::from_str_radix(&digits, 16)
                    .map_err(|_| format!("invalid hex escape on line {line_number}"))?;
                output.push(value);
            }
            '0'..='7' => {
                let mut digits = String::from(escaped);
                for _ in 0..2 {
                    if chars.peek().is_some_and(|digit| matches!(digit, '0'..='7')) {
                        digits.push(chars.next().unwrap());
                    }
                }
                let value = u8::from_str_radix(&digits, 8)
                    .map_err(|_| format!("invalid octal escape on line {line_number}"))?;
                output.push(value);
            }
            other => {
                return Err(format!(
                    "unsupported escape \\{other} on line {line_number}"
                ));
            }
        }
    }
    String::from_utf8(output)
        .map_err(|error| format!("invalid UTF-8 string on line {line_number}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_po_strings_and_plural_forms() {
        let entries = parse(
            "msgid \"one \\\"thing\\\"\"\nmsgid_plural \"many things\"\nmsgstr[0] \"ein Ding\"\nmsgstr[1] \"viele Dinge\"\n",
        )
        .unwrap();
        assert_eq!(
            entries,
            [Entry {
                id: "one \"thing\"".into(),
                plural: Some("many things".into()),
                translation: None,
                plural_translations: vec![(0, "ein Ding".into()), (1, "viele Dinge".into())],
                fuzzy: false,
            }]
        );
    }

    #[test]
    fn marks_fuzzy_entries() {
        let entries =
            parse("#, fuzzy\nmsgid \"old\"\nmsgstr \"alt\"\n\nmsgid \"fine\"\nmsgstr \"gut\"\n")
                .unwrap();
        assert!(entries[0].fuzzy);
        assert!(!entries[1].fuzzy);
    }

    #[test]
    fn reads_multiline_po_strings_and_escapes() {
        let entries =
            parse("msgid \"first\\n\"\n\"second\"\nmsgstr \"erste\\n\"\n\"zweite\"\n").unwrap();
        assert_eq!(entries[0].id, "first\nsecond");
        assert_eq!(entries[0].translation.as_deref(), Some("erste\nzweite"));
    }

    #[test]
    fn reads_octal_and_hex_utf8_escapes() {
        let entries = parse("msgid \"\\303\\244\"\nmsgstr \"\\xc3\\xa4\"\n").unwrap();
        assert_eq!(entries[0].id, "ä");
        assert_eq!(entries[0].translation.as_deref(), Some("ä"));
    }
}
