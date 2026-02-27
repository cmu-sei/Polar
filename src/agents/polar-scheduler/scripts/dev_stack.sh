#!/usr/bin/env bash
set -euo pipefail

log() { echo "[scheduler] $*"; }
die() { echo "[scheduler] ERROR: $*" >&2; exit 1; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || die "missing command: $1"; }
need_env() { [[ -n "${!1:-}" ]] || die "required env var not set: $1 (did you 'direnv allow' at repo root or run 'nix develop'?)"; }
need_file() { [[ -f "$1" ]] || die "required file missing: $1"; }
need_dir() { [[ -d "$1" ]] || die "required dir missing: $1"; }

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "${REPO_ROOT}" ]] || die "not in a git repo (need Polar checkout)"
AGENTS_ROOT="${REPO_ROOT}/src/agents"
SCHEDULER_ROOT="${AGENTS_ROOT}/polar-scheduler"
[[ -d "${SCHEDULER_ROOT}" ]] || die "scheduler root not found at ${SCHEDULER_ROOT}"

: "${BUILD_PROFILE:=debug}"
WORKSPACE_TARGET_DIR="${AGENTS_ROOT}/target/${BUILD_PROFILE}"

# Validate profile
if [[ "${BUILD_PROFILE}" != "debug" && "${BUILD_PROFILE}" != "release" ]]; then
    die "BUILD_PROFILE must be 'debug' or 'release'"
fi

# TLS certs – must be set by dev environment (nix develop / direnv)
need_env TLS_CA_CERT
need_env TLS_SERVER_CERT_CHAIN
need_env TLS_SERVER_KEY
need_env TLS_CLIENT_CERT
need_env TLS_CLIENT_KEY
need_file "${TLS_CA_CERT}"
need_file "${TLS_SERVER_CERT_CHAIN}"
need_file "${TLS_SERVER_KEY}"
need_file "${TLS_CLIENT_CERT}"
need_file "${TLS_CLIENT_KEY}"

# Broker defaults
: "${BROKER_ADDR:=127.0.0.1:8080}"
: "${CASSINI_SERVER_NAME:=localhost}"
: "${RUST_LOG:=info}"

# Where we store PIDs and logs
RUNS_ROOT="${SCHEDULER_ROOT}/output/dev/runs"
mkdir -p "${RUNS_ROOT}"
CURRENT_LINK="${SCHEDULER_ROOT}/output/dev/current"

# Per-run paths (set by begin_run)
RUN_ID=""
RUN_DIR=""
PID_DIR=""
LOG_DIR=""

# Broker process
BROKER_PID=""
BROKER_LOG=""

# ----------------------------------------------------------------------
# Helper functions
# ----------------------------------------------------------------------
is_pid_running() {
    local pidfile="$1"
    [[ -f "$pidfile" ]] || return 1
    local pid; pid="$(cat "$pidfile" 2>/dev/null || true)"
    [[ -n "$pid" ]] || return 1
    kill -0 "$pid" >/dev/null 2>&1
}

stop_pidfile() {
    local pidfile="$1" name="$2"
    [[ -f "$pidfile" ]] || return 0
    local pid; pid="$(cat "$pidfile" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
        log "stopping ${name} (pid ${pid})"
        kill "$pid" >/dev/null 2>&1 || true
        sleep 0.5
        kill -0 "$pid" >/dev/null 2>&1 && kill -9 "$pid" >/dev/null 2>&1 || true
    fi
    rm -f "$pidfile"
}

compose_up() {
    need_cmd docker
    # Detect compose command: prefer podman-compose if available, then docker compose, then docker-compose
    if command -v podman-compose >/dev/null 2>&1; then
        DC=(podman-compose)
    elif docker compose version >/dev/null 2>&1; then
        DC=(docker compose)
    else
        need_cmd docker-compose
        DC=(docker-compose)
    fi
    log "starting compose services (neo4j, jaeger, gitea)..."
    "${DC[@]}" -f "${SCHEDULER_ROOT}/docker-compose.yml" -p scheduler up -d
}

compose_down() {
    if command -v podman-compose >/dev/null 2>&1; then
        DC=(podman-compose)
    elif docker compose version >/dev/null 2>&1; then
        DC=(docker compose)
    else
        need_cmd docker-compose 2>/dev/null || return 0
        DC=(docker-compose)
    fi
    log "stopping compose services..."
    "${DC[@]}" -f "${SCHEDULER_ROOT}/docker-compose.yml" -p scheduler down || true
}

# Build the cassini broker binary if needed
ensure_broker_binary() {
    need_cmd cargo
    local broker_bin="cassini-server"
    if [[ ! -x "${WORKSPACE_TARGET_DIR}/${broker_bin}" ]]; then
        log "building cassini broker (${BUILD_PROFILE})..."
        local cargo_flags=()
        [[ "${BUILD_PROFILE}" == "release" ]] && cargo_flags+=(--release)
        ( cd "${AGENTS_ROOT}" && cargo build -q -p cassini-broker --bin "${broker_bin}" "${cargo_flags[@]}" ) \
            || die "failed to build cassini broker"
    fi
    [[ -x "${WORKSPACE_TARGET_DIR}/${broker_bin}" ]] || die "broker binary missing after build"
}

broker_up() {
    ensure_broker_binary
    : > "${BROKER_LOG}"

    export BIND_ADDR="${BROKER_ADDR}"
    export TLS_CA_CERT TLS_SERVER_CERT_CHAIN TLS_SERVER_KEY TLS_CLIENT_CERT TLS_CLIENT_KEY
    export RUST_LOG

    log "starting cassini broker on ${BROKER_ADDR} ..."
    (
        cd "${AGENTS_ROOT}"
        exec "${WORKSPACE_TARGET_DIR}/cassini-server" \
            2>&1 | tee >(sed -r 's/\x1b\[[0-9;]*[a-zA-Z]//g' >> "${BROKER_LOG}")
    ) &
    echo $! > "${BROKER_PID}"
    sleep 2
    if ! is_pid_running "${BROKER_PID}"; then
        die "broker failed to start (check ${BROKER_LOG})"
    fi
    log "broker started (pid $(cat "${BROKER_PID}"))"
}

broker_down() {
    stop_pidfile "${BROKER_PID}" "broker"
}

wait_broker_ready() {
    local host port
    host="$(echo "${BROKER_ADDR}" | awk -F: '{print $1}')"
    port="$(echo "${BROKER_ADDR}" | awk -F: '{print $2}')"
    log "waiting for broker mTLS at ${host}:${port} ..."
    local i
    for i in $(seq 1 30); do
        if timeout 5 openssl s_client \
            -connect "${host}:${port}" \
            -servername "${CASSINI_SERVER_NAME}" \
            -CAfile "${TLS_CA_CERT}" \
            -cert "${TLS_CLIENT_CERT}" \
            -key "${TLS_CLIENT_KEY}" \
            -verify_return_error \
            </dev/null >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.5
    done
    die "broker not ready after 15s"
}

begin_run() {
    local ts rand
    ts="$(date -u +'%Y%m%dT%H%M%SZ')"
    rand="$(tr -dc 'a-z0-9' </dev/urandom | head -c 6 || true)"
    RUN_ID="${ts}-${rand}"
    RUN_DIR="${RUNS_ROOT}/${RUN_ID}"
    PID_DIR="${RUN_DIR}/pids"
    LOG_DIR="${RUN_DIR}/logs"
    mkdir -p "${PID_DIR}" "${LOG_DIR}"
    BROKER_PID="${PID_DIR}/broker.pid"
    BROKER_LOG="${LOG_DIR}/broker.log"
    ln -sfn "${RUN_DIR}" "${CURRENT_LINK}"
    log "run_id=${RUN_ID}"
    log "run_dir=${RUN_DIR}"
}

# ----------------------------------------------------------------------
# Commands
# ----------------------------------------------------------------------
up() {
    begin_run
    log "bringing up scheduler stack (mode=compose+native)"
    log "  BROKER_ADDR=${BROKER_ADDR}"
    log "  CASSINI_SERVER_NAME=${CASSINI_SERVER_NAME}"
    log "  RUST_LOG=${RUST_LOG}"
    log "  BUILD_PROFILE=${BUILD_PROFILE}"

    compose_up
    broker_up
    wait_broker_ready
    log "stack is ready"
    log "  - Neo4j:  http://localhost:7474 (neo4j/somepassword)"
    log "  - Jaeger: http://localhost:16686"
    log "  - Gitea:  http://localhost:3000"
    log ""
    log "To run integration tests:"
    log "  cd ${SCHEDULER_ROOT} && cargo test -- --nocapture"
    log ""
    log "To stop: $0 down"
}

down() {
    log "bringing down scheduler stack"
    broker_down
    compose_down
    log "stack stopped"
}

status() {
    if [[ -L "${CURRENT_LINK}" ]]; then
        log "current_run=$(readlink "${CURRENT_LINK}")"
        if is_pid_running "${CURRENT_LINK}/pids/broker.pid"; then
            log "broker: RUNNING (pid $(cat "${CURRENT_LINK}/pids/broker.pid"))"
        else
            log "broker: STOPPED"
        fi
    else
        log "no current run (have you run: $0 up ?)"
    fi
}

logs() {
    if [[ -L "${CURRENT_LINK}" ]]; then
        tail -n 200 -f "${CURRENT_LINK}/logs/broker.log"
    else
        die "no current run (run: $0 up)"
    fi
}

# ----------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------
cmd="${1:-}"
case "${cmd}" in
    up) up ;;
    down) down ;;
    restart) down && up ;;
    status) status ;;
    logs) logs ;;
    *)
        cat <<EOF
Usage: $0 {up|down|restart|status|logs}

Env:
  BUILD_PROFILE=debug|release
  BROKER_ADDR=127.0.0.1:8080
  CASSINI_SERVER_NAME=localhost
  RUST_LOG=info

Requires: nix develop / direnv (for TLS certs)
EOF
        exit 2
        ;;
esac
