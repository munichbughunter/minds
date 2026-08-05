#!/usr/bin/env bash
# CI-Triade als Stop-Hook: Claude darf den Turn nicht beenden, solange
# fmt/clippy/test nicht grün sind. Exit 2 blockiert und speist stderr
# als Korrektur-Auftrag zurück an Claude; Exit 0 lässt den Stop durch.
#
# Fail-closed auf Workflow-Ebene — dasselbe Prinzip wie RedactedSession,
# nur für den Entwicklungsprozess selbst.
set -u

# Nur aktiv, wenn es uncommittete Rust-Änderungen gibt. Reine Frage-
# Antwort-Turns ohne Codeänderung sollen nicht durch die Triade laufen.
if git diff --quiet && git diff --cached --quiet; then
  exit 0
fi
if ! git status --porcelain | grep -qE '\.(rs|toml)$'; then
  exit 0
fi

fail() {
  echo "CI-Triade fehlgeschlagen: $1" >&2
  echo "" >&2
  echo "$2" >&2
  echo "" >&2
  echo "Behebe die obigen Fehler und versuche es erneut." >&2
  exit 2
}

out=$(cargo fmt --all -- --check 2>&1) \
  || fail "cargo fmt --all -- --check" "$out"

out=$(cargo clippy --workspace --all-targets -- -D warnings 2>&1) \
  || fail "cargo clippy -D warnings" "$out"

out=$(cargo test --workspace 2>&1) \
  || fail "cargo test --workspace" "$out"

exit 0
