#!/usr/bin/env bash
# Rebuilds the UI fonts in crates/observation-client/assets/fonts/ from the
# upstream releases: Inter 4.1 with tabular digits frozen into the default
# glyphs (egui cannot switch OpenType features on), and Iosevka Term 33.3.3.
# Both are cut down to Latin-1 plus the punctuation and symbols the UI uses.
# Needs python3 (with venv) and network access to github.com.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${root}/crates/observation-client/assets/fonts"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

inter=4.1
iosevka=33.3.3
unicodes="U+0020-007E,U+00A0-00FF,U+0131,U+0152-0153,U+2013-2014,U+2018-201E,U+2022,U+2026,U+2032-2033,U+2039-203A,U+20AC,U+2122,U+2190-2193,U+2212,U+2248,U+2264-2265,U+25B2,U+25B6,U+25BC,U+25C0,U+2713,U+2715"

python3 -m venv "${work}/venv"
"${work}/venv/bin/pip" install -q fonttools opentype-feature-freezer
curl -fsSL -o "${work}/inter.zip" "https://github.com/rsms/inter/releases/download/v${inter}/Inter-${inter}.zip"
curl -fsSL -o "${work}/iosevka.zip" "https://github.com/be5invis/Iosevka/releases/download/v${iosevka}/PkgTTF-IosevkaTerm-${iosevka}.zip"
unzip -q -o "${work}/inter.zip" extras/ttf/Inter-Regular.ttf extras/ttf/Inter-SemiBold.ttf LICENSE.txt -d "${work}/inter"
unzip -q -o "${work}/iosevka.zip" IosevkaTerm-Regular.ttf IosevkaTerm-SemiBold.ttf -d "${work}/iosevka"

for w in Regular SemiBold; do
  "${work}/venv/bin/pyftfeatfreeze" -f tnum -S -U " Tnum" \
    "${work}/inter/extras/ttf/Inter-${w}.ttf" "${work}/Inter-${w}-tnum.ttf" >/dev/null
  "${work}/venv/bin/pyftsubset" "${work}/Inter-${w}-tnum.ttf" --unicodes="${unicodes}" \
    --layout-features=kern --no-hinting --desubroutinize --output-file="${out}/Inter-${w}.ttf"
  "${work}/venv/bin/pyftsubset" "${work}/iosevka/IosevkaTerm-${w}.ttf" --unicodes="${unicodes}" \
    --layout-features=kern --no-hinting --output-file="${out}/IosevkaTerm-${w}.ttf"
done
cp "${work}/inter/LICENSE.txt" "${out}/OFL-Inter.txt"
curl -fsSL -o "${out}/OFL-Iosevka.md" "https://raw.githubusercontent.com/be5invis/Iosevka/v${iosevka}/LICENSE.md"
ls -l "${out}"
