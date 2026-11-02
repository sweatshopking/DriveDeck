#!/usr/bin/env python3
"""Flags translations that are really English: copied unchanged, or English sentences with a few
words swapped ("Microsoft is bezet juiste now."). The catalog test only checks that every entry
has *something*, so run this too before calling a language done.

usage: scripts/check-po-quality.py [lang ...]     (no languages: every catalog in po/)
Exit status 1 when anything is flagged. Words that are the same in the language (OneDrive,
Status, node_modules...) are allowed below; add to ALLOWED only when that's really true."""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# the same in every language: names, folder names, units, examples
ALLOWED_SAME = {
    "OneDrive", "DriveDeck for OneDrive", "DriveDeck for OneDrive ", "OK", "Online ▾",
    "China (21Vianet)", "node_modules", "target", "venv", ".venv", "build", "dist", ".git",
    "__pycache__", ".gradle", ".idea", ".cache", "Webhooks", "{}: {}", "{} {}", "{} — {}",
    "name@example.com, another@example.com", "MB", "1 MB/s", "2 MB/s", "5 MB/s", "10 MB/s",
    "25 MB/s", "50 MB/s", "Tenant", "Site", "Status", "Type", "Name", "Version ", "Error",
    "Options", "File", "Accounts", "Logging", "Transfers", "In OneDrive: {}",
    "5 minutes", "10 minutes", "15 minutes", "30 minutes", "{} (Microsoft says: {})",
}
# words that only English has (none of them is a word in the shipped languages)
ENGLISH = re.compile(
    r"\b(the|your|you|which|now|just|didn't|isn't|can't|won't|doesn't|their|them|been|would|"
    r"should|please|try|while|though|someone|usually|otherwise|itself|making|having|moving|"
    r"meanwhile|little|reported|asks|keep|delete|stopped|shows|right|syncing|signed|paused|"
    r"starting|sending|cancelling|restoring|uploading|downloading|uploaded|downloaded|deleted|"
    r"renamed|today|yesterday|until|tomorrow|morning|hours?|always|solid|invitations?|sent|"
    r"items|seconds|checks|wait|network|subscription|lasts|application|means|never|cuts|"
    r"large|transfers|out|get|with|from|into|here|after|before|again|when|what|only|not)\b",
    re.I,
)
# ...except where the language has the same word
NATIVE = {
    "de": {"was"},
    "fr": {"items", "invitation", "invitations", "application"},
    "it": {"items"},
}


def entries(text):
    join = lambda s: "".join(re.findall(r'"(.*)"', s))
    for msgid, msgstr in re.findall(r'msgid ((?:".*"\n)+)msgstr ((?:".*"\n)+)', text):
        yield join(msgid), join(msgstr)


def strip_code(s):
    # placeholders, quoted UI names, options, paths and file names aren't prose
    return re.sub(r'\{[^}]*\}|“[^”]*”|\\"[^\\]*\\"|--\S+|\S+[./]\S+', "", s)


def check(lang):
    path = ROOT / "po" / lang / "LC_MESSAGES" / "drivedeck.po"
    problems = []
    for msgid, msgstr in entries(path.read_text(encoding="utf-8")):
        if not msgid or not msgstr:
            continue
        if msgstr == msgid and msgid not in ALLOWED_SAME and re.search(r"[A-Za-z]{3}", msgid):
            problems.append(("left in English", msgid))
            continue
        words = {w.lower() for w in ENGLISH.findall(strip_code(msgstr))}
        words -= NATIVE.get(lang, set())
        # English words that are also in the English source are the tell-tale of a word swap
        words &= {w.lower() for w in re.findall(r"[A-Za-z']+", msgid)}
        if words and msgstr != msgid:
            problems.append(("English words (%s)" % ", ".join(sorted(words)), msgid))
    return problems


def main():
    langs = sys.argv[1:] or sorted(
        p.name for p in (ROOT / "po").iterdir() if (p / "LC_MESSAGES").is_dir()
    )
    bad = 0
    for lang in langs:
        problems = check(lang)
        bad += len(problems)
        print(f"{lang}: {len(problems)} to fix")
        for why, msgid in problems:
            print(f"    [{why}] {msgid[:100]}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
