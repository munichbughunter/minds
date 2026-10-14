# minds enable: the protected witness (Linux with systemd). Run once as root via sudo.
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
UNIT={{LABEL}}
UNIT_FILE={{SERVICE_FILE}}
MDIR={{MANAGED_DIR}}
PREVIOUS={{MANAGED_PREVIOUS}}
INTENT_KEY={{INTENT_KEY}}
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

fail() { echo "minds: $*" >&2; exit 1; }
owner_of() { stat -c '%u' "$1"; }
mode_of() { stat -c '%a' "$1"; }
# A directory only root may change: a real directory, owned by root, not
# writable by group or others.
root_dir() {
  { [ -d "$1" ] && [ ! -L "$1" ]; } || fail "$1 is not a plain directory"
  [ "$(owner_of "$1")" = 0 ] || fail "$1 does not belong to root"
  case "$(mode_of "$1")" in
    "" | *[2367]? | *[2367]) fail "$1 is writable by others than root" ;;
  esac
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
    if [ -e "$path" ] || [ -L "$path" ]; then root_dir "$path"; else mkdir -m 0755 "$path" && root_dir "$path"; fi
  done
}

# 1. The binary, root-owned, outside every user's home. You read it (sudo -u):
#    a file you cannot read cannot be copied; root only writes the copy into
#    its own, checked directory.
root_dirs "$(dirname "$BIN")"
root_dirs "$MDIR"
[ -f "$SOURCE" ] || fail "$SOURCE is not a regular file"
rm -f "$BIN.minds-tmp"
# At most 256 MiB — a swapped-in endless source cannot fill the system disk.
(umask 077 && sudo -u "$DEVUSER" dd if="$SOURCE" bs=1048576 count=257 2>/dev/null \
  > "$BIN.minds-tmp") || { rm -f "$BIN.minds-tmp"; fail "cannot read $SOURCE as $DEVUSER"; }
size=$(stat -c '%s' "$BIN.minds-tmp")
[ "$size" -gt 0 ] && [ "$size" -le 268435456 ] \
  || { rm -f "$BIN.minds-tmp"; fail "$SOURCE is empty or larger than 256 MiB"; }
chown root:root "$BIN.minds-tmp"
chmod 0755 "$BIN.minds-tmp"
mv -f "$BIN.minds-tmp" "$BIN"

# 2. The witness user (system account, no login) and its group — the group
#    has two members: the witness and you (effective for you after your next
#    login).
if ! getent passwd "$WUSER" >/dev/null; then
  useradd --system --user-group --no-create-home --home-dir /nonexistent \
    --shell /usr/sbin/nologin "$WUSER"
fi
usermod -aG "$WUSER" "$DEVUSER"

# 3. The repository for the witness — done as you, never as root (you own
#    these paths; sudo -u gives you the new group at once): it may pass your
#    directories above the repository (an ACL entry, search only) and list
#    the root; the parts of .git it writes (prepared by you) move to its group.
command -v setfacl >/dev/null || fail "setfacl is missing — install the acl package"
as_dev() { sudo -u "$DEVUSER" "$@"; }
dir=$(dirname "$ROOT")
while [ "$dir" != / ]; do
  if [ "$(owner_of "$dir")" = "$DEVUID" ]; then as_dev setfacl -m "u:$WUSER:x" "$dir"; fi
  dir=$(dirname "$dir")
done
as_dev setfacl -m "u:$WUSER:rx" "$ROOT"
# First the group, then the modes — nothing becomes writable for any other
# group. Directories: group-writable and setgid (new entries keep the group;
# .git also sticky: the witness may create lock files but not replace
# yours); files: group-readable/writable.
as_dev find "$ROOT/.git" "$ROOT/.git/minds" -maxdepth 0 -user "$DEVUID" -exec chgrp -h "$WUSER" {} +
as_dev find "$ROOT/.git/objects" "$ROOT/.git/refs" "$ROOT/.git/logs" -xdev -user "$DEVUID" \
  -exec chgrp -h "$WUSER" {} +
as_dev chmod g+rwxs,+t "$ROOT/.git"
as_dev chmod g+rwxs "$ROOT/.git/minds"
as_dev find "$ROOT/.git/objects" "$ROOT/.git/refs" "$ROOT/.git/logs" -xdev -user "$DEVUID" \
  -type d -exec chmod g+rwxs {} +
as_dev find "$ROOT/.git/objects" "$ROOT/.git/refs" "$ROOT/.git/logs" -xdev -user "$DEVUID" \
  -type f -exec chmod g+rw {} +
sudo -u "$WUSER" test -w "$ROOT/.git" -a -w "$ROOT/.git/objects" -a -w "$ROOT/.git/refs" \
  -a -w "$ROOT/.git/logs" -a -w "$ROOT/.git/minds" -a -r "$ROOT/.git/HEAD" \
  || fail "the witness user cannot write $ROOT/.git"

# 4. Its home: the base belongs to the witness, its group may only traverse
#    it to the socket.
root_dirs "$(dirname "$BASE")"
if [ -e "$BASE" ] || [ -L "$BASE" ]; then
  { [ -d "$BASE" ] && [ ! -L "$BASE" ]; } || fail "$BASE is not a plain directory"
fi
install -d -o "$WUSER" -g "$WUSER" -m 0710 "$BASE"
as_witness() {
  sudo -u "$WUSER" env -i HOME=/nonexistent PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0="$ROOT" \
    MINDS_WITNESS_HOME="$HOME_DIR" "$BIN" "$@"
}
# Your intent key (if minds.intentKey names a security key) is pinned as the key that
# approves intents: a fresh touch activates one over the witness socket. A key
# pinned earlier is never replaced here (witness init --replace-intent-key).
if [ -n "$INTENT_KEY" ]; then
  out=$(as_witness witness init --repo "$ROOT" --profile user --socket-group "$WUSER" \
    --intent-key "$INTENT_KEY")
else
  out=$(as_witness witness init --repo "$ROOT" --profile user --socket-group "$WUSER")
fi
printf '%s\n' "$out" | sed -n 's/^intent key: */MINDS-INTENT-KEY /p'
[ -e "$HOME_DIR/key/witness_ed25519" ] || as_witness witness keygen >/dev/null
line=$(as_witness witness signers)
[ -n "$line" ] || fail "the witness printed no signers line"
echo "MINDS-SIGNERS $line"

# 5. The system service, under the witness user.
root_dir /etc/systemd/system
printf '%s\n' {{SERVICE_CONTENT}} > "$UNIT_FILE.minds-tmp"
chown root:root "$UNIT_FILE.minds-tmp"
chmod 0644 "$UNIT_FILE.minds-tmp"
mv -f "$UNIT_FILE.minds-tmp" "$UNIT_FILE"
systemctl daemon-reload
systemctl enable "$UNIT" >/dev/null
systemctl restart "$UNIT"
echo "MINDS-NOTE log out and back in once — your membership in $WUSER takes effect with your next login"

# 6. Managed settings for Claude Code: deny rules only, and only if none
#    exist; our own file from an earlier run (also another repository's) is
#    merged. Its directory was checked first.
tmp=$(mktemp "$MDIR/.minds.XXXXXX")
printf '%s\n' {{MANAGED_CONTENT}} > "$tmp"
chmod 0644 "$tmp"
if [ -e "$MDIR/managed-settings.json" ] || [ -L "$MDIR/managed-settings.json" ]; then
  if cmp -s "$tmp" "$MDIR/managed-settings.json"; then
    rm -f "$tmp"
    echo "MINDS-MANAGED unchanged $MDIR/managed-settings.json"
  elif [ -n "$PREVIOUS" ] && printf '%s' "$PREVIOUS" | cmp -s - "$MDIR/managed-settings.json"; then
    # Our own file, exactly as minds read it: the union of its rules and the
    # new ones replaces it. Anything else stays and gets a proposal.
    mv -f "$tmp" "$MDIR/managed-settings.json"
    echo "MINDS-MANAGED updated $MDIR/managed-settings.json"
  else
    mv -f "$tmp" "$MDIR/managed-settings.minds-proposed.json"
    echo "MINDS-MANAGED proposed $MDIR/managed-settings.minds-proposed.json"
  fi
else
  mv -f "$tmp" "$MDIR/managed-settings.json"
  echo "MINDS-MANAGED installed $MDIR/managed-settings.json"
fi
