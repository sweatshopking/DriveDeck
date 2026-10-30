#!/usr/bin/env python3
import ast
import json
import re
import sys
from pathlib import Path


def quoted(text):
    return ast.literal_eval(text)


def read_ids(path):
    ids = set()
    field = None
    value = ""
    for line in path.read_text().splitlines():
        if not line:
            if field == "msgid":
                ids.add(value)
            field = None
            value = ""
        elif line.startswith("msgid "):
            field = "msgid"
            value = quoted(line[6:])
        elif line.startswith("msgid_plural "):
            field = "plural"
        elif line.startswith('"') and field:
            if field == "msgid":
                value += quoted(line)
    if field == "msgid":
        ids.add(value)
    return ids


def call_arguments(source, start):
    closers = [")"]
    quoted = False
    escaped = False
    begin = start
    arguments = []
    for index in range(start, len(source)):
        character = source[index]
        if quoted:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quoted = False
            continue
        if character == '"':
            quoted = True
        elif character in "([{":
            closers.append({"(": ")", "[": "]", "{": "}"}[character])
        elif character in ")]}":
            if closers[-1] != character:
                continue
            closers.pop()
            if not closers:
                arguments.append(source[begin:index].strip())
                return arguments
        elif character == "," and len(closers) == 1:
            arguments.append(source[begin:index].strip())
            begin = index + 1
    return []


def advanced_messages(path):
    source = path.read_text().split("#[cfg(test)]", 1)[0]
    found = set()

    def literal(expression):
        expression = expression.strip()
        if re.fullmatch(r'"(?:\\.|[^"\\])*"', expression):
            found.add(quoted(expression))

    for match in re.finditer(r"\b(bool_opt|num)\s*\(", source):
        arguments = call_arguments(source, match.end())
        indexes = (1, 2, 3) if match.group(1) == "bool_opt" else (1, 2, 3, 7)
        for index in indexes:
            if index < len(arguments):
                literal(arguments[index])

    for field in ("group", "label", "hint", "unit"):
        for value in re.findall(rf"\b{field}\s*:\s*(\"(?:\\.|[^\"\\])*\")", source):
            literal(value)

    for _, label in re.findall(
        r"\(\s*(\"(?:\\.|[^\"\\])*\"),\s*(\"(?:\\.|[^\"\\])*\")\s*\)",
        source,
    ):
        literal(label)
    return found


def backup_messages(path):
    source = path.read_text().split("#[cfg(test)]", 1)[0]
    found = set()
    for spec in re.findall(r"\bKnown\s*\{([^}]*)\}", source, re.DOTALL):
        label = re.search(r'\blabel\s*:\s*("(?:\\.|[^"\\])*")', spec)
        if label:
            found.add(quoted(label.group(1)))
    return found


def main():
    pot_path = Path(sys.argv[1])
    ids = read_ids(pot_path)
    additions = []
    call = re.compile(r"(?<![A-Za-z0-9_])tr!\s*\(")
    for source in sorted(Path("src").rglob("*.rs")):
        text = source.read_text().split("#[cfg(test)]", 1)[0]
        for match in call.finditer(text):
            arguments = call_arguments(text, match.end())
            if not arguments:
                continue
            expression = arguments[0]
            literal = re.match(r'^("(?:\\.|[^"\\])*")', expression)
            if literal:
                singular = quoted(literal.group(1))
                plural_match = re.match(
                    r'^\s*\|\s*("(?:\\.|[^"\\])*")\s*%',
                    expression[literal.end() :],
                )
                plural = quoted(plural_match.group(1)) if plural_match else None
                messages = [(singular, plural)]
            else:
                messages = [
                    (quoted(value), None)
                    for value in re.findall(r'"(?:\\.|[^"\\])*"', expression)
                ]
            for singular, plural in messages:
                if singular and singular not in ids:
                    additions.append((singular, plural))
                    ids.add(singular)
    for message in sorted(advanced_messages(Path("src/advanced.rs"))):
        if message and message not in ids:
            additions.append((message, None))
            ids.add(message)
    for message in sorted(backup_messages(Path("src/backup.rs"))):
        if message and message not in ids:
            additions.append((message, None))
            ids.add(message)
    if additions:
        with pot_path.open("a") as pot:
            for singular, plural in additions:
                pot.write(f"\nmsgid {json.dumps(singular, ensure_ascii=False)}\n")
                if plural:
                    pot.write(
                        f"msgid_plural {json.dumps(plural, ensure_ascii=False)}\n"
                        'msgstr[0] ""\nmsgstr[1] ""\n'
                    )
                else:
                    pot.write('msgstr ""\n')


if __name__ == "__main__":
    main()
