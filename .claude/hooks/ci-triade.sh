#!/usr/bin/env bash
# CI-Triade als Stop-Hook: Claude darf den Turn nicht beenden, solange
# fmt/clippy/test nicht grün sind — die Tests nur für die geänderten
# Crates, die volle Suite läuft in der CI. Exit 2 blockiert und speist stderr
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

# Tests nur für die Crates mit uncommitteten Änderungen — die volle Suite
# läuft in der CI. Eine Änderung außerhalb von crates/ (Workspace-Cargo.toml,
# Cargo.lock) nimmt wieder den ganzen Workspace.
packages=""
whole=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  case "$path" in
    crates/*/*) crate=${path#crates/}; packages="$packages ${crate%%/*}" ;;
    xtask/*)    packages="$packages xtask" ;;
    *)          whole=1 ;;
  esac
done < <(git status --porcelain | awk '{print $NF}' | grep -E '\.(rs|toml)$')

if [ "$whole" = 1 ]; then
  out=$(cargo test --workspace 2>&1) \
    || fail "cargo test --workspace" "$out"
else
  args=""
  for crate in $packages; do
    case " $args " in
      *" -p $crate "*) ;;
      *) args="$args -p $crate" ;;
    esac
  done
  if [ -n "$args" ]; then
    # shellcheck disable=SC2086
    out=$(cargo test $args 2>&1) \
      || fail "cargo test$args" "$out"
  fi
fi

exit 0
