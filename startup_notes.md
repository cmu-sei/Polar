# Polar Agent Container — Startup Notes

## One-Time Host Setup

### Directory Structure
```bash
# AI models — separate directories for llama.cpp and ollama
mkdir -p ~/Documents/projects/ai_models/llama
mkdir -p ~/Documents/projects/ai_models/ollama

# Pi agent state (sessions, models.json, skills, etc.)
# This is a separate git repo: github.com/daveman1010221/ai_state
git clone https://github.com/daveman1010221/ai_state.git ~/Documents/projects/ai_state
```

### Pi Agent Configuration

`ai_state` already contains `models.json`, `settings.json`, and `tool_instructions.md`
under `pi/agent/`. Edit those directly — they persist across container restarts via
the volume mount.

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

Inside the container, start llama-server:
```bash
llama-server \
  --hf-repo Tesslate/OmniCoder-9B-GGUF \
  --hf-file omnicoder-9b-q4_k_m.gguf \
  --ctx-size 262144 \
  --n-gpu-layers 99 \
  --flash-attn on \
  --alias "local-model" \
  --port 8080 \
  --host 0.0.0.0 \
  --temperature 0.3 \
  --jinja \
  -ub 4096 \
  -ctk q8_0 \
  -ctv q8_0 &
```

### NVIDIA (CUDA) — e.g. RTX 4000 Ada Generation

> **Note:** The NVIDIA driver libraries must be bind-mounted from the host
> because `libcuda.so.1` is driver-version-specific and cannot be packaged
> in the container. The `$(readlink -f ...)` calls resolve symlinks in
> `/run/opengl-driver/lib` to the actual Nix store paths on the host.
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

Inside the container, start llama-server:
```bash
llama-server \
  --hf-repo Tesslate/OmniCoder-9B-GGUF \
  --hf-file omnicoder-9b-q4_k_m.gguf \
  --ctx-size 65536 \
  --n-gpu-layers 99 \
  --flash-attn on \
  --alias "local-model" \
  --port 8080 \
  --host 0.0.0.0 \
  --temperature 0.3 \
  --jinja \
  -ub 2048 \
  -ctk q8_0 \
  -ctv q8_0 &
```

---

## Using Pi Agent

After starting `llama-server`, run:
```bash
pi "Your prompt here"
pi -c   # continue previous session
pi -r   # resume from session picker
```

Sessions persist across container restarts in `~/Documents/projects/ai_state/pi/agent/sessions`.

---

## Volume Mount Summary

| Host Path | Container Path | Purpose |
|-----------|---------------|---------|
| `~/Documents/projects/ai_models/llama` | `/opt/llama-models` | llama.cpp GGUF models |
| `~/Documents/projects/ai_models/ollama` | `/opt/ollama` | Ollama models + identity key |
| `~/Documents/projects/ai_state/pi` | `/opt/pi` | Pi sessions, models.json, skills |
| `$PWD` | `/workspace` | Project workspace |
| `$HOME/.config/atuin` | `/root/.config/atuin` | Atuin shell history config |
| `$HOME/.local/share/atuin` | `/root/.local/share/atuin` | Atuin shell history data |

The container entrypoint automatically symlinks:
- `/opt/llama-models` → `~/.cache/llama.cpp`
- `/opt/ollama` → `~/.ollama`
- `/opt/pi` → `~/.pi`

---

## NVIDIA Driver Library Note

On NixOS, `/run/opengl-driver/lib/libcuda.so.1` is a symlink into the Nix
store. Since the Nix store path isn't available inside the container, the
actual library files must be bind-mounted directly using `readlink -f` to
resolve the symlink targets. If the NVIDIA driver is updated, these paths
will change automatically on next container start since `readlink -f` is
evaluated at runtime.
