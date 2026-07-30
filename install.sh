#!/bin/sh
#
# Installiert `minds` nach ~/.local/bin.
#
#   curl -sSfL https://raw.githubusercontent.com/munichbughunter/minds/main/install.sh | sh
#
# Umgebungsvariablen:
#   MINDS_VERSION       bestimmte Version statt der neuesten (z. B. 0.1.0)
#   MINDS_INSTALL_DIR   Zielverzeichnis (Default ~/.local/bin)
#   MINDS_TOKEN         GitHub-Token; für das öffentliche Repo nicht nötig, hilft
#                       aber gegen das Rate-Limit der API (60 Anfragen/Stunde je IP)
#
# Bewusst POSIX-sh und ohne jq: Das Skript soll auch auf einer nackten Kiste
# laufen, auf der außer curl und tar nichts installiert ist.

set -eu

REPO="munichbughunter/minds"
API="https://api.github.com/repos/${REPO}"
INSTALL_DIR="${MINDS_INSTALL_DIR:-$HOME/.local/bin}"

say()  { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die()  { printf 'Fehler: %s\n' "$*" >&2; exit 1; }

need() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' wird gebraucht, ist aber nicht installiert."
}

need curl
need tar

# --- Authentifizierung (optional, siehe MINDS_TOKEN oben) -------------------
AUTH=""
if [ -n "${MINDS_TOKEN:-}" ]; then
  AUTH="Authorization: Bearer ${MINDS_TOKEN}"
fi

fetch() { # fetch <url> <ziel|-]
  if [ -n "$AUTH" ]; then
    curl -sSfL --header "$AUTH" "$1" -o "$2"
  else
    curl -sSfL "$1" -o "$2"
  fi
}

# --- Plattform bestimmen ----------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin) ;;
  Linux)  ;;
  *) die "Nicht unterstütztes Betriebssystem: $os. Für Windows liegt das Archiv im Release." ;;
esac

case "$os/$arch" in
  Darwin/arm64)         target="aarch64-apple-darwin" ;;
  Darwin/x86_64)        target="x86_64-apple-darwin" ;;
  Linux/x86_64)         target="x86_64-unknown-linux-musl" ;;
  Linux/aarch64|Linux/arm64) target="aarch64-unknown-linux-musl" ;;
  *) die "Nicht unterstützte Architektur: $os/$arch" ;;
esac

# --- Version bestimmen ------------------------------------------------------
if [ -n "${MINDS_VERSION:-}" ]; then
  version="${MINDS_VERSION#v}"
else
  say "Suche die neueste Version …"
  tmp_json="$(mktemp)"
  fetch "${API}/releases/latest" "$tmp_json" \
    || die "Konnte die neueste Version nicht ermitteln. Gibt es schon ein Release? Sonst MINDS_VERSION direkt setzen."
  version="$(sed -n 's/.*"tag_name"[ ]*:[ ]*"\([^"]*\)".*/\1/p' "$tmp_json" | head -n 1)"
  rm -f "$tmp_json"
  version="${version#v}"
  [ -n "$version" ] || die "Konnte die Versionsnummer nicht aus der Release-Antwort lesen."
fi

pkg="minds-${version}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/v${version}"

say "minds ${version} für ${target}"

# --- Herunterladen und prüfen ----------------------------------------------
workdir="$(mktemp -d)"
cleanup() { rm -rf "$workdir"; }
trap cleanup EXIT INT TERM

say "Lade ${pkg} …"
fetch "${base}/${pkg}" "${workdir}/${pkg}" \
  || die "Download fehlgeschlagen: ${base}/${pkg}"

# Prüfsumme, falls das Release eine mitliefert. Fehlt sie, wird gewarnt statt
# abgebrochen — ein fehlender Nebenwert soll keine Installation verhindern.
if fetch "${base}/SHA256SUMS" "${workdir}/SHA256SUMS" 2>/dev/null; then
  expected="$(grep " ${pkg}\$" "${workdir}/SHA256SUMS" | awk '{print $1}' | head -n 1)"
  if [ -n "$expected" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "${workdir}/${pkg}" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
      actual="$(shasum -a 256 "${workdir}/${pkg}" | awk '{print $1}')"
    else
      actual=""
    fi
    if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
      die "Prüfsumme stimmt nicht. Erwartet ${expected}, bekommen ${actual}."
    fi
    [ -n "$actual" ] && say "Prüfsumme stimmt."
  fi
else
  warn "Hinweis: keine SHA256SUMS im Release gefunden — überspringe die Prüfung."
fi

# --- Auspacken und installieren --------------------------------------------
tar -xzf "${workdir}/${pkg}" -C "$workdir"
binary="${workdir}/minds-${version}-${target}/minds"
[ -f "$binary" ] || die "Im Archiv liegt kein 'minds'-Binary an der erwarteten Stelle."

mkdir -p "$INSTALL_DIR"
install -m 0755 "$binary" "${INSTALL_DIR}/minds" 2>/dev/null \
  || { cp "$binary" "${INSTALL_DIR}/minds" && chmod 0755 "${INSTALL_DIR}/minds"; }

# macOS hängt heruntergeladenen Dateien ein Quarantäne-Attribut an; ohne das
# Entfernen begrüßt Gatekeeper den ersten Aufruf mit einem Fehlerdialog. Das
# Binary ist nicht notarisiert — das ist der bewusste Preis für eine
# Testauslieferung ohne Apple-Developer-Programm.
if [ "$os" = "Darwin" ] && command -v xattr >/dev/null 2>&1; then
  xattr -d com.apple.quarantine "${INSTALL_DIR}/minds" 2>/dev/null || true
fi

say ""
say "minds ${version} liegt in ${INSTALL_DIR}/minds"

# --- PATH-Hinweis -----------------------------------------------------------
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    say "Loslegen mit:  minds enable --agent claude-code"
    ;;
  *)
    say ""
    warn "${INSTALL_DIR} liegt nicht in deinem PATH. Ergänze in ~/.zshrc oder ~/.bashrc:"
    warn ""
    warn "    export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
