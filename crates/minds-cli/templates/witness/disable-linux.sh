# minds disable: the protected witness (Linux with systemd). Run once as root via sudo.
# Every value below was quoted by minds. Root touches only its own places
# (system units, the witness base, the managed settings) and removes
# nothing it did not install: the managed settings only if they are still
# exactly the file minds read a moment ago.
set -eu
umask 022
BASE={{BASE}}
ID={{ID}}
ALL={{ALL}}
BIN={{BINARY}}
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

if [ "$ALL" = 1 ]; then
  for unit in /etc/systemd/system/minds-witness*.service; do
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
  remove_service "minds-witness-$ID.service"
  if plain_dir "$BASE/$ID"; then
    rm -rf "$BASE/$ID"
    echo "MINDS-REMOVED home $BASE/$ID"
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
