# Polar Justfile
#
# Host-side recipes: build images, run containers, dev workflow
# Container-side recipes: start llama-server, start pi
#
# Usage:
#   just <recipe>                         # run with defaults
#   just platform=aarch64-linux <recipe>  # override platform
#   just verbose=false <recipe>           # quiet status line
#
# Platform autodetects from uname. Override for cross-builds.
# Verbose defaults to true (show full nix build logs).

platform := `uname -m | sed 's/x86_64/x86_64-linux/;s/aarch64/aarch64-linux/'`
verbose   := "true"

_nix_flags := if verbose == "true" { "-L" } else { "--log-format bar-with-logs" }

# ── Help ──────────────────────────────────────────────────────────────────────

# List all available recipes
default:
    @just --list

# ── Agent containers ──────────────────────────────────────────────────────────

# Build and load the agent container for your GPU type
# Usage: just agent-container           (ROCm, default)
#        just agent-container nvidia
#        just agent-container vulkan
#        just agent-container cpu
agent-container gpu='rocm':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{gpu}}" in
        rocm)   target=agentContainerRocm ;;
        nvidia) target=agentContainerNvidia ;;
        vulkan) target=agentContainerVulkan ;;
        cpu)    target=agentContainer ;;
        *) echo "Unknown gpu type: {{gpu}}. Use rocm, nvidia, vulkan, or cpu." && exit 1 ;;
    esac
    echo "Building agent container ($target) for {{platform}}..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.$target" -o result-agent-container
    podman load -i result-agent-container

# ── Dev container ─────────────────────────────────────────────────────────────

# Build and load the dev container
dev-container:
    echo "Building dev container ({{platform}})..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer" -o result-dev-container
    podman load -i result-dev-container

# ── Cassini ───────────────────────────────────────────────────────────────────

# Build cassini images
# Usage: just cassini           (all images)
#        just cassini server
#        just cassini producer
#        just cassini sink
#        just cassini client
cassini target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.cassini"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.cassiniImage"  -o result-cassini-image
            nix build {{_nix_flags}} "$base.producerImage" -o result-cassini-producer-image
            nix build {{_nix_flags}} "$base.sinkImage"     -o result-cassini-sink-image
            podman load -i result-cassini-image
            podman load -i result-cassini-producer-image
            podman load -i result-cassini-sink-image
            ;;
        server)
            nix build {{_nix_flags}} "$base.cassiniImage" -o result-cassini-image
            podman load -i result-cassini-image
            ;;
        producer)
            nix build {{_nix_flags}} "$base.producerImage" -o result-cassini-producer-image
            podman load -i result-cassini-producer-image
            ;;
        sink)
            nix build {{_nix_flags}} "$base.sinkImage" -o result-cassini-sink-image
            podman load -i result-cassini-sink-image
            ;;
        client)
            nix build {{_nix_flags}} "$base.clientBin" -o result-cassini-client
            ;;
        *) echo "Unknown target: {{target}}. Use all, server, producer, sink, or client." && exit 1 ;;
    esac

# ── GitLab agents ─────────────────────────────────────────────────────────────

# Build gitlab agent images
# Usage: just gitlab            (all)
#        just gitlab observer
#        just gitlab consumer
gitlab target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.gitlabAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-gitlab-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-gitlab-consumer
            podman load -i result-gitlab-observer
            podman load -i result-gitlab-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-gitlab-observer
            podman load -i result-gitlab-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-gitlab-consumer
            podman load -i result-gitlab-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Kubernetes agents ─────────────────────────────────────────────────────────

# Build kubernetes agent images
# Usage: just kube              (all)
#        just kube observer
#        just kube consumer
kube target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.kubeAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-kube-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-kube-consumer
            podman load -i result-kube-observer
            podman load -i result-kube-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-kube-observer
            podman load -i result-kube-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-kube-consumer
            podman load -i result-kube-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Jira agents ───────────────────────────────────────────────────────────────

# Build jira agent images
# Usage: just jira              (all)
#        just jira observer
#        just jira consumer
jira target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.jiraAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-jira-consumer
            podman load -i result-jira-observer
            podman load -i result-jira-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            podman load -i result-jira-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-jira-consumer
            podman load -i result-jira-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Git agents ────────────────────────────────────────────────────────────────

# Build git agent images
# Usage: just git-agents        (all)
#        just git-agents observer
#        just git-agents consumer
#        just git-agents scheduler
git-agents target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.gitAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage"  -o result-git-observer
            nix build {{_nix_flags}} "$base.consumerImage"  -o result-git-consumer
            nix build {{_nix_flags}} "$base.schedulerImage" -o result-git-scheduler
            podman load -i result-git-observer
            podman load -i result-git-consumer
            podman load -i result-git-scheduler
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-git-observer
            podman load -i result-git-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-git-consumer
            podman load -i result-git-consumer
            ;;
        scheduler)
            nix build {{_nix_flags}} "$base.schedulerImage" -o result-git-scheduler
            podman load -i result-git-scheduler
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, consumer, or scheduler." && exit 1 ;;
    esac

# ── Web (OpenAPI) agents ──────────────────────────────────────────────────────

# Build web/openapi agent images
# Usage: just web               (all)
#        just web observer
#        just web consumer
web target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.webAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-web-observer
            nix build {{_nix_flags}} "$base.consumerImage" -o result-web-consumer
            podman load -i result-web-observer
            podman load -i result-web-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-web-observer
            podman load -i result-web-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-web-consumer
            podman load -i result-web-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Provenance agents ─────────────────────────────────────────────────────────

# Build provenance agent images
# Usage: just provenance        (all)
#        just provenance linker
#        just provenance resolver
provenance target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.provenance"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.linkerImage"   -o result-provenance-linker
            nix build {{_nix_flags}} "$base.resolverImage" -o result-provenance-resolver
            podman load -i result-provenance-linker
            podman load -i result-provenance-resolver
            ;;
        linker)
            nix build {{_nix_flags}} "$base.linkerImage" -o result-provenance-linker
            podman load -i result-provenance-linker
            ;;
        resolver)
            nix build {{_nix_flags}} "$base.resolverImage" -o result-provenance-resolver
            podman load -i result-provenance-resolver
            ;;
        *) echo "Unknown target: {{target}}. Use all, linker, or resolver." && exit 1 ;;
    esac

# ── Scheduler agents ──────────────────────────────────────────────────────────

# Build scheduler agent images
# Usage: just scheduler         (all)
#        just scheduler processor
#        just scheduler observer
scheduler target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.scheduler"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.processorImage" -o result-scheduler-processor
            nix build {{_nix_flags}} "$base.observerImage"  -o result-scheduler-observer
            podman load -i result-scheduler-processor
            podman load -i result-scheduler-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-scheduler-processor
            podman load -i result-scheduler-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-scheduler-observer
            podman load -i result-scheduler-observer
            ;;
        *) echo "Unknown target: {{target}}. Use all, processor, or observer." && exit 1 ;;
    esac

# ── Build orchestrator ────────────────────────────────────────────────────────

# Build orchestrator images
# Usage: just orchestrator          (all)
#        just orchestrator agent
#        just orchestrator clone
orchestrator target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.buildOrchestrator"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.orchestratorImage" -o result-orchestrator-image
            nix build {{_nix_flags}} "$base.cloneImage"        -o result-clone-image
            podman load -i result-orchestrator-image
            podman load -i result-clone-image
            ;;
        agent)
            nix build {{_nix_flags}} "$base.orchestratorImage" -o result-orchestrator-image
            podman load -i result-orchestrator-image
            ;;
        clone)
            nix build {{_nix_flags}} "$base.cloneImage" -o result-clone-image
            podman load -i result-clone-image
            ;;
        *) echo "Unknown target: {{target}}. Use all, agent, or clone." && exit 1 ;;
    esac

# ── Workspace binaries ────────────────────────────────────────────────────────

# Build all workspace binaries
agents:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default" -o polar

# Build TLS certificates for local testing
tls:
    nix build {{_nix_flags}} ".#packages.{{platform}}.tlsCerts" -o result-tlsCerts
    ls -alh result-tlsCerts

# ── Run containers ────────────────────────────────────────────────────────────

# Start the agent container (AMD ROCm — RX 9070 XT)
run-rocm:
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

# Start the agent container (NVIDIA CUDA — RTX 4000 Ada)
run-nvidia:
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
        -v `readlink -f /run/opengl-driver/lib/libcuda.so.1`:/run/opengl-driver/lib/libcuda.so.1:ro \
        -v `readlink -f /run/opengl-driver/lib/libcuda.so`:/run/opengl-driver/lib/libcuda.so:ro \
        -v `readlink -f /run/opengl-driver/lib/libnvidia-ml.so.1`:/run/opengl-driver/lib/libnvidia-ml.so.1:ro \
        localhost/polar-agent:latest

# Start the dev container
run-dev:
    podman run -it \
        -v ./:/workspace:rw \
        -e CREATE_USER="$USER" \
        -e CREATE_UID="$(id -u)" \
        -e CREATE_GID="$(id -g)" \
        polar-dev:latest

# Start the local docker-compose stack
run-compose:
    podman compose -f src/conf/docker-compose.yml up -d

# ── Inside container: LLM server ──────────────────────────────────────────────

# Start Qwen3-Coder-30B (AMD ROCm) — recommended
# Run in a separate terminal or tmux pane; does not background itself
llm-qwen:
    llama-server \
        --hf-repo unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF \
        --hf-file Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf \
        --ctx-size 65536 \
        --n-gpu-layers 35 \
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

# Start OmniCoder-9B (AMD ROCm) — fallback for lighter tasks
llm-omnicoder:
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
        -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1

# ── Inside container: Pi agent ────────────────────────────────────────────────

# Download pi (if not already present)
pi-install:
    cd ~ && curl -L https://github.com/badlogic/pi-mono/releases/download/v0.61.1/pi-linux-x64.tar.gz | tar -xz

# Launch pi (installs first if needed)
pi: pi-install
    ~/pi/pi

# ── Dev workflow ──────────────────────────────────────────────────────────────

# Check all crates compile
check:
    cargo check --workspace

# Run tests — optionally scoped to a single package
# Usage: just test
#        just test cassini-types
test package='':
    #!/usr/bin/env bash
    if [ -n "{{package}}" ]; then
        cargo test --package {{package}} -- --nocapture
    else
        cargo test --workspace -- --nocapture
    fi

# Run clippy with warnings as errors
lint:
    cargo clippy --workspace -- -D warnings

# Format all crates
fmt:
    cargo fmt --all

# Run static analysis tools
static-analysis:
    sh scripts/static-tools.sh --manifest-path src/agents/Cargo.toml

# Run CI pipeline locally
ci:
    podman run --rm \
        --name polar-dev \
        --env-file=ci.env \
        --env CI_COMMIT_REF_NAME=$(git symbolic-ref --short HEAD) \
        --env CI_COMMIT_SHORT_SHA=$(git rev-parse --short=8 HEAD) \
        --env SSL_CERT_FILE="./src/conf/certs/proxy-ca.pem" \
        --user 0 \
        --userns=keep-id \
        -v $(pwd):/workspace:rw \
        -p 2222:2223 \
        polar-dev:0.1.0 \
        bash -c "start.sh; chmod +x scripts/gitlab-ci.sh; chmod +x scripts/static-tools.sh; scripts/gitlab-ci.sh"
