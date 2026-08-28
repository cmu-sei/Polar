# Polar Justfile
#
# Host-side recipes: build images, run containers, dev workflow
# Container-side recipes: start llama-server, start pi, agent task management
#
# Usage:
#   just <recipe>                         # run with defaults
#   just platform=aarch64-linux <recipe>  # override platform
#   just verbose=false <recipe>           # quiet status line
#
# Platform autodetects from uname. Override for cross-builds.
# Verbose defaults to true (show full nix build logs).

platform := `echo "$(uname -m)-linux"`
verbose   := "true"

_nix_flags := if verbose == "true" { "-L" } else { "--log-format bar-with-logs" }

_taskfile := "/workspace/agent-task.json"

# ── Help ──────────────────────────────────────────────────────────────────────

# List all available recipes
default:
    @just --list

# ── Helpers (private) ─────────────────────────────────────────────────────────

# Load a docker-archive build result into the host podman.
# No-ops when podman is unavailable (e.g. inside the dev container, where
# podman-in-podman is explicitly not wanted and images reach the cluster via
# `just cluster-load-*` instead), or when POLAR_SKIP_LOAD is set. This lets the
# per-service build recipes run unchanged in both the dev container and on the
# host — in the container they build only; on the host they also load.
_load result:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${POLAR_SKIP_LOAD:-}" ]; then
        echo "POLAR_SKIP_LOAD set — skipping podman load of {{result}}"
        exit 0
    fi
    if ! command -v podman >/dev/null 2>&1; then
        echo "podman not found — skipping load of {{result}} (build-only)"
        exit 0
    fi
    podman load -i "{{result}}"

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
    just _load result-agent-container

# Build every package — binaries, all containers, TLS certs.
# Use this to verify the full build after disruptive changes.
build-all:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default"               -o result-binaries
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer"          -o result-dev-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.ciContainer"           -o result-ci-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainer"        -o result-agent-container
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerRocm"    -o result-agent-container-rocm
    nix build {{_nix_flags}} ".#packages.{{platform}}.agentContainerVulkan"  -o result-agent-container-vulkan
    nix build {{_nix_flags}} ".#packages.{{platform}}.tlsCerts"              -o result-tls-certs
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniImage"          -o result-cassini
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniProducerImage"  -o result-harness-producer
    nix build {{_nix_flags}} ".#packages.{{platform}}.cassiniSinkImage"      -o result-harness-sink
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitlabObserverImage"   -o result-gitlab-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitlabConsumerImage"   -o result-gitlab-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.kubeObserverImage"     -o result-kube-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.kubeConsumerImage"     -o result-kube-consumer
    nix build {{_nix_flags}} ".#packages.{{platform}}.openApiObserverImage"  -o result-openapi-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.openApiProcessorImage" -o result-openapi-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.polarPkgs.ociResolver.resolverImage" -o result-resolver
    nix build {{_nix_flags}} ".#packages.{{platform}}.schedulerProcessorImage" -o result-scheduler-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.schedulerObserverImage"  -o result-scheduler-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.jiraObserverImage"     -o result-jira-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.jiraProcessorImage"    -o result-jira-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitObserverImage"      -o result-git-observer
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitProcessorImage"     -o result-git-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitSchedulerImage"     -o result-git-scheduler
    nix build {{_nix_flags}} ".#packages.{{platform}}.buildProcessorImage"   -o result-build-processor
    nix build {{_nix_flags}} ".#packages.{{platform}}.nuInitImage"           -o result-nu-init
    nix build {{_nix_flags}} ".#packages.{{platform}}.polarInitImage"        -o result-polar-init
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitServerImage"        -o result-git-server

# ── Dev container ─────────────────────────────────────────────────────────────

# Build and load the dev container
dev-container:
    echo "Building dev container ({{platform}})..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.devContainer" -o result-dev-container
    just _load result-dev-container

# Build cert-issuer and cert-client images
# Usage: just cert-issuer          (all)
#        just cert-issuer server
#        just cert-issuer client
cert-issuer target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} ".#polarPkgs.certIssuer.server" -o result-cert-issuer-server
            nix build {{_nix_flags}} ".#polarPkgs.certIssuer.clientImage" -o result-cert-client
            nix build {{_nix_flags}} ".#polarPkgs.certIssuer.setupBin" -o result-cert-setup-bin
            nix build {{_nix_flags}} ".#packages.{{platform}}.certIssuerImage" -o result-cert-issuer-image
            just _load result-cert-issuer-image
            ;;
        serverImage)
            nix build {{_nix_flags}} ".#packages.{{platform}}.certIssuerImage" -o result-cert-issuer
            just _load result-cert-issuer
            ;;
        client)
            nix build {{_nix_flags}} ".#polarPkgs.certIssuer.clientImage" -o result-cert-client
            ;;
        *) echo "Unknown target: {{target}}. Use all, server, or client." && exit 1 ;;
    esac

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
            just _load result-cassini-image
            just _load result-cassini-producer-image
            just _load result-cassini-sink-image
            ;;
        server)
            nix build {{_nix_flags}} "$base.cassiniImage" -o result-cassini-image
            just _load result-cassini-image
            ;;
        producer)
            nix build {{_nix_flags}} "$base.producerImage" -o result-cassini-producer-image
            just _load result-cassini-producer-image
            ;;
        sink)
            nix build {{_nix_flags}} "$base.sinkImage" -o result-cassini-sink-image
            just _load result-cassini-sink-image
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
            just _load result-gitlab-observer
            just _load result-gitlab-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-gitlab-observer
            just _load result-gitlab-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-gitlab-consumer
            just _load result-gitlab-consumer
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
            just _load result-kube-observer
            just _load result-kube-consumer
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-kube-observer
            just _load result-kube-observer
            ;;
        consumer)
            nix build {{_nix_flags}} "$base.consumerImage" -o result-kube-consumer
            just _load result-kube-consumer
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or consumer." && exit 1 ;;
    esac

# ── Jira agents ───────────────────────────────────────────────────────────────
# Build jira agent images
# Usage: just jira              (all)
#        just jira observer
#        just jira processor
jira target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.jiraAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            nix build {{_nix_flags}} "$base.processorImage" -o result-jira-processor
            just _load result-jira-observer
            just _load result-jira-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-jira-observer
            just _load result-jira-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-jira-processor
            just _load result-jira-processor
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or processor." && exit 1 ;;
    esac

# ── Local dev PKI / cert-issuer stack ─────────────────────────────────────────
#
# One-time bootstrap sequence (run once per CA rotation):
#
#   just setup-cert-issuer                    # generates CA, OIDC keypair, JWKS, tokens, config
#
# Then, in separate terminals (or tmux panes):
#
#   just run-jwks                     # Terminal A: JWKS HTTP server (port 8081)
#   just run-cert-issuer                   # Terminal B: cert-issuer gRPC server (port 8443)
#
# Then issue all three certs and copy Neo4j layout:
#
#   just issue-certs                    # issues cassini, neo4j, and agent certs
#
# Print env vars for a given workload:
#
#   just dev-env cassini
#   just dev-env agent
#   just dev-env neo4j
#
# Tokens expire after 1h. Refresh without rotating the CA:
#
#   just                     # safe to re-run; preserves dev/tmp/
#
# Rotate the CA root (invalidates all outstanding certs):
#
#   just dev-clean ca && just dev-setup

# Run the one-shot dev environment setup (CA, OIDC keypair, JWKS, tokens, config).
# Safe to re-run — preserves dev/tmp/. Delete dev/tmp/ explicitly to rotate the CA.
setup-cert-issuer:
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-issuer-setup

# Start the JWKS HTTP server on port 8081.
# Run in a dedicated terminal — this is a foreground process.
# Must be running before dev-issuer starts.
run-jwks:
    python3 -m http.server 8081 --directory dev

# Start the cert-issuer on port 8443.
# Run in a dedicated terminal — this is a foreground process.
# Requires dev-jwks to be running (issuer fetches JWKS on startup).
run-cert-issuer:
    export CERT_ISSUER_CONFIG=dev/config.json
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-issuer

# Issue all three dev certs (cassini server, neo4j server, agent client).
# Requires both dev-jwks and dev-issuer to be running in separate terminals.
# Tokens expire after 1h; re-run dev-setup first if they've expired.
issue-dev-certs:
    #!/usr/bin/env bash
    set -euo pipefail

    # TODO: add healthcheck
    # Fail fast if the issuer is not reachable rather than producing a confusing TLS error.
    # if ! curl -sf --max-time 2 http://127.0.0.1:8443 > /dev/null 2>&1; then
    #     echo "ERROR: cert-issuer not responding on :8443 — run 'just dev-issuer' in another terminal first."
    #     exit 1
    # fi

    echo "--- Issuing Cassini server cert ---"
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-client -- \
        --cert-issuer-url http://127.0.0.1:8443 \
        --token-path dev/token-cassini \
        --cert-dir dev/certs/server \
        --cert-type server \
        --key-algorithm ecdsa-p256

    echo "--- Issuing Neo4j server cert ---"
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-client -- \
        --cert-issuer-url http://127.0.0.1:8443 \
        --token-path dev/token-neo4j \
        --cert-dir dev/certs/neo4j-server \
        --cert-type server \
        --key-algorithm ecdsa-p256 \
        --extra-san neo4j.polar.svc.cluster.local \
        --extra-san polar-db-svc.polar.svc.cluster.local

    for p in https bolt; do
        mkdir -p "dev/certs/neo4j/$p/trusted"
        cp dev/certs/neo4j-server/cert.pem "dev/certs/neo4j/$p/public.crt"
        cp dev/certs/neo4j-server/key.pem  "dev/certs/neo4j/$p/private.key"
        cp dev/certs/neo4j-server/ca.pem   "dev/certs/neo4j/$p/trusted/ca.pem"
        echo "  populated dev/certs/neo4j/$p/"
    done

    echo "--- Issuing agent client cert ---"
    cargo run --manifest-path ./src/agents/Cargo.toml --bin cert-client -- \
        --cert-issuer-url http://127.0.0.1:8443 \
        --token-path dev/token-agent \
        --cert-dir dev/certs/client \
        --key-algorithm ecdsa-p256

    echo "--- Verifying EKUs ---"
    for cert in dev/certs/server/cert.pem dev/certs/neo4j-server/cert.pem dev/certs/client/cert.pem; do
        echo "  $cert:"
        openssl x509 -in "$cert" -noout -text | grep -A5 'Extended Key Usage' | sed 's/^/    /'
    done

    echo "Done."



# ── Git agents ────────────────────────────────────────────────────────────────

# Build git agent images
# Usage: just git-agents        (all)
#        just git-agents observer
#        just git-agents consumer
git-agents target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.gitAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage"  -o result-git-observer
            nix build {{_nix_flags}} "$base.processorImage" -o result-git-processor
            just _load result-git-observer
            just _load result-git-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-git-observer
            just _load result-git-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-git-processor
            just _load result-git-processor
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, processor, or scheduler." && exit 1 ;;
    esac

# ── Build processor + resolver ──────────────────────────────────────────────
# Build the consolidated build-processor and resolver images.
# Usage: just build-agents           (all)
#        just build-agents processor
#        just build-agents resolver
build-agents target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} ".#packages.{{platform}}.buildProcessorImage" -o result-build-processor
            nix build {{_nix_flags}} ".#packages.{{platform}}.polarPkgs.ociResolver.resolverImage" -o result-resolver
            just _load result-build-processor
            just _load result-resolver
            ;;
        processor)
            nix build {{_nix_flags}} ".#packages.{{platform}}.buildProcessorImage" -o result-build-processor
            just _load result-build-processor
            ;;
        resolver)
            nix build {{_nix_flags}} ".#packages.{{platform}}.polarPkgs.ociResolver.resolverImage" -o result-resolver
            just _load result-resolver
            ;;
        *) echo "Unknown target: {{target}}. Use all, processor, or resolver." && exit 1 ;;
    esac

# ── OpenAPI agents ────────────────────────────────────────────────────────────

# Build openapi agent images
# Usage: just openapi               (all)
#        just openapi observer
#        just openapi processor
openapi target='all':
    #!/usr/bin/env bash
    set -euo pipefail
    base=".#packages.{{platform}}.polarPkgs.webAgent"
    case "{{target}}" in
        all)
            nix build {{_nix_flags}} "$base.observerImage"  -o result-openapi-observer
            nix build {{_nix_flags}} "$base.processorImage" -o result-openapi-processor
            just _load result-openapi-observer
            just _load result-openapi-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-openapi-observer
            just _load result-openapi-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-openapi-processor
            just _load result-openapi-processor
            ;;
        *) echo "Unknown target: {{target}}. Use all, observer, or processor." && exit 1 ;;
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
            just _load result-scheduler-processor
            just _load result-scheduler-observer
            ;;
        processor)
            nix build {{_nix_flags}} "$base.processorImage" -o result-scheduler-processor
            just _load result-scheduler-processor
            ;;
        observer)
            nix build {{_nix_flags}} "$base.observerImage" -o result-scheduler-observer
            just _load result-scheduler-observer
            ;;
        *) echo "Unknown target: {{target}}. Use all, processor, or observer." && exit 1 ;;
    esac

# ── Workspace binaries ────────────────────────────────────────────────────────

# Build all workspace binaries
agents:
    nix build {{_nix_flags}} ".#packages.{{platform}}.default" -o polar

# Build TLS certificates for local testing
tls:
    nix build {{_nix_flags}} ".#packages.{{platform}}.tlsCerts" -o result-tls-certs
    ls -alh result-tls-certs

# Build the nu-init infrastructure container
nu-init:
    echo "Building nu-init container..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.nuInitImage" -o result-nu-init
    just _load result-nu-init

polar-init:
    echo "Building polar-init container..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.polarInitImage" -o result-polar-init
    just _load result-polar-init

ace:
    echo "Building ace container..."
    nix build {{_nix_flags}} ".#packages.{{platform}}.aceImage" -o result-ace
    just _load result-ace

git-server:
    nix build {{_nix_flags}} ".#packages.{{platform}}.gitServerImage" -o result-git-server-image
    just _load result-git-server-image

# Update the nix-container-lib pin in container-lib.dhall and re-render all container.nix files.
update-dhall-pins:
    #!/usr/bin/env bash
    set -euo pipefail
    COMMIT=$(nix flake metadata --json | jq -r '.locks.nodes["nix-container-lib"].locked.rev')
    DHALL_HASH=$(dhall hash <<< "https://raw.githubusercontent.com/daveman1010221/nix-container-lib/${COMMIT}/dhall/prelude.dhall")
    echo "nix-container-lib commit: $COMMIT"
    echo "Dhall prelude hash:       $DHALL_HASH"
    echo "Updating src/containers/container-lib.dhall..."
    echo "Re-rendering container.nix files..."
    for f in $(grep -rl "container-lib.dhall" src/ --include="*.dhall" | grep -v 'container-lib.dhall'); do
        DIR="$(dirname $f)"
        BASE="$(basename $f .dhall)"
        pushd "$DIR" > /dev/null
        dhall-to-nix < "$BASE.dhall" > "$BASE.nix" && echo "  rendered: $DIR/$BASE.nix" || echo "  skipped:  $f"
        popd > /dev/null
    done
    echo "Done. Review with: git diff"

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
        -e CREATE_USER="$USER" \
        -e CREATE_UID="$(id -u)" \
        -e CREATE_GID="$(id -g)" \
        -e ATUIN_SESSION_NAME=polar-dev \
        -e HIP_VISIBLE_DEVICES=0 \
        -v /sys/class/kfd:/sys/class/kfd:ro \
        -v /sys/bus/pci/devices:/sys/bus/pci/devices:ro \
        -v ~/Documents/projects/ai_models/llama:/opt/llama-models:rw \
        -v ~/Documents/projects/ai_models/llama:/opt/ollama:rw \
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
        -e OLLAMA_HOST="0.0.0.0:8080" \
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
run-dev-container:
    podman run --rm --name polar-dev -it \
        --user 0 --userns=keep-id \
        -e CREATE_USER="$USER" \
        -e CREATE_UID="$(id -u)" \
        -e CREATE_GID="$(id -g)" \
        -e ATUIN_SESSION_NAME=polar-dev \
        -v $PWD:/workspace \
        -v $HOME/.config/atuin:/root/.config/atuin:ro \
        -v $HOME/.local/share/atuin:/root/.local/share/atuin \
        -p 2222:2223 \
        localhost/polar-dev:latest

# ── Inside container: LLM server ──────────────────────────────────────────────

# Start Qwen3-Coder-30B (AMD ROCm) — recommended
# Run in a separate terminal or tmux pane; does not background itself
llm-qwen:
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

# Start OmniCoder-9B (AMD ROCm) — fallback for lighter tasks
llm-omnicoder-rocm-16GB:
    llama-server \
        --hf-repo Tesslate/OmniCoder-9B-GGUF \
        --hf-file omnicoder-9b-q8_0.gguf \
        --ctx-size 262144 \
        --n-gpu-layers 99 \
        --flash-attn on \
        --alias "local-model" \
        --port 8080 --host 0.0.0.0 \
        --temperature 0.3 \
        --top-p 0.95 \
        --top-k 20 \
        --min-p 0.0 \
        --repeat-penalty 1.0 \
        --jinja \
        --spec-type ngram-mod \
        -ub 512 \
        -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1

llm-omnicoder-nvidia-12GB:
    llama-server \
        --hf-repo Tesslate/OmniCoder-9B-GGUF \
        --hf-file omnicoder-9b-q8_0.gguf \
        --ctx-size 131072 \
        --n-gpu-layers 99 \
        --flash-attn on \
        --alias "local-model" \
        --port 8080 --host 0.0.0.0 \
        --temperature 0.3 \
        --top-p 0.95 \
        --top-k 20 \
        --min-p 0.0 \
        --repeat-penalty 1.0 \
        --jinja \
        --spec-type ngram-mod \
        -ub 512 \
        -ctk q8_0 -ctv q8_0 > /tmp/llama-server.log 2>&1

# ── Inside container: Pi agent ────────────────────────────────────────────────

pi_version := ""

# Download/update pi to latest version
pi-install:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(curl -fsSL https://api.github.com/repos/badlogic/pi-mono/releases/latest | grep '"tag_name"' | sed 's/.*"v\([^"]*\)".*/\1/')
    INSTALLED=""
    if [ -f ~/pi/pi ]; then
        INSTALLED=$(~/pi/pi --version 2>/dev/null | grep -oP '\d+\.\d+\.\d+' || echo "")
    fi
    if [ "$INSTALLED" = "$VERSION" ]; then
        echo "pi v${VERSION} already installed, skipping."
        exit 0
    fi
    echo "Installing pi v${VERSION}..."
    curl -fsSL "https://github.com/badlogic/pi-mono/releases/download/v${VERSION}/pi-linux-x64.tar.gz" \
        | tar -xz -C ~
    echo "pi v${VERSION} installed."

# Launch pi (installs/updates first)
pi: pi-install
    ~/pi/pi @/workspace/agent-task.json

# ── Agent task management (container-side) ────────────────────────────────────

# Show current agent task status
agent-status:
    #!/usr/bin/env bash
    if [ ! -f {{_taskfile}} ]; then
        echo "No task file found at {{_taskfile}}"
        exit 1
    fi
    echo "=== Current Task ==="
    jq '.current_task' {{_taskfile}}
    echo ""
    echo "=== Pending ($(jq '.pending | length' {{_taskfile}}) tasks) ==="
    jq '.pending[] | {crate, op, review}' {{_taskfile}}
    echo ""
    echo "=== Completed ($(jq '.completed | length' {{_taskfile}}) tasks) ==="
    jq '.completed[] | {crate, op, status}' {{_taskfile}}

# Resume agent from existing task file — prints the resume prompt to paste into pi
agent-resume:
    #!/usr/bin/env bash
    if [ ! -f {{_taskfile}} ]; then
        echo "No task file found at {{_taskfile}}"
        exit 1
    fi
    WORKSPACE_ROOT=$(jq -r '.workspace_root' {{_taskfile}})
    CRATE=$(jq -r '.current_task.crate' {{_taskfile}})
    OP=$(jq -r '.current_task.op' {{_taskfile}})
    echo "Paste this into pi to resume:"
    echo "────────────────────────────────────────────────────────────────"
    printf "export TASKFILE=%s WORKSPACE_ROOT=%s\n" "{{_taskfile}}" "$WORKSPACE_ROOT"
    printf "Read your task file: jq '.' \$TASKFILE\n"
    printf "Your current task is .current_task — crate=%s op=%s\n" "$CRATE" "$OP"
    printf "Your tools are in .tools — substitute {placeholders} as needed. Use locate tools first to find symbols, then extract tools to read exact definitions.\n"
    printf "Begin: cd \$WORKSPACE_ROOT && cargo check --package %s 2>&1 | head -80\n" "$CRATE"
    printf "Follow .current_task.next_action. Update .current_task.current_file as you move between files. When .current_task.success_condition is met run the taskfile.mark_done tool to advance to the next pending task. Do not ask for clarification. Start working.\n"
    echo "────────────────────────────────────────────────────────────────"

# Set agent to work on a specific crate and op, or pop next pending task
# Usage: just agent-task                        # pop next pending, launch pi
#        just agent-task cassini-client write-tests  # queue explicit task, launch pi
agent-task crate='' op='':
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f {{_taskfile}} ]; then
        echo "No task file found at {{_taskfile}}"
        exit 1
    fi

    if [ -z "{{crate}}" ]; then
        # No args — pop first pending task into current_task
        PENDING_LEN=$(jq '.pending | length' {{_taskfile}})
        if [ "$PENDING_LEN" -eq 0 ]; then
            echo "No pending tasks. Add tasks to pending in {{_taskfile}} first."
            exit 1
        fi
        jq '
            .current_task = .pending[0] |
            .pending = .pending[1:]
        ' {{_taskfile}} > {{_taskfile}}.tmp && mv {{_taskfile}}.tmp {{_taskfile}}
    else
        # Explicit crate/op — push current_task to back of pending if one exists
        jq --arg crate "{{crate}}" --arg op "{{op}}" '
            if .current_task != null then
                .pending = .pending + [.current_task]
            else
                .
            end |
            .current_task = {
                "crate": $crate,
                "op": $op,
                "status": "in-progress"
            }
        ' {{_taskfile}} > {{_taskfile}}.tmp && mv {{_taskfile}}.tmp {{_taskfile}}
    fi

    echo "=== Current Task ==="
    jq '.current_task' {{_taskfile}}
    echo ""

    # Launch pi
    just pi

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

# Install neo4j mTLS client cert into the browser's NSS certificate store.
# Fetches fresh certs from the running cluster and imports into Librewolf.
# Usage: just neo4j-install-certs
neo4j-install-certs:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Waiting for a Running Neo4j-client pod..."
    AGENT_POD=""
    AGENT_CONTAINER=""
    for i in $(seq 1 30); do
        for LABEL in kube-consumer jira-processor build-processor git-repo-consumer polar-gitlab-consumer artifact-linker polar-scheduler; do
            CANDIDATE=$(kubectl get pod -n polar -l name="$LABEL" --field-selector=status.phase=Running -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
            if [ -n "$CANDIDATE" ]; then
                AGENT_POD="$CANDIDATE"
                AGENT_CONTAINER="$LABEL"
                break 2
            fi
        done
        sleep 2
    done
    if [ -z "$AGENT_POD" ]; then
        echo "ERROR: no Running Neo4j-client pod found after 60s" >&2
        exit 1
    fi
    echo "Fetching certs from $AGENT_POD (container: $AGENT_CONTAINER)..."
    kubectl exec -n polar "$AGENT_POD" -c "$AGENT_CONTAINER" -- cat /etc/neo4j-client-tls/cert.pem > /tmp/neo4j-client-cert.pem
    kubectl exec -n polar "$AGENT_POD" -c "$AGENT_CONTAINER" -- cat /etc/neo4j-client-tls/key.pem  > /tmp/neo4j-client-key.pem
    kubectl exec -n polar-graph polar-neo4j-0 -- cat /var/lib/neo4j/certificates/https/trusted/ca.pem > /tmp/neo4j-ca.pem
    echo "Bundling PKCS12..."
    openssl pkcs12 -export \
        -out /tmp/neo4j-client.p12 \
        -inkey /tmp/neo4j-client-key.pem \
        -in /tmp/neo4j-client-cert.pem \
        -certfile /tmp/neo4j-ca.pem \
        -name "Polar Neo4j Client" \
        -passout pass:changeit
    NSS_DB=~/.librewolf/$(ls ~/.librewolf/ | head -1)
    echo "Importing into Librewolf NSS db at $NSS_DB..."
    nix-shell -p nssTools --run "pk12util -i /tmp/neo4j-client.p12 -d '$NSS_DB' -W changeit"
    nix-shell -p nssTools --run "certutil -M -n 'Polar Internal CA' -t 'CT,,' -d '$NSS_DB'"
    echo "Done. Port-forward and browse to https://localhost:7473"

# Load a neo4j dump file into the cluster database.
# Scales down neo4j, runs a loader pod as root, chowns the data, scales back up,
# and resets the neo4j password to match the cluster secret.
# Usage: just neo4j-load-dump ~/Documents/projects/jira.5.26.26.neo4j.dump
neo4j-load-dump dump_path:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f "{{dump_path}}" ]; then
        echo "ERROR: dump file not found: {{dump_path}}"
        exit 1
    fi
    NEO4J_PASSWORD=$(kubectl get secret -n polar-graph neo4j-secret -o jsonpath='{.data.secret}' | base64 -d | cut -d'/' -f2)
    echo "Scaling neo4j down..."
    kubectl scale statefulset -n polar-graph polar-neo4j --replicas=0
    kubectl wait --for=delete pod/polar-neo4j-0 -n polar-graph --timeout=120s 2>/dev/null || true
    echo "Launching loader pod..."
    kubectl run -n polar-graph neo4j-loader \
        --image=neo4j:5.26.2 \
        --restart=Never \
        --overrides='{"spec":{"securityContext":{"runAsUser":0,"runAsGroup":0},"volumes":[{"name":"data","persistentVolumeClaim":{"claimName":"polar-db-data"}},{"name":"logs","persistentVolumeClaim":{"claimName":"polar-db-logs"}}],"containers":[{"name":"neo4j-loader","image":"neo4j:5.26.2","command":["/bin/sh","-c","sleep 3600"],"volumeMounts":[{"name":"data","mountPath":"/var/lib/neo4j/data"},{"name":"logs","mountPath":"/var/lib/neo4j/logs"}]}]}}'
    kubectl wait --for=condition=Ready pod/neo4j-loader -n polar-graph --timeout=60s
    echo "Copying dump file..."
    cat "{{dump_path}}" | kubectl exec -i -n polar-graph neo4j-loader -- /bin/sh -c 'cat > /var/lib/neo4j/data/neo4j.dump'
    echo "Loading dump..."
    kubectl exec -n polar-graph neo4j-loader -- /bin/sh -c \
        'mkdir -p /var/lib/neo4j/tmp && NEO4J_HOME=/var/lib/neo4j NEO4J_CONF=/var/lib/neo4j/conf JAVA_TOOL_OPTIONS="-Djava.io.tmpdir=/var/lib/neo4j/tmp" /nix/store/qbds1qr8sf5qkzz8cady90bwhcnw05xf-neo4j-community-src-2025.8.0/opt/neo4j/bin/neo4j-admin database load neo4j --from-path=/var/lib/neo4j/data --overwrite-destination=true'
    echo "Fixing ownership..."
    kubectl exec -n polar-graph neo4j-loader -- /bin/sh -c 'chown -R 7474:7474 /var/lib/neo4j/data && rm /var/lib/neo4j/data/neo4j.dump'
    echo "Cleaning up loader pod..."
    kubectl delete pod -n polar-graph neo4j-loader
    echo "Scaling neo4j back up..."
    kubectl scale statefulset -n polar-graph polar-neo4j --replicas=1
    echo "Waiting for neo4j to be ready..."
    kubectl wait --for=condition=Ready pod/polar-neo4j-0 -n polar-graph --timeout=180s
    echo "Waiting for neo4j to finish initializing..."
    sleep 15
    echo "Resetting neo4j password..."
    kubectl exec -n polar-graph polar-neo4j-0 -- /nix/store/qbds1qr8sf5qkzz8cady90bwhcnw05xf-neo4j-community-src-2025.8.0/opt/neo4j/bin/cypher-shell \
        -u neo4j -p neo4j \
        "ALTER USER neo4j SET PASSWORD '$NEO4J_PASSWORD' CHANGE NOT REQUIRED" || \
    kubectl exec -n polar-graph polar-neo4j-0 -- /nix/store/qbds1qr8sf5qkzz8cady90bwhcnw05xf-neo4j-community-src-2025.8.0/opt/neo4j/bin/cypher-shell \
        -u neo4j -p "$NEO4J_PASSWORD" \
        "ALTER USER neo4j SET PASSWORD '$NEO4J_PASSWORD' CHANGE NOT REQUIRED"
    echo "Done. Run 'just neo4j-install-certs' to update browser certs."

# ── nix-usernetes (local cluster) ────────────────────────────────────────────
#
# Full cluster startup sequence (for developers using nix-usernetes locally;
# users bringing their own cluster should skip this section entirely):
#
#   cd ~/Documents/projects/nix-usernetes
#   just load-node-image   # after every nix build
#   just reset && just up && just init && just kubeconfig
#   export KUBECONFIG=$(pwd)/kubeconfig
#
#   cd ~/Documents/projects/Polar
#   just cluster-build-all && just cluster-load-all
#   nu infra/render.nu local-minimal --apply

_u7s_dir      := env_var('HOME') + "/Documents/projects/nix-usernetes"
_u7s_kubeconf := _u7s_dir + "/kubeconfig"

# Wipe all cluster state
cluster-reset:
    just -f {{_u7s_dir}}/Justfile reset

# Show cluster status. Respects whatever KUBECONFIG is already set.
cluster-status:
    kubectl get nodes
    kubectl get pods -A


# Start detached, auto-restarting port-forwards for local dev (neo4j
# browser/bolt, keycloak admin console, ace dashboard, jaeger traces/UI).
# Fully detached via setsid -- survives terminal close, does not appear
# in `jobs`. Logs go to /tmp/pf-*.log; stop with `just port-forwards-down`.
port-forwards-up:
    #!/usr/bin/env bash
    set -euo pipefail
    start_pf() {
        local ns="$1" svc="$2" ports="$3" logfile="$4"
        setsid bash -c "while true; do kubectl port-forward -n $ns svc/$svc $ports > $logfile 2>&1; sleep 1; done" < /dev/null > /dev/null 2>&1 &
        disown
    }
    start_pf polar-graph polar-db-svc "7473:7473 7687:7687" /tmp/pf-neo4j.log
    start_pf polar-auth  keycloak-svc "8080:8080"            /tmp/pf-keycloak.log
    start_pf polar       ace-svc      "8050:8050"             /tmp/pf-ace.log
    start_pf polar       jaeger-svc   "4318:4318 16686:16686" /tmp/pf-jaeger.log
    echo "Port-forwards started (neo4j: 7473/7687, keycloak: 8080, ace: 8050, jaeger: 4318/16686)."
    echo "Logs: /tmp/pf-neo4j.log /tmp/pf-keycloak.log /tmp/pf-ace.log /tmp/pf-jaeger.log"

# Stop all detached port-forwards started by port-forwards-up.
port-forwards-down:
    #!/usr/bin/env bash
    pkill -f "port-forward -n polar-graph svc/polar-db-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar-auth svc/keycloak-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/ace-svc" 2>/dev/null || true
    pkill -f "port-forward -n polar svc/jaeger-svc" 2>/dev/null || true
    echo "Port-forwards stopped."

# Idempotent Keycloak bootstrap: realm, public client (PKCE, Authorization
# Code + direct-grant enabled for scripted testing), audience mapper
# scoping tokens to cert-issuer, and a test user. Safe to re-run --
# every step checks for existence first. Test user credentials are read
# directly from sops (secrets.sops.yaml / user.sops.yaml), never stored
# as a Kubernetes Secret -- nothing in the cluster ever consumes them,
# so there's no reason for them to touch cluster storage at all.
#
# Known Keycloak gotchas this script works around (found the hard way):
#   - kcadm set-password defaults to a *temporary* password, which
#     attaches a required action and blocks direct-grant login entirely
#     until resolved -- creating the user with an inline credentials
#     array (temporary=false) sidesteps this in one step.
#   - the realm's default User Profile requires email/firstName/lastName;
#     a user missing any of these fails direct-grant with the generic,
#     unhelpful "Account is not fully set up" error, indistinguishable
#     from the temporary-password case without checking both.
#   - kcadm's `-q` search does a *partial* match by default (e.g.
#     username=testuser also matches "testuser2") -- every existence
#     check below passes `-q exact=true` explicitly to avoid a false
#     "already exists" against an unrelated, similarly-named resource.
keycloak-bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail

    KC_NS="polar-auth"
    KC_URL="http://localhost:8080"
    KC_REALM="polar"
    KC_CLIENT_ID="browser-cert-fetcher"
    KC_AUDIENCE="polar-cert-issuer.local"
    KC_REDIRECT_URI="http://localhost:8765/callback"

    kc() {
        kubectl exec -n "$KC_NS" deployment/keycloak -- /opt/keycloak/bin/kcadm.sh "$@"
    }

    echo "Logging into kcadm..."
    KC_ADMIN_USER=$(kubectl get secret -n "$KC_NS" keycloak-admin-secret -o jsonpath="{.data.username}" | base64 -d)
    KC_ADMIN_PASSWORD=$(kubectl get secret -n "$KC_NS" keycloak-admin-secret -o jsonpath="{.data.password}" | base64 -d)
    kc config credentials --server "$KC_URL" --realm master --user "$KC_ADMIN_USER" --password "$KC_ADMIN_PASSWORD"

    echo "Ensuring realm '$KC_REALM' exists..."
    REALM_CHECK=$(kc get "realms/$KC_REALM" 2>/dev/null || echo "{}")
    if [ "$(echo "$REALM_CHECK" | jq -r ".realm // empty")" != "$KC_REALM" ]; then
        kc create realms -s realm="$KC_REALM" -s enabled=true
        echo "  created"
    else
        echo "  already exists"
    fi

    echo "Ensuring client '$KC_CLIENT_ID' exists..."
    CLIENT_LIST=$(kc get clients -r "$KC_REALM" -q clientId="$KC_CLIENT_ID" -q exact=true 2>/dev/null || echo "[]")
    CLIENT_ID_INTERNAL=$(echo "$CLIENT_LIST" | jq -r ".[0].id // empty")
    if [ -z "$CLIENT_ID_INTERNAL" ]; then
        kc create clients -r "$KC_REALM" \
            -s clientId="$KC_CLIENT_ID" \
            -s publicClient=true \
            -s standardFlowEnabled=true \
            -s directAccessGrantsEnabled=true \
            -s "redirectUris=[\"$KC_REDIRECT_URI\"]" \
            -s "attributes={\"pkce.code.challenge.method\":\"S256\"}"
        CLIENT_ID_INTERNAL=$(kc get clients -r "$KC_REALM" -q clientId="$KC_CLIENT_ID" -q exact=true | jq -r ".[0].id")
        echo "  created (id=$CLIENT_ID_INTERNAL)"
    else
        echo "  already exists (id=$CLIENT_ID_INTERNAL)"
    fi

    echo "Ensuring audience mapper exists..."
    MAPPERS=$(kc get "clients/$CLIENT_ID_INTERNAL/protocol-mappers/models" -r "$KC_REALM" 2>/dev/null || echo "[]")
    MAPPER_EXISTS=$(echo "$MAPPERS" | jq -r ".[] | select(.name==\"polar-audience\") | .id // empty")
    if [ -z "$MAPPER_EXISTS" ]; then
        kc create "clients/$CLIENT_ID_INTERNAL/protocol-mappers/models" -r "$KC_REALM" \
            -s name=polar-audience \
            -s protocol=openid-connect \
            -s protocolMapper=oidc-audience-mapper \
            -s "config={\"included.custom.audience\":\"$KC_AUDIENCE\",\"id.token.claim\":\"false\",\"access.token.claim\":\"true\"}"
        echo "  created"
    else
        echo "  already exists"
    fi

    echo "Ensuring test user exists..."
    TARGET_DIR="infra/targets/local-minimal"
    SHARED_JSON=$(SOPS_CONFIG="$TARGET_DIR/.sops.yaml" sops --decrypt --output-type json "$TARGET_DIR/secrets.sops.yaml" 2>/dev/null || echo "{}")
    USER_JSON=$(SOPS_CONFIG="$TARGET_DIR/.user.sops.yaml" sops --decrypt --output-type json "$TARGET_DIR/user.sops.yaml" 2>/dev/null || echo "{}")
    MERGED_JSON=$(jq -s ".[0] * .[1]" <(echo "$SHARED_JSON") <(echo "$USER_JSON"))
    KC_TEST_USERNAME=$(echo "$MERGED_JSON" | jq -r ".keycloak_test_username // empty")
    KC_TEST_PASSWORD=$(echo "$MERGED_JSON" | jq -r ".keycloak_test_password // empty")
    if [ -z "$KC_TEST_USERNAME" ] || [ -z "$KC_TEST_PASSWORD" ]; then
        echo "  ERROR: keycloak_test_username/keycloak_test_password not set in secrets.sops.yaml or user.sops.yaml" >&2
        exit 1
    fi
    USER_LIST=$(kc get users -r "$KC_REALM" -q username="$KC_TEST_USERNAME" -q exact=true 2>/dev/null || echo "[]")
    USER_EXISTS=$(echo "$USER_LIST" | jq -r ".[0].id // empty")
    if [ -z "$USER_EXISTS" ]; then
        kc create users -r "$KC_REALM" \
            -s username="$KC_TEST_USERNAME" \
            -s enabled=true \
            -s emailVerified=true \
            -s firstName=Test \
            -s lastName=User \
            -s email="$KC_TEST_USERNAME@polar.local" \
            -s "credentials=[{\"type\":\"password\",\"value\":\"$KC_TEST_PASSWORD\",\"temporary\":false}]"
        echo "  created"
    else
        echo "  already exists"
    fi

    echo "keycloak-bootstrap: done"
# Print nix-usernetes kubeconfig export command (dev convenience only).
cluster-ctx:
    @echo "Run: export KUBECONFIG={{_u7s_kubeconf}}"

# Load a nix result or podman image into the cluster containerd
# Usage: just cluster-load ./result-cassini-image
#        just cluster-load cassini:latest
cluster-load image:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -e "{{image}}" ]; then
        podman exec -i u7s ctr -n k8s.io images import - < "{{image}}"
    else
        podman save "{{image}}" | podman exec -i u7s ctr -n k8s.io images import -
    fi

_neo4j_result := env_var('HOME') + "/Documents/projects/nix-neo4j/result-neo4j-image"
_registry_host := "localhost"
_registry_port := "5000"
_registry_gateway := "10.0.2.2"   # how u7s reaches the host registry (slirp4netns gateway)
_cluster_runtime := "u7s"   # set to anything else (e.g. "none") if your cluster isn't u7s/podman-based -- skips the containerd image-cache cleanup below, which is u7s-specific and doesn't translate to other runtimes (kind, k3d, Docker Desktop Kubernetes, etc.)

# Start a local OCI registry for image distribution. Works with any
# Kubernetes cluster (u7s, kind, k3d, a real cluster) -- push here from
# the host, and configure your cluster's node(s) to pull from wherever
# this registry is reachable on their end.
registry-up:
    podman run -d --name polar-registry -p {{_registry_port}}:{{_registry_port}} docker.io/library/registry:2 2>/dev/null || echo "Container 'polar-registry' already running."

registry-down:
    podman rm -f polar-registry

registry-status:
    curl -sf http://{{_registry_host}}:{{_registry_port}}/v2/ && echo "registry reachable"

# Build all images needed for cluster deployment with correct result symlink names
cluster-build-all:
    just cert-issuer all
    just cassini all
    just gitlab all
    just kube all
    just scheduler all
    just git-agents all
    just jira all
    just openapi all
    just build-agents all
    just nu-init
    just polar-init
    just ace

# Load only the images needed by the local-minimal target:
# neo4j, cassini, cert-issuer, polar-init, kube observer/consumer, jira observer/processor.
# Pushes to the local registry (see registry-up) rather than importing
# directly into a specific cluster's containerd -- works with any
# Kubernetes distribution, not just u7s.
cluster-load-minimal neo4j_result=_neo4j_result: registry-up cluster-load-provisioner
    #!/usr/bin/env bash
    set -euo pipefail
    _push() {
        if [ -e "$1" ]; then
            echo "Pushing $1 -> {{_registry_host}}:{{_registry_port}}/$2"
            skopeo copy "docker-archive:$1" "docker://{{_registry_host}}:{{_registry_port}}/$2" --dest-tls-verify=false
            if [ "{{_cluster_runtime}}" = "u7s" ]; then
                podman exec u7s ctr -n k8s.io images rm "{{_registry_gateway}}:{{_registry_port}}/$2" > /dev/null 2>&1 || true
            fi
        else
            echo "Skipping $1 (not found)"
        fi
    }
    # Infrastructure
    if [ -n "{{neo4j_result}}" ]; then
        _push "{{neo4j_result}}" "neo4j:5.26.2"
    fi
    # Minimal-target images only
    _push ./result-cassini-image  "cassini:latest"
    _push ./result-cert-issuer-image "cert-issuer:latest"
    _push ./result-cert-client    "polar-cert-client:latest"
    _push ./result-polar-init     "polar-init:latest"
    _push ./result-kube-observer  "kube-observer:latest"
    _push ./result-ace            "ace-viz:latest"
    _push ./result-kube-consumer  "kube-consumer:latest"
    _push ./result-jira-observer  "jira-observer:latest"
    _push ./result-jira-processor "jira-processor:latest"
    _push ./result-nu-init        "polar-nu-init:latest"
    echo "Minimal images pushed to {{_registry_host}}:{{_registry_port}}."

# Build and load all core Polar images into the cluster.
# All images are loaded directly from nix result symlinks.
# Usage: just cluster-load-all
#        just cluster-load-all neo4j_result=../nix-neo4j/result-neo4j-image
cluster-load-all neo4j_result=_neo4j_result: registry-up cluster-load-provisioner
    #!/usr/bin/env bash
    set -euo pipefail
    _push() {
        if [ -e "$1" ]; then
            echo "Pushing $1 -> {{_registry_host}}:{{_registry_port}}/$2"
            skopeo copy "docker-archive:$1" "docker://{{_registry_host}}:{{_registry_port}}/$2" --dest-tls-verify=false
            if [ "{{_cluster_runtime}}" = "u7s" ]; then
                podman exec u7s ctr -n k8s.io images rm "{{_registry_gateway}}:{{_registry_port}}/$2" > /dev/null 2>&1 || true
            fi
        else
            echo "Skipping $1 (not found)"
        fi
    }
    # Infrastructure
    if [ -n "{{neo4j_result}}" ]; then
        _push "{{neo4j_result}}" "neo4j:5.26.2"
    fi
    # Polar images — all from nix result symlinks
    _push ./result-cassini-image       "cassini:latest"
    _push ./result-cert-issuer-image    "cert-issuer:latest"
    _push ./result-cert-client         "polar-cert-client:latest"
    _push ./result-gitlab-observer     "gitlab-observer:latest"
    _push ./result-gitlab-consumer     "gitlab-consumer:latest"
    _push ./result-kube-observer       "kube-observer:latest"
    _push ./result-kube-consumer       "kube-consumer:latest"
    _push ./result-git-observer        "git-observer:latest"
    _push ./result-git-processor       "git-processor:latest"
    _push ./result-git-scheduler       "git-scheduler:latest"
    _push ./result-jira-observer       "jira-observer:latest"
    _push ./result-jira-processor      "jira-processor:latest"
    _push ./result-openapi-observer    "openapi-observer:latest"
    _push ./result-openapi-processor   "openapi-processor:latest"
    _push ./result-resolver            "oci-resolver:latest"
    _push ./result-scheduler-observer  "scheduler-observer:latest"
    _push ./result-scheduler-processor "scheduler-processor:latest"
    _push ./result-build-processor     "build-processor:latest"
    _push ./result-nu-init             "polar-nu-init:latest"
    _push ./result-polar-init          "polar-init:latest"
    _push ./result-ace                 "ace-viz:latest"
    echo "Core images pushed to {{_registry_host}}:{{_registry_port}}."

# Pull and push the local-path provisioner image to the local registry.

# Push Cassini's own message-flow test harness images (producer/sink,
# cassini/test/producer + cassini/test/sink) to the registry. Not part
# of cluster-load-all/cluster-load-minimal -- these are test tooling,
# not real Polar agents, and most Cassini development happens locally
# without a cluster at all. Opt-in only, for the times you do want to
# exercise the harness against a live deployment.
cluster-load-cassini-harness: registry-up
    #!/usr/bin/env bash
    set -euo pipefail
    _push() {
        if [ -e "$1" ]; then
            echo "Pushing $1 -> {{_registry_host}}:{{_registry_port}}/$2"
            skopeo copy "docker-archive:$1" "docker://{{_registry_host}}:{{_registry_port}}/$2" --dest-tls-verify=false
            if [ "{{_cluster_runtime}}" = "u7s" ]; then
                podman exec u7s ctr -n k8s.io images rm "{{_registry_gateway}}:{{_registry_port}}/$2" > /dev/null 2>&1 || true
            fi
        else
            echo "Skipping $1 (not found)"
        fi
    }
    _push ./result-cassini-producer-image "harness-producer:latest"
    _push ./result-cassini-sink-image     "harness-sink:latest"
    echo "Cassini test harness images pushed to {{_registry_host}}:{{_registry_port}}."

# Mirror jaeger's third-party image into the local registry. Unlike
# every other image in cluster-load-all/-minimal, jaeger isn't
# Nix-built -- there's no result-jaeger symlink to push from, so this
# needs its own recipe rather than fitting the existing _push pattern.
# Was previously a manual, undocumented skopeo copy run by hand after
# every registry/cluster reset.
registry-load-jaeger: registry-up
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -m)" in
        x86_64) arch="amd64" ;;
        aarch64|arm64) arch="arm64" ;;
        *) arch="$(uname -m)" ;;
    esac
    skopeo copy --override-os linux --override-arch "$arch" docker://cr.jaegertracing.io/jaegertracing/jaeger:2.13.0 "docker://{{_registry_host}}:{{_registry_port}}/jaeger:2.13.0" --dest-tls-verify=false
    if [ "{{_cluster_runtime}}" = "u7s" ]; then
        podman exec u7s ctr -n k8s.io images rm "{{_registry_gateway}}:{{_registry_port}}/jaeger:2.13.0" > /dev/null 2>&1 || true
    fi
    echo "jaeger:2.13.0 pushed to {{_registry_host}}:{{_registry_port}}."

cluster-load-provisioner:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -m)" in
        x86_64) arch="amd64" ;;
        aarch64|arm64) arch="arm64" ;;
        *) arch="$(uname -m)" ;;
    esac
    img="docker.io/rancher/local-path-provisioner:v0.0.35"
    echo "Pushing $img -> {{_registry_host}}:{{_registry_port}}/local-path-provisioner:v0.0.35"
    skopeo copy --override-os linux --override-arch "$arch" "docker://$img" "docker://{{_registry_host}}:{{_registry_port}}/local-path-provisioner:v0.0.35" --dest-tls-verify=false
    skopeo copy --override-os linux --override-arch "$arch" "docker://docker.io/library/busybox:latest" "docker://{{_registry_host}}:{{_registry_port}}/busybox:latest" --dest-tls-verify=false
