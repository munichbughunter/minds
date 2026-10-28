#!/bin/sh
# minds Demo M2 — NUR für Container ohne systemd (PID 1 ist hier nicht systemd).
# Startet die eine Unit, die `minds enable` schreibt, direkt unter ihrem User=;
# alles andere ist ein No-op. Auf einem echten Linux mit systemd entfällt diese Datei.
unit_file() { echo "/etc/systemd/system/$1"; }
case "$1" in
  restart|start)
    f=$(unit_file "$2"); [ -f "$f" ] || exit 1
    user=$(sed -n 's/^User=//p' "$f"); cmd=$(sed -n 's/^ExecStart=//p' "$f")
    pkill -u "$user" -f "witness serve" 2>/dev/null; sleep 1
    cd / && setsid runuser -u "$user" -- env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin \
      sh -c "umask 0007; exec $cmd" </dev/null >>/var/log/minds-witness.log 2>&1 &
    exit 0 ;;
  stop) user=$(sed -n 's/^User=//p' "$(unit_file "$2")"); pkill -u "$user" -f "witness serve"; exit 0 ;;
  is-active) pgrep -f "witness serve" >/dev/null ;;
  *) exit 0 ;;
esac
