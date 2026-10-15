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

set -euo pipefail

# Resolve directory of this script
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

# Find repo root by walking up from script's directory to find flake.nix
find_repo_root() {
    local dir="$SCRIPT_DIR"
    while [[ "$dir" != "/" ]]; do
        if [[ -f "$dir/flake.nix" ]]; then
            echo "$dir"
            return
        fi
        dir="$(dirname "$dir")"
    done
    echo "❌ Could not find flake.nix in any parent directory of $SCRIPT_DIR." >&2
    exit 1
}

REPO_ROOT="$(find_repo_root)"
BIN_PATH="$REPO_ROOT/result/bin/git-hooks"
GIT_HOOK_PATH="$REPO_ROOT/.git/hooks/commit-msg"

if [[ ! -x "$BIN_PATH" ]]; then
    echo "❌ Built hook binary not found at: $BIN_PATH"
    echo "👉 Did you forget to run: nix build .#commitMsgHooksPkg.commitMsgHook"
    exit 1
fi

# Ensure .git/hooks exists
mkdir -p "$(dirname "$GIT_HOOK_PATH")"

# Install the hook
echo "🪝 Installing commit-msg hook to $GIT_HOOK_PATH"
cp "$BIN_PATH" "$GIT_HOOK_PATH"
chmod +x "$GIT_HOOK_PATH"

echo "✅ Git hook installed successfully!"
