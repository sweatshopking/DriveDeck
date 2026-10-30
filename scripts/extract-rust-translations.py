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


def main():
    pot_path = Path(sys.argv[1])
    ids = read_ids(pot_path)
    additions = []
    call = re.compile(
        r'(?<![A-Za-z0-9_])tr!\(\s*("(?:\\.|[^"\\])*")'
        r'(?:\s*\|\s*("(?:\\.|[^"\\])*")\s*%)?',
        re.DOTALL,
    )
    for source in sorted(Path("src").rglob("*.rs")):
        for singular, plural in call.findall(source.read_text()):
            singular = quoted(singular)
            plural = quoted(plural) if plural else None
            if singular not in ids:
                additions.append((singular, plural))
                ids.add(singular)
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
