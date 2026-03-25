#!/usr/bin/env bash
# scripts/task-done.sh — called by Pi via taskfile.mark_done
set -euo pipefail
TASKFILE="${TASKFILE:-/workspace/agent-task.json}"

jq '
  if .current_task != null then
    .completed += [{
      "crate": .current_task.crate,
      "op": .current_task.op,
      "scope": (.current_task.scope // "all"),
      "status": "done"
    }]
  else
    .
  end |
  .current_task = (if (.pending | length) > 0 then .pending[0] else null end) |
  .pending = (if (.pending | length) > 0 then .pending[1:] else [] end)
' "$TASKFILE" > "$TASKFILE.tmp" && mv "$TASKFILE.tmp" "$TASKFILE"

echo "Task marked done. Current task is now:"
jq '.current_task' "$TASKFILE"
