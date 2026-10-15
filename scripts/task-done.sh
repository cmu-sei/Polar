#!/usr/bin/env bash

# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

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
