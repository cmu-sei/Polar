# Polar Agent Container — Startup Notes

## One-Time Host Setup

### Directory Structure
```bash
mkdir -p ~/Documents/projects/ai_models/llama
mkdir -p ~/Documents/projects/ai_models/ollama

# Pi agent state (sessions, models.json, etc.) — separate git repo
git clone https://github.com/daveman1010221/ai_state.git ~/Documents/projects/ai_state
```

### Pi Agent Configuration

`ai_state` contains `models.json` and `auth.json` under `pi/agent/`. Edit those
directly — they persist across container restarts via the volume mount at `/opt/pi`.

---

## Building the Container

```bash
# AMD ROCm (RX 9070 XT, etc.)
nix build -L .#agentContainerRocm -o result-agent-container-rocm
podman load -i result-agent-container-rocm

# NVIDIA CUDA (RTX 4000 Ada, etc.)
nix build -L .#agentContainerNvidia -o result-agent-container-nvidia
podman load -i result-agent-container-nvidia

# CPU / Mac ARM (no GPU)
nix build -L .#agentContainer -o result-agent-container
podman load -i result-agent-container
```

---

## Running the Container

### AMD (ROCm) — e.g. RX 9070 XT

```bash
podman run --rm -it --privileged --name polar-agent \
    --user 0 --userns=keep-id \
    --device /dev/kfd \
    --device /dev/dri/ \
    --ulimit memlock=-1:-1 \
    --security-opt=label=disable \
    --cap-add=SYS_PTRACE \
    --ipc=host \
    --network=host \
    -v /sys/class/kfd:/sys/class/kfd:ro \
    -v /sys/bus/pci/devices:/sys/bus/pci/devices:ro \
    -e CREATE_USER="$USER" \
    -e CREATE_UID="$(id -u)" \
    -e CREATE_GID="$(id -g)" \
    -e ATUIN_SESSION_NAME=polar-dev \
    -e HIP_VISIBLE_DEVICES=0 \
    -v ~/Documents/projects/ai_models/llama:/opt/llama-models:rw \
    -v ~/Documents/projects/ai_models/ollama:/opt/ollama:rw \
    -v ~/Documents/projects/ai_state/pi:/opt/pi:rw \
    -v $PWD:/workspace \
    -v $HOME/.config/atuin:/root/.config/atuin:ro \
    -v $HOME/.local/share/atuin:/root/.local/share/atuin \
    localhost/polar-agent:latest
```

### NVIDIA (CUDA) — e.g. RTX 4000 Ada

> `libcuda.so.1` is driver-version-specific and cannot be packaged in the container.
> The `$(readlink -f ...)` calls resolve NixOS symlinks to actual store paths at runtime.
> If the driver is updated, paths update automatically on next container start.

```bash
podman run --rm -it --privileged --name polar-agent \
  --user 0 --userns=keep-id \
  --device /dev/nvidia0 \
  --device /dev/nvidiactl \
  --device /dev/nvidia-uvm \
  --device /dev/nvidia-uvm-tools \
  --ulimit memlock=-1:-1 \
  --security-opt=label=disable \
  --cap-add=SYS_PTRACE \
  --ipc=host \
  --network=host \
  -e NVIDIA_VISIBLE_DEVICES=all \
  -e NVIDIA_DRIVER_CAPABILITIES=compute,utility \
  -e CREATE_USER="$USER" \
  -e CREATE_UID="$(id -u)" \
  -e CREATE_GID="$(id -g)" \
  -e ATUIN_SESSION_NAME=polar-dev \
  -e LD_LIBRARY_PATH=/run/opengl-driver/lib \
  -v ~/Documents/projects/ai_models/llama:/opt/llama-models:rw \
  -v ~/Documents/projects/ai_models/ollama:/opt/ollama:rw \
  -v ~/Documents/projects/ai_state/pi:/opt/pi:rw \
  -v $PWD:/workspace \
  -v $HOME/.config/atuin:/root/.config/atuin:ro \
  -v $HOME/.local/share/atuin:/root/.local/share/atuin \
  -v $(readlink -f /run/opengl-driver/lib/libcuda.so.1):/run/opengl-driver/lib/libcuda.so.1:ro \
  -v $(readlink -f /run/opengl-driver/lib/libcuda.so):/run/opengl-driver/lib/libcuda.so:ro \
  -v $(readlink -f /run/opengl-driver/lib/libnvidia-ml.so.1):/run/opengl-driver/lib/libnvidia-ml.so.1:ro \
  localhost/polar-agent:latest
```

---

## Starting llama-server

### Qwen3-Coder-30B-A3B-Instruct (recommended — AMD ROCm)

MoE model: 30.5B total params, 3.3B active at inference. Fits comfortably on 16GB VRAM
at Q4_K_M with KV cache quantization. Use Unsloth's Dynamic 2.0 quant for best quality.

```bash
    llama-server \
        --hf-repo unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF \
        --hf-file Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf \
        --ctx-size 65536 \
        --n-gpu-layers 32 \
        --flash-attn on \
        --alias "local-model" \
        --port 8080 --host 0.0.0.0 \
        --temperature 0.7 \
        --top-p 0.8 \
        --top-k 20 \
        --repeat-penalty 1.05 \
        --jinja \
        -ub 512 \
        --threads 20 \
        --threads-batch 20 \
        -ctk q4_0 -ctv q4_0 > /tmp/llama-server.log 2>&1
```

If Q4_K_M OOMs, drop to IQ4_XS (16.4GB):
```bash
  --hf-file Qwen3-Coder-30B-A3B-Instruct-IQ4_XS.gguf
```

Note: Qwen3-Coder does not use thinking mode (`<think>` blocks). No `enable_thinking`
flag needed. Sampling params above are per Qwen's own recommendation for this model —
do not use temperature=0.3 as with OmniCoder.

### OmniCoder-9B (fallback — AMD ROCm)

Smaller, faster, lower capability. Useful for simple tasks or when VRAM is constrained.

```bash
llama-server \
  --hf-repo Tesslate/OmniCoder-9B-GGUF \
  --hf-file omnicoder-9b-q4_k_m.gguf \
  --ctx-size 98304 \
  --n-gpu-layers 99 \
  --flash-attn on \
  --alias "local-model" \
  --port 8080 --host 0.0.0.0 \
  --temperature 0.3 \
  --jinja \
  -ub 4096 \
  -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1 &
```

### OmniCoder-9B (NVIDIA CUDA)

```bash
llama-server \
  --hf-repo Tesslate/OmniCoder-9B-GGUF \
  --hf-file omnicoder-9b-q4_k_m.gguf \
  --ctx-size 65536 \
  --n-gpu-layers 99 \
  --flash-attn on \
  --alias "local-model" \
  --port 8080 --host 0.0.0.0 \
  --temperature 0.3 \
  --jinja \
  -ub 2048 \
  -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1 &
```

---

## Starting Pi

```bash
cd ~
curl -L https://github.com/badlogic/pi-mono/releases/download/v0.61.1/pi-linux-x64.tar.gz | tar -xz
~/pi/pi
```

Pi commands:
```
pi          # new session
pi -c       # continue previous session
pi -r       # resume from session picker
```

Sessions persist across container restarts in `~/Documents/projects/ai_state/pi/agent/sessions`.

---

## Volume Mount Summary

| Host Path | Container Path | Purpose |
|-----------|----------------|---------|
| `~/Documents/projects/ai_models/llama` | `/opt/llama-models` | llama.cpp GGUF models |
| `~/Documents/projects/ai_models/ollama` | `/opt/ollama` | Ollama models |
| `~/Documents/projects/ai_state/pi` | `/opt/pi` | Pi sessions, models.json, auth |
| `$PWD` | `/workspace` | Project workspace |
| `$HOME/.config/atuin` | `/root/.config/atuin` | Atuin shell history config |
| `$HOME/.local/share/atuin` | `/root/.local/share/atuin` | Atuin shell history data |

The container entrypoint automatically symlinks:
- `/opt/llama-models` → `~/.cache/llama.cpp`
- `/opt/ollama` → `~/.ollama`
- `/opt/pi` → `~/.pi`
