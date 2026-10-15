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

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
MODEL_REPO="unsloth/Qwen3-Coder-Next-GGUF"
MODEL_FILE="Qwen3-Coder-Next-UD-IQ2_XXS.gguf"
MODEL_ALIAS="local-model"
LLAMA_PORT=8080
LLAMA_HOST="0.0.0.0"
CTX_SIZE=262144
GPU_LAYERS=99

# Where to find the model — prefer a volume-mounted path, fall back to workspace
MODEL_SOURCE="${MODEL_DIR:-/workspace/ai_models}/${MODEL_FILE}"
MODEL_CACHE="${HOME}/.cache/llama.cpp"
PI_CONFIG_DIR="${HOME}/.pi/agent"

# ---------------------------------------------------------------------------
# Model cache
# ---------------------------------------------------------------------------
mkdir -p "${MODEL_CACHE}"

if [ ! -f "${MODEL_CACHE}/${MODEL_FILE}" ]; then
    echo "Copying model to cache..."
    cp "${MODEL_SOURCE}"* "${MODEL_CACHE}/"
else
    echo "Model already cached, skipping copy."
fi

# ---------------------------------------------------------------------------
# Pi config
# ---------------------------------------------------------------------------
mkdir -p "${PI_CONFIG_DIR}"

cat > "${PI_CONFIG_DIR}/models.json" << EOF
{
  "providers": {
    "ollama": {
      "baseUrl": "http://localhost:${LLAMA_PORT}/v1",
      "api": "openai-completions",
      "apiKey": "dummy",
      "compat": {
        "supportsDeveloperRole": false,
        "supportsReasoningEffort": false
      },
      "models": [
        { "id": "${MODEL_ALIAS}" }
      ]
    }
  }
}
EOF

echo "Pi config written to ${PI_CONFIG_DIR}/models.json"

# ---------------------------------------------------------------------------
# Start llama-server (if not already running)
# ---------------------------------------------------------------------------
if curl -sf "http://localhost:${LLAMA_PORT}/health" > /dev/null 2>&1; then
    echo "llama-server already running on port ${LLAMA_PORT}, skipping."
else
    echo "Starting llama-server..."
    llama-server \
        --model "${MODEL_CACHE}/${MODEL_FILE}" \
        --alias "${MODEL_ALIAS}" \
        --ctx-size "${CTX_SIZE}" \
        --n-gpu-layers "${GPU_LAYERS}" \
        --flash-attn on \
        --cpu-moe \
        --port "${LLAMA_PORT}" \
        --host "${LLAMA_HOST}" \
        --main-gpu 0 \
        &

    # Wait for server to become ready
    echo -n "Waiting for llama-server..."
    for i in $(seq 1 30); do
        if curl -sf "http://localhost:${LLAMA_PORT}/health" > /dev/null 2>&1; then
            echo " ready!"
            break
        fi
        echo -n "."
        sleep 2
    done
fi

echo ""
echo "Done. Run pi with:"
echo "  PI_PROVIDER=ollama PI_MODEL=${MODEL_ALIAS} pi"
