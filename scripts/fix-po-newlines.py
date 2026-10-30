#!/usr/bin/env python3
"""Repairs .po files where a msgstr runs over several raw lines instead of using \\n.

Usage: fix-po-newlines.py file.po [...]   (rewrites the files; prints how many it fixed)
"""
import sys


def ends_closed(line: str) -> bool:
    if not line.endswith('"') or len(line) < 1:
        return False
    # an odd number of backslashes before the final quote means it is escaped
    body = line[:-1]
    slashes = len(body) - len(body.rstrip("\\"))
    return slashes % 2 == 0


def fix(text: str) -> tuple[str, int]:
    lines = text.split("\n")
    out, i, fixed = [], 0, 0
    while i < len(lines):
        line = lines[i]
        starts = line.startswith('msgstr "') or line.startswith('msgstr[') and '] "' in line
        # a complete one-line string closes itself
        if starts and not ends_closed(line):
            parts = [line]
            i += 1
            while i < len(lines) and not ends_closed(lines[i]):
                parts.append(lines[i])
                i += 1
            if i < len(lines):
                parts.append(lines[i])
                i += 1
            # the pieces were one string whose line breaks were typed raw: put \n back
            joined = "\\n".join(parts)
            out.append(joined)
            fixed += 1
            continue
        out.append(line)
        i += 1
    return "\n".join(out), fixed


for path in sys.argv[1:]:
    text = open(path, encoding="utf8").read()
    new, n = fix(text)
    open(path, "w", encoding="utf8").write(new)
    print(f"{path}: fixed {n}")
