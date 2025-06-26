#!/usr/bin/env bash
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
