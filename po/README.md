# Translations

The app bundles its `.po` catalogs, so translations work in AppImage, deb, and rpm installs
without separate runtime files. Slint 1.18 expects them at
`po/<language>/LC_MESSAGES/drivedeck.po`; `drivedeck` is the package name in `Cargo.toml`.

## Updating the catalog

Install the matching Slint extractor once, then run:

```sh
cargo install slint-tr-extractor --version 1.18.1
sh scripts/update-translations.sh
```

The extractor reads annotated strings from `ui/app.slint`. The script adds strings used with
Rust's `tr!` macro. Review `po/drivedeck.pot`, then merge it into every language catalog with `msgmerge`. Translate
new entries, keeping every placeholder unchanged. The unit tests check catalog coverage and
placeholders.

## Adding a language

Copy the catalog to `po/<language>/LC_MESSAGES/drivedeck.po`, set its `Language` and
`Plural-Forms` headers, and translate each entry. Locale selection checks `LANGUAGE`,
`LC_ALL`, `LC_MESSAGES`, then `LANG`; regional locales fall back to their base language and
then English. Add the language's translated `Name` and `Comment` fields to the desktop files.
The bundled catalogs cover German, Spanish, French, Italian, Brazilian Portuguese, Dutch, Polish,
Japanese, Simplified Chinese, Russian, Turkish, Korean, Czech, Ukrainian, and Swedish. Their
translations were drafted by machine; corrections are welcome.

Do not translate log messages, quoted messages from the OneDrive client, configuration keys,
or command-line help.
