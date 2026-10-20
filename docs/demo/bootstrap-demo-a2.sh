#!/bin/sh
# Legt das Demo-Projekt für die A2-Demo an (Dev- und Auditor-Sicht):
#   - ein neues Git-Repo mit Projektgerüst (Python/uv) und der Anforderung LOG-2417
#   - ein lokales Bare-Repo als "Forge" (kein Netz nötig)
#   - ein Verzeichnis für die Vertrauensdatei des Auditors
# Pfade über Umgebungsvariablen änderbar; bestehende Verzeichnisse werden nie angefasst.
set -eu

DEMO_ROOT="${DEMO_ROOT:-$HOME/dev/minds-demo-a2}"
DEMO_REMOTE="${DEMO_REMOTE:-$HOME/dev/minds-demo-a2-remote.git}"
TRUST_DIR="${TRUST_DIR:-$HOME/audit/trust}"

for p in "$DEMO_ROOT" "$DEMO_REMOTE"; do
  if [ -e "$p" ]; then
    echo "bootstrap: $p existiert schon — bitte einen anderen Pfad wählen oder löschen" >&2
    exit 1
  fi
done
command -v uv >/dev/null || { echo "bootstrap: uv fehlt" >&2; exit 1; }

mkdir -p "$DEMO_ROOT" "$TRUST_DIR"
cd "$DEMO_ROOT"
git init -q -b main

cat > README.md <<'MD'
# logsort — Demo-Projekt für Minds

Kleines Python-Werkzeug, das Logzeilen nach Zeitstempel sortiert.
Die fachliche Anforderung steht in `anforderung/LOG-2417.md`.

    uv sync
    uv run pytest
MD

cat > .gitignore <<'GI'
.venv/
__pycache__/
.pytest_cache/
*.pyc
site/
GI

cat > pyproject.toml <<'PY'
[project]
name = "logsort"
version = "0.1.0"
description = "Sortiert Logzeilen nach Zeitstempel"
requires-python = ">=3.12"
dependencies = []

[dependency-groups]
dev = ["pytest>=8"]

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["src/logsort"]

[tool.pytest.ini_options]
testpaths = ["tests"]
PY

mkdir -p src/logsort tests anforderung
cat > src/logsort/__init__.py <<'PY'
"""logsort — Logzeilen nach Zeitstempel sortieren (siehe anforderung/LOG-2417.md)."""
PY
: > tests/__init__.py

cat > anforderung/LOG-2417.md <<'MD'
# LOG-2417 — Logzeilen nach Zeitstempel sortieren

## Kontext

Unsere Dienste schreiben Logzeilen im Format

    2026-10-19T14:03:21.512Z WARN  payment  retrying charge 8812
    2026-10-19T14:03:21.512Z ERROR payment  charge 8812 failed

Beim Zusammenführen mehrerer Dateien liegen die Zeilen nicht mehr in
zeitlicher Reihenfolge. Gebraucht wird eine Funktion `sort_lines(lines)`
im Paket `logsort`, die eine Liste von Zeilen zeitlich sortiert zurückgibt,
und ein Kommandozeilen-Einstieg `python -m logsort <datei>...`, der die
sortierten Zeilen auf stdout schreibt.

## Akzeptanzkriterien

- **AC1** Zeilen werden aufsteigend nach dem ISO-8601-Zeitstempel am
  Zeilenanfang sortiert (UTC, `Z`-Suffix, Millisekunden optional).
- **AC2** Zeilen ohne gültigen Zeitstempel werden nicht verworfen: Sie
  bleiben unmittelbar hinter der zuletzt gültigen Zeile stehen
  (Fortsetzungszeilen wie Stacktraces).
- **AC3** Die Eingabe kann aus mehreren Dateien stammen; das Ergebnis ist
  eine einzige, zeitlich sortierte Ausgabe.
- **AC4** Eine leere Eingabe liefert eine leere Ausgabe und Exit-Code 0.
- **AC5** Die Sortierung ist stabil: Zeilen mit gleichem Zeitstempel
  behalten ihre Eingabereihenfolge.
- **AC6** Zeilen mit gleichem Zeitstempel werden nach Schweregrad
  geordnet: `ERROR` vor `WARN` vor `INFO` vor `DEBUG`.

## Hinweis

Alle Kriterien sind mit Tests zu belegen. Widersprechen sich Kriterien,
ist die Entscheidung im Code zu dokumentieren, nicht stillschweigend zu
treffen.
MD

uv sync -q
git add -A
git commit -q -m "chore: Projektgerüst und Anforderung LOG-2417"

git init -q --bare -b main "$DEMO_REMOTE"
git remote add origin "$DEMO_REMOTE"
git push -q -u origin main

echo "bootstrap: fertig"
echo "  Projekt   $DEMO_ROOT"
echo "  Remote    $DEMO_REMOTE"
echo "  Trust-Dir $TRUST_DIR"
echo "  Commit    $(git log -1 --format='%h %s')"
