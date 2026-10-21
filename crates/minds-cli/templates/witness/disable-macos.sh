# minds disable: the protected witness (macOS). Run once as root via sudo.
# Every value below was quoted by minds. Root touches only its own places
# (system service files, the witness base, the managed settings) and
# removes nothing it did not install: the managed settings only if they
# are still exactly the file minds read a moment ago.
set -eu
umask 022
BASE={{BASE}}
ID={{ID}}
ALL={{ALL}}
BIN={{BINARY}}
MDIR={{MANAGED_DIR}}
PREVIOUS={{MANAGED_PREVIOUS}}
NEXT={{MANAGED_NEXT}}

fail() { echo "minds: $*" >&2; exit 1; }
plain_dir() { [ -d "$1" ] && [ ! -L "$1" ]; }
remove_service() {
  /bin/launchctl bootout "system/$1" >/dev/null 2>&1 || true
  if [ -e "/Library/LaunchDaemons/$1.plist" ]; then
    /bin/rm -f "/Library/LaunchDaemons/$1.plist"
    echo "MINDS-REMOVED service $1"
  fi
}

if [ "$ALL" = 1 ]; then
  for plist in /Library/LaunchDaemons/dev.minds.witness*.plist; do
    [ -e "$plist" ] || continue
    remove_service "$(/usr/bin/basename "$plist" .plist)"
  done
  if plain_dir "$BASE"; then
    /bin/rm -rf "$BASE"
    echo "MINDS-REMOVED home $BASE"
  fi
  if [ -f "$BIN" ]; then
    /bin/rm -f "$BIN"
    echo "MINDS-REMOVED binary $BIN"
  fi
else
  remove_service "dev.minds.witness.$ID"
  if plain_dir "$BASE/$ID"; then
    /bin/rm -rf "$BASE/$ID"
    echo "MINDS-REMOVED home $BASE/$ID"
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
