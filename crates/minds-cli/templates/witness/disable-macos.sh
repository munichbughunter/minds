# minds disable: the protected witness (macOS). Run once as root via sudo.
# Every value below was quoted by minds. Root touches only its own places
# (system service files, the witness base, the managed settings) and
# removes nothing it did not install: the managed settings only if they
# are still exactly the file minds read a moment ago. A repository's
# witness is stopped and its home removed by the witness service itself,
# asked as the witness user — the agent, who shares the developer's user
# ID, cannot ask that.
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

fail() { echo "minds: $*" >&2; exit 1; }
plain_dir() { [ -d "$1" ] && [ ! -L "$1" ]; }
remove_service() {
  /bin/launchctl bootout "system/$1" >/dev/null 2>&1 || true
  # bootout returns before the job is gone; a home removed under a witness
  # still writing came back half-deleted.
  tries=0
  while /bin/launchctl print "system/$1" >/dev/null 2>&1 && [ "$tries" -lt 50 ]; do
    tries=$((tries + 1))
    /bin/sleep 0.1
  done
  if [ -e "/Library/LaunchDaemons/$1.plist" ]; then
    /bin/rm -f "/Library/LaunchDaemons/$1.plist"
    echo "MINDS-REMOVED service $1"
  fi
}
as_witness() {
  /usr/bin/sudo -u "$WUSER" /usr/bin/env -i HOME=/var/empty PATH=/usr/bin:/bin "$BIN" "$@"
}

if [ "$ALL" = 1 ]; then
  remove_service "$HOST"
  for plist in /Library/LaunchDaemons/dev.minds.witness.*.plist; do
    [ -e "$plist" ] || continue
    remove_service "$(/usr/bin/basename "$plist" .plist)"
  done
  if plain_dir "$BASE"; then
    /bin/rm -rf "$BASE" || /bin/rm -rf "$BASE"
    echo "MINDS-REMOVED home $BASE"
  fi
  if [ -f "$BIN" ]; then
    /bin/rm -f "$BIN"
    echo "MINDS-REMOVED binary $BIN"
  fi
else
  # The service stops this repository's witness and removes its home; without
  # a running service, root removes the home itself.
  if [ -n "$ROOT" ] && [ -S "$BASE/run/control.sock" ] \
     && as_witness witness unregister --repo "$ROOT" >/dev/null 2>&1; then
    echo "MINDS-REMOVED home $BASE/$ID"
  else
    # A service of an earlier setup (one per repository) — stopped before
    # its home goes.
    remove_service "dev.minds.witness.$ID"
    if plain_dir "$BASE/$ID"; then
      /bin/rm -rf "$BASE/$ID" || /bin/rm -rf "$BASE/$ID"
      echo "MINDS-REMOVED home $BASE/$ID"
    fi
  fi
  remove_service "dev.minds.witness.$ID"
  # The last repository takes the host service and the managed settings with it.
  if ! /bin/ls "$BASE"/*/witness.json >/dev/null 2>&1; then
    remove_service "$HOST"
    NEXT=""
  fi
fi

# The managed settings: only minds' own file, exactly as minds read it.
managed="$MDIR/managed-settings.json"
if [ -n "$PREVIOUS" ] && [ -f "$managed" ] && [ ! -L "$managed" ] \
   && printf '%s' "$PREVIOUS" | /usr/bin/cmp -s - "$managed"; then
  if [ -n "$NEXT" ]; then
    tmp=$(/usr/bin/mktemp "$MDIR/.minds.XXXXXX")
    printf '%s\n' "$NEXT" > "$tmp"
    /bin/chmod 0644 "$tmp"
    /bin/mv -f "$tmp" "$managed"
    echo "MINDS-MANAGED updated $managed"
  else
    /bin/rm -f "$managed"
    echo "MINDS-MANAGED removed $managed"
  fi
fi
