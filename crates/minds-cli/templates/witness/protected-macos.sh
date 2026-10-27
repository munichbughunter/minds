# minds enable: the protected witness (macOS). Run once as root via sudo.
# Every value below was quoted by minds. Root never follows a path the
# developer (or an agent running as the developer) could have planted: system
# directories are checked to be root's alone before anything is written, the
# binary is read from one opened file, and everything inside the repository is
# done as the developer (sudo -u), never as root.
set -eu
umask 022
SOURCE={{SOURCE}}
BIN={{BINARY}}
WUSER={{WUSER}}
DEVUSER={{DEVUSER}}
DEVUID={{DEVUID}}
BASE={{BASE}}
HOME_DIR={{HOME}}
ROOT={{ROOT}}
LABEL={{LABEL}}
PLIST={{SERVICE_FILE}}
MDIR={{MANAGED_DIR}}
PREVIOUS={{MANAGED_PREVIOUS}}
INTENT_KEY={{INTENT_KEY}}
PROJECT={{PROJECT}}

fail() { echo "minds: $*" >&2; exit 1; }
owner_of() { /usr/bin/stat -f '%u' "$1"; }
mode_of() { /usr/bin/stat -f '%Lp' "$1"; }
# A directory only root may change: a real directory, owned by root, not
# writable by group or others.
root_dir() {
  { [ -d "$1" ] && [ ! -L "$1" ]; } || fail "$1 is not a plain directory"
  [ "$(owner_of "$1")" = 0 ] || fail "$1 does not belong to root"
  case "$(mode_of "$1")" in
    "" | *[2367]? | *[2367]) fail "$1 is writable by others than root" ;;
  esac
  # An ACL could grant what the mode bits deny.
  if /bin/ls -led "$1" | /usr/bin/grep -q '^ *[0-9][0-9]*: '; then fail "$1 carries an ACL"; fi
}
# Every directory of an absolute path checked as root's alone; missing ones
# are created (root, 0755) — never through a symlink.
root_dirs() {
  root_dir /
  rest=${1#/}
  path=""
  while [ -n "$rest" ]; do
    part=${rest%%/*}
    case "$rest" in */*) rest=${rest#*/} ;; *) rest="" ;; esac
    path="$path/$part"
    if [ -e "$path" ] || [ -L "$path" ]; then root_dir "$path"; else /bin/mkdir -m 0755 "$path" && root_dir "$path"; fi
  done
}

# 1. The binary, root-owned, outside every user's home. You read it (sudo -u):
#    a file you cannot read cannot be copied; root only writes the copy into
#    its own, checked directory.
root_dirs "$(/usr/bin/dirname "$BIN")"
root_dirs "$MDIR"
[ -f "$SOURCE" ] || fail "$SOURCE is not a regular file"
/bin/rm -f "$BIN.minds-tmp"
# At most 256 MiB — a swapped-in endless source cannot fill the system disk.
(umask 077 && /usr/bin/sudo -u "$DEVUSER" /bin/dd if="$SOURCE" bs=1048576 count=257 2>/dev/null \
  > "$BIN.minds-tmp") || { /bin/rm -f "$BIN.minds-tmp"; fail "cannot read $SOURCE as $DEVUSER"; }
size=$(/usr/bin/stat -f '%z' "$BIN.minds-tmp")
[ "$size" -gt 0 ] && [ "$size" -le 268435456 ] \
  || { /bin/rm -f "$BIN.minds-tmp"; fail "$SOURCE is empty or larger than 256 MiB"; }
/usr/sbin/chown root:wheel "$BIN.minds-tmp"
/bin/chmod 0755 "$BIN.minds-tmp"
/bin/mv -f "$BIN.minds-tmp" "$BIN"

# 2. The witness user (hidden, no login) and its group — the group has two
#    members: the witness and you.
if ! /usr/bin/dscl . -read "/Users/$WUSER" >/dev/null 2>&1; then
  # A group left by an aborted run keeps its id.
  id=$(/usr/bin/dscl . -read "/Groups/$WUSER" PrimaryGroupID 2>/dev/null | /usr/bin/awk '{print $2}')
  if [ -n "$id" ]; then
    if /usr/bin/dscl . -list /Users UniqueID | /usr/bin/awk '{print $2}' | /usr/bin/grep -qx "$id"; then
      fail "the group $WUSER exists with id $id, which another user already has"
    fi
  else
    id=300
    while /usr/bin/dscl . -list /Users UniqueID | /usr/bin/awk '{print $2}' | /usr/bin/grep -qx "$id" \
       || /usr/bin/dscl . -list /Groups PrimaryGroupID | /usr/bin/awk '{print $2}' | /usr/bin/grep -qx "$id"; do
      id=$((id + 1))
      [ "$id" -lt 400 ] || fail "no free system id for $WUSER"
    done
    /usr/bin/dscl . -create "/Groups/$WUSER"
    /usr/bin/dscl . -create "/Groups/$WUSER" PrimaryGroupID "$id"
  fi
  /usr/bin/dscl . -create "/Users/$WUSER"
  /usr/bin/dscl . -create "/Users/$WUSER" UniqueID "$id"
  /usr/bin/dscl . -create "/Users/$WUSER" PrimaryGroupID "$id"
  /usr/bin/dscl . -create "/Users/$WUSER" UserShell /usr/bin/false
  /usr/bin/dscl . -create "/Users/$WUSER" NFSHomeDirectory /var/empty
  /usr/bin/dscl . -create "/Users/$WUSER" RealName "minds witness"
  /usr/bin/dscl . -create "/Users/$WUSER" Password '*'
  /usr/bin/dscl . -create "/Users/$WUSER" IsHidden 1
fi
/usr/sbin/dseditgroup -o edit -a "$DEVUSER" -t user "$WUSER"
/usr/bin/dscacheutil -flushcache
/usr/sbin/dsmemberutil flushcache >/dev/null 2>&1 || true

# 3. The repository for the witness — done as you, never as root (you own
#    these paths, and you are now in the witness group): it may pass your
#    directories above the repository (an ACL entry, search only) and list
#    the root; the parts of .git it writes (prepared by you) move to its group.
#    (minds does the same without sudo for every further repository —
#    enable_protected::share_repo — once the service runs.)
as_dev() { /usr/bin/sudo -u "$DEVUSER" "$@"; }
dir=$(/usr/bin/dirname "$ROOT")
while [ "$dir" != / ]; do
  if [ "$(owner_of "$dir")" = "$DEVUID" ] \
     && ! /bin/ls -led "$dir" | /usr/bin/grep -q "user:$WUSER allow search"; then
    as_dev /bin/chmod -h +a "user:$WUSER allow search" "$dir"
  fi
  dir=$(/usr/bin/dirname "$dir")
done
if ! /bin/ls -led "$ROOT" | /usr/bin/grep -q "user:$WUSER allow list,search"; then
  as_dev /bin/chmod -h +a "user:$WUSER allow list,search" "$ROOT"
fi
# First the group, then the modes — nothing becomes writable for any other
# group. Directories: group-writable (and, on .git, sticky: the witness may
# create lock files but not replace yours); files: group-readable/writable.
as_dev /usr/bin/find "$ROOT/.git" "$ROOT/.git/minds" -maxdepth 0 -user "$DEVUID" \
  -exec /usr/bin/chgrp -h "$WUSER" {} +
as_dev /usr/bin/find "$ROOT/.git/objects" "$ROOT/.git/refs" "$ROOT/.git/logs" -xdev -user "$DEVUID" \
  -exec /usr/bin/chgrp -h "$WUSER" {} +
as_dev /bin/chmod g+rwx,+t "$ROOT/.git"
as_dev /bin/chmod g+rwx "$ROOT/.git/minds"
as_dev /usr/bin/find "$ROOT/.git/objects" "$ROOT/.git/refs" "$ROOT/.git/logs" -xdev -user "$DEVUID" \
  -type d -exec /bin/chmod g+rwx {} +
as_dev /usr/bin/find "$ROOT/.git/objects" "$ROOT/.git/refs" "$ROOT/.git/logs" -xdev -user "$DEVUID" \
  -type f -exec /bin/chmod g+rw {} +
/usr/bin/sudo -u "$WUSER" /bin/test -w "$ROOT/.git" -a -w "$ROOT/.git/objects" -a -w "$ROOT/.git/refs" \
  -a -w "$ROOT/.git/logs" -a -w "$ROOT/.git/minds" -a -r "$ROOT/.git/HEAD" \
  || fail "the witness user cannot write $ROOT/.git"

# 4. Its home: the base belongs to the witness, its group may only traverse
#    it to the sockets. The service (step 5) adopts the home; a later
#    repository is registered over the service's socket instead.
root_dirs "$(/usr/bin/dirname "$BASE")"
if [ -e "$BASE" ] || [ -L "$BASE" ]; then
  { [ -d "$BASE" ] && [ ! -L "$BASE" ]; } || fail "$BASE is not a plain directory"
fi
/usr/bin/install -d -o "$WUSER" -g "$WUSER" -m 0710 "$BASE"
/usr/bin/install -d -o "$WUSER" -g "$WUSER" -m 0700 "$BASE/log"
as_witness() {
  /usr/bin/sudo -u "$WUSER" /usr/bin/env -i HOME=/var/empty PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0="$ROOT" \
    MINDS_WITNESS_HOME="$HOME_DIR" "$BIN" "$@"
}
# Your intent key (if minds.intentKey names a security key) is pinned as the key that
# approves intents: a fresh touch activates one over the witness socket. A key
# pinned earlier is never replaced here (witness init --replace-intent-key).
# The project the witness binds commits for (minds enable --project) is pinned
# only where none is pinned yet; a different pinned project stops the setup.
set -- witness init --repo "$ROOT" --profile user --socket-group "$WUSER"
[ -z "$INTENT_KEY" ] || set -- "$@" --intent-key "$INTENT_KEY"
[ -z "$PROJECT" ] || set -- "$@" --project "$PROJECT"
out=$(as_witness "$@")
printf '%s\n' "$out" | /usr/bin/sed -n 's/^pinned project: *\([^ ]*\).*/MINDS-PROJECT \1/p'
printf '%s\n' "$out" | /usr/bin/sed -n 's/^intent key: */MINDS-INTENT-KEY /p'
[ -e "$HOME_DIR/key/witness_ed25519" ] || as_witness witness keygen >/dev/null
line=$(as_witness witness signers)
[ -n "$line" ] || fail "the witness printed no signers line"
echo "MINDS-SIGNERS $line"

# 5. The system service — one per host, under the witness user: it runs a
#    witness per home under the base and takes registrations. Services of
#    earlier setups (one per repository) give way to it; their homes stay
#    and the service adopts them. (Re)started so that it runs this binary.
root_dir /Library/LaunchDaemons
for old in /Library/LaunchDaemons/dev.minds.witness.*.plist; do
  [ -e "$old" ] || continue
  /bin/launchctl bootout "system/$(/usr/bin/basename "$old" .plist)" >/dev/null 2>&1 || true
  /bin/rm -f "$old"
done
printf '%s\n' {{SERVICE_CONTENT}} > "$PLIST.minds-tmp"
/usr/sbin/chown root:wheel "$PLIST.minds-tmp"
/bin/chmod 0644 "$PLIST.minds-tmp"
/bin/mv -f "$PLIST.minds-tmp" "$PLIST"
/bin/launchctl bootout "system/$LABEL" >/dev/null 2>&1 || true
tries=0
until /bin/launchctl bootstrap system "$PLIST" 2>/dev/null; do
  tries=$((tries + 1))
  [ "$tries" -lt 10 ] || { /bin/launchctl bootstrap system "$PLIST"; exit 1; }
  /bin/sleep 0.3
done

# 6. Managed settings for Claude Code: deny rules only, machine-wide (no
#    rule per repository), and only if none exist; our own file from an
#    earlier run is merged. Its directory was checked first.
tmp=$(/usr/bin/mktemp "$MDIR/.minds.XXXXXX")
printf '%s\n' {{MANAGED_CONTENT}} > "$tmp"
/bin/chmod 0644 "$tmp"
if [ -e "$MDIR/managed-settings.json" ] || [ -L "$MDIR/managed-settings.json" ]; then
  if /usr/bin/cmp -s "$tmp" "$MDIR/managed-settings.json"; then
    /bin/rm -f "$tmp"
    echo "MINDS-MANAGED unchanged $MDIR/managed-settings.json"
  elif [ -n "$PREVIOUS" ] && printf '%s' "$PREVIOUS" | /usr/bin/cmp -s - "$MDIR/managed-settings.json"; then
    # Our own file, exactly as minds read it: the union of its rules and the
    # new ones replaces it. Anything else stays and gets a proposal.
    /bin/mv -f "$tmp" "$MDIR/managed-settings.json"
    echo "MINDS-MANAGED updated $MDIR/managed-settings.json"
  else
    /bin/mv -f "$tmp" "$MDIR/managed-settings.minds-proposed.json"
    echo "MINDS-MANAGED proposed $MDIR/managed-settings.minds-proposed.json"
  fi
else
  /bin/mv -f "$tmp" "$MDIR/managed-settings.json"
  echo "MINDS-MANAGED installed $MDIR/managed-settings.json"
fi
