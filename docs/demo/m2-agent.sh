#!/bin/sh
# minds Demo M2 — ein Agent-Lauf über die echten Claude-Code-Hooks, ohne
# Modell: dieselben Hook-Ereignisse, die Claude Code an `minds hook` schickt
# (SessionStart, UserPromptSubmit, PreToolUse/PostToolUse je Write, Stop), und
# dazwischen schreibt der „Agent“ die Datei — der Witness sieht sie über seinen
# Dateibeobachter. Auf dem Mac ersetzt `claude` dieses Skript.
#
#   sh m2-agent.sh <session-id> "<prompt>" <pfad>=<inhalt> [<pfad>=<inhalt> …]
#
# Läuft im Repository des Agenten (cwd), unter dessen Kennung.
set -eu
sid=$1; prompt=$2; shift 2
cwd=$(pwd)
hook() { printf '%s' "$1" | minds hook --agent claude-code >/dev/null; }
json() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
hook "{\"session_id\":\"$sid\",\"hook_event_name\":\"SessionStart\",\"source\":\"startup\",\"cwd\":\"$cwd\"}"
hook "{\"session_id\":\"$sid\",\"hook_event_name\":\"UserPromptSubmit\",\"prompt\":\"$(json "$prompt")\",\"cwd\":\"$cwd\"}"
for spec in "$@"; do
  path=${spec%%=*}; content=${spec#*=}
  input="{\"file_path\":\"$cwd/$path\",\"content\":\"$(json "$content")\"}"
  hook "{\"session_id\":\"$sid\",\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Write\",\"tool_input\":$input,\"cwd\":\"$cwd\"}"
  mkdir -p "$(dirname "$path")"
  printf '%s\n' "$content" > "$path"
  sleep 1
  hook "{\"session_id\":\"$sid\",\"hook_event_name\":\"PostToolUse\",\"tool_name\":\"Write\",\"tool_input\":$input,\"tool_response\":{\"success\":true},\"cwd\":\"$cwd\"}"
done
hook "{\"session_id\":\"$sid\",\"hook_event_name\":\"Stop\",\"stop_hook_active\":false,\"cwd\":\"$cwd\"}"
echo "agent: $# Datei(en) geschrieben, Session $sid ruht"
