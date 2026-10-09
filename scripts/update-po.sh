#!/usr/bin/env bash
# Extrae las cadenas traducibles y actualiza po/*.po.
set -euo pipefail
cd "$(dirname "$0")/.."
# xgettext no conoce Rust; como C reconoce bien las llamadas tr("…"), trf("…") y n_("…").
find src -name '*.rs' | sort | xgettext --language=C --from-code=UTF-8 --keyword=tr --keyword=trf --keyword=n_ \
    --add-comments=TRANSLATORS --package-name=gnome-personal-assistant --files-from=- -o po/messages.pot
for po in po/*.po; do
    msgmerge --quiet --update --backup=none "$po" po/messages.pot
done
msgfmt --check --statistics -o /dev/null po/en.po
