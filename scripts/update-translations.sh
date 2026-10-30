#!/bin/sh
set -eu

slint-tr-extractor --no-default-translation-context -o po/drivedeck.pot ui/app.slint
python3 scripts/extract-rust-translations.py po/drivedeck.pot
