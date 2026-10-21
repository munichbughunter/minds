# minds disable: the protected witness (Linux with systemd). Run once as root via sudo.
# Every value below was quoted by minds. Root touches only its own places
# (system units, the witness base, the managed settings) and removes
# nothing it did not install: the managed settings only if they are still
# exactly the file minds read a moment ago. A repository's witness is
# stopped and its home removed by the witness service itself, asked as the
# witness user — the agent, who shares the developer's user ID, cannot ask
# that.
set -eu
umask 022
BASE={{BASE}}
ID={{ID}}
ROOT={{ROOT}}
ALL={{ALL}}
BIN={{BINARY}}
WUSER={{WUSER}}
HOST={{HOST_SERVICE}}
MDIR={{MANAGED_DIR}}
PREVIOUS={{MANAGED_PREVIOUS}}
NEXT={{MANAGED_NEXT}}
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

fail() { echo "minds: $*" >&2; exit 1; }
plain_dir() { [ -d "$1" ] && [ ! -L "$1" ]; }
remove_service() {
  systemctl disable --now "$1" >/dev/null 2>&1 || true
  if [ -e "/etc/systemd/system/$1" ]; then
    rm -f "/etc/systemd/system/$1"
    echo "MINDS-REMOVED service $1"
  fi
}
as_witness() {
  sudo -u "$WUSER" env -i HOME=/nonexistent PATH=/usr/bin:/bin "$BIN" "$@"
}

if [ "$ALL" = 1 ]; then
  remove_service "$HOST"
  for unit in /etc/systemd/system/minds-witness-*.service; do
    [ -e "$unit" ] || continue
    remove_service "$(basename "$unit")"
  done
  if plain_dir "$BASE"; then
    rm -rf "$BASE"
    echo "MINDS-REMOVED home $BASE"
  fi
  if [ -f "$BIN" ]; then
    rm -f "$BIN"
    echo "MINDS-REMOVED binary $BIN"
  fi
else
  # The service stops this repository's witness and removes its home; without
  # a running service, root removes the home itself.
  if [ -n "$ROOT" ] && [ -S "$BASE/run/control.sock" ] \
     && as_witness witness unregister --repo "$ROOT" >/dev/null 2>&1; then
    echo "MINDS-REMOVED home $BASE/$ID"
  else
    # A unit of an earlier setup (one per repository) — stopped before its
    # home goes.
    remove_service "minds-witness-$ID.service"
    if plain_dir "$BASE/$ID"; then
      rm -rf "$BASE/$ID" || rm -rf "$BASE/$ID"
      echo "MINDS-REMOVED home $BASE/$ID"
    fi
  fi
  remove_service "minds-witness-$ID.service"
  # The last repository takes the host service and the managed settings with it.
  if ! ls "$BASE"/*/witness.json >/dev/null 2>&1; then
    remove_service "$HOST"
    NEXT=""
  fi
fi
systemctl daemon-reload

# The managed settings: only minds' own file, exactly as minds read it.
managed="$MDIR/managed-settings.json"
if [ -n "$PREVIOUS" ] && [ -f "$managed" ] && [ ! -L "$managed" ] \
   && printf '%s' "$PREVIOUS" | cmp -s - "$managed"; then
  if [ -n "$NEXT" ]; then
    tmp=$(mktemp "$MDIR/.minds.XXXXXX")
    printf '%s\n' "$NEXT" > "$tmp"
    chmod 0644 "$tmp"
    mv -f "$tmp" "$managed"
    echo "MINDS-MANAGED updated $managed"
  else
    rm -f "$managed"
    echo "MINDS-MANAGED removed $managed"
  fi
fi
