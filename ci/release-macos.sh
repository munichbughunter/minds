#!/bin/sh
#
# Baut die macOS-Archive auf einem Mac und hängt sie an ein bestehendes Release.
#
# Die Brücke, solange kein GitLab-Runner auf einem Mac registriert ist: Die
# Pipeline baut Linux, dieses Skript liefert macOS nach. Namensschema, Layout und
# Ablageort sind identisch mit dem, was `.gitlab-ci.yml` erzeugt — der Installer
# merkt keinen Unterschied.
#
#   MINDS_TOKEN=glpat-… sh ci/release-macos.sh v0.1.0
#
# Der Token braucht den Scope `api` (Package Registry schreiben, Release ändern).
# Sobald ein Mac-Runner mit dem Tag `macos` existiert, setzt du in den CI/CD-
# Variablen MACOS_RUNNER="true" und dieses Skript wird überflüssig.

set -eu

TAG="${1:-}"
[ -n "$TAG" ] || { echo "Aufruf: sh ci/release-macos.sh vX.Y.Z" >&2; exit 1; }
[ -n "${MINDS_TOKEN:-}" ] || { echo "MINDS_TOKEN ist nicht gesetzt." >&2; exit 1; }

VERSION="${TAG#v}"
API="https://gitlab.com/api/v4/projects/pdoering-it%2Fminds"
BASE="${API}/packages/generic/minds/${VERSION}"
TARGETS="aarch64-apple-darwin x86_64-apple-darwin"

[ "$(uname -s)" = "Darwin" ] || { echo "Das hier will auf einem Mac laufen." >&2; exit 1; }

rm -rf dist-macos
mkdir -p dist-macos

for target in $TARGETS; do
  echo "→ baue $target"
  rustup target add "$target" >/dev/null
  cargo build --release --locked --target "$target" --bin minds

  pkg="minds-${VERSION}-${target}"
  mkdir -p "dist-macos/${pkg}"
  cp "target/${target}/release/minds" "dist-macos/${pkg}/"
  cp README.md CHANGELOG.md "dist-macos/${pkg}/"
  tar -czf "dist-macos/${pkg}.tar.gz" -C dist-macos "$pkg"
  rm -rf "dist-macos/${pkg}"
done

echo "→ Prüfsummen"
( cd dist-macos && shasum -a 256 ./*.tar.gz > SHA256SUMS.macos )

# Die von der Pipeline erzeugte SHA256SUMS-Datei holen und um die macOS-Zeilen
# ergänzen, damit der Installer für *alle* Archive verifizieren kann.
echo "→ SHA256SUMS zusammenführen"
if curl -sSfL --header "PRIVATE-TOKEN: ${MINDS_TOKEN}" \
     "${BASE}/SHA256SUMS" -o dist-macos/SHA256SUMS.remote 2>/dev/null; then
  cat dist-macos/SHA256SUMS.remote dist-macos/SHA256SUMS.macos \
    | sed 's|\./||' | sort -u -k2 > dist-macos/SHA256SUMS
else
  echo "  (kein SHA256SUMS im Release — lege eines nur mit den macOS-Archiven an)"
  sed 's|\./||' dist-macos/SHA256SUMS.macos > dist-macos/SHA256SUMS
fi
rm -f dist-macos/SHA256SUMS.macos dist-macos/SHA256SUMS.remote

echo "→ hochladen"
for f in dist-macos/*; do
  name="$(basename "$f")"
  echo "   $name"
  curl --fail --silent --show-error \
    --header "PRIVATE-TOKEN: ${MINDS_TOKEN}" \
    --upload-file "$f" "${BASE}/${name}"
done

echo "→ Asset-Links am Release ergänzen"
for target in $TARGETS; do
  name="minds-${VERSION}-${target}.tar.gz"
  curl --fail --silent --show-error --request POST \
    --header "PRIVATE-TOKEN: ${MINDS_TOKEN}" \
    --data "name=${name}" \
    --data "url=${BASE}/${name}" \
    --data "link_type=package" \
    "${API}/releases/${TAG}/assets/links" >/dev/null \
    || echo "   (Link für ${name} existiert vermutlich schon — übersprungen)"
done

echo
echo "Fertig. macOS-Archive hängen an ${TAG}."
