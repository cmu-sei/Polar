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

# health.py
#
# Background health tracking for the ACE Dash application, mirroring
# Polar's Rust polar-health/polar-healthcheck pattern (see
# src/agents/polar-health, src/agents/polar-healthcheck) so this
# service's operational behavior is consistent with the rest of Polar.
#
# What's ported from the Rust design, and why:
#   - GraphConnected (driver constructed) is NOT the same as GraphAvailable
#     (a real query actually succeeded). Only a real canary query proves
#     reachability -- constructing a neo4j.GraphDatabase.driver() object
#     does no handshake at all.
#   - The deadline anchors on the more recent of (last real success) or
#     (process start time), covering both "never became available" and
#     "was available, then went dark" with the same formula.
#   - A single failed canary is logged but never fatal by itself -- only
#     *sustained* failure past the deadline is unhealthy. This is what
#     stops one transient blip from restarting the pod.
#   - The health-state file is written on every tick regardless of
#     status, so file *freshness alone* proves this background thread is
#     still alive -- independent of whatever status it last reported.
#   - Writes are atomic (tmp file + os.replace), matching the Rust
#     implementation, to avoid the liveness probe ever reading a
#     partially-written file.
#
# What's deliberately NOT ported, and why:
#   - The RejuvenationPhase state machine (WaitingForDeps / Jittering /
#     WaitingForAck), dependency cert-TTL checking, and jitter-based
#     thundering-herd avoidance all exist in the Rust agents solely to
#     coordinate a graceful Job-exit-and-respawn for cert *renewal* --
#     each Job retry is a brand new pod, which is what actually gets a
#     fresh certificate. ACE is a Deployment with a one-year certificate
#     (see infra identityLifetimeOverrides); a liveness-probe-triggered
#     in-place container restart does NOT re-run the init container and
#     so cannot renew a cert anyway. There is nothing to coordinate.
#   - PrepareShutdown / ShutdownAck / bounded drain window: this exists
#     in the Rust agents to avoid silently dropping in-flight Cassini
#     messages during a supervised handoff. ACE has no broker and no
#     durable in-flight work -- an in-memory dashboard session lost to a
#     restart is not a correctness problem the way a dropped graph write
#     would be.
#
# Driver: uses the shared get_driver() singleton from utils.neo4j_client
# (mTLS-configured) rather than constructing its own -- see that module
# for why a single shared driver instance is correct here.

from __future__ import annotations

import json
import os
import tempfile
import threading
import time
from dataclasses import asdict, dataclass
from pathlib import Path

from neo4j.exceptions import Neo4jError, ServiceUnavailable

from utils.neo4j_client import get_driver

# ---------------------------------------------------------------------------
# Environment variables (same names as the Rust polar-health convention,
# for operational consistency -- anyone who knows the Rust agents' knobs
# already knows ACE's).
# ---------------------------------------------------------------------------

POLAR_HEALTH_FILE_ENV = "POLAR_HEALTH_FILE"
POLAR_HEALTH_FILE_DEFAULT = "/var/run/polar-health/polar-health.json"
POLAR_HEALTH_TICK_SECS_ENV = "POLAR_HEALTH_TICK_SECS"
DEFAULT_TICK_SECS = 30
POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV = "POLAR_HEALTH_GRAPH_DEADLINE_SECS"
DEFAULT_GRAPH_DEADLINE_SECS = 120

# Canary query: chosen because it can only ever fail on auth/connectivity,
# never on data content -- same reasoning as the Rust "RETURN 1" canary.
CANARY_QUERY = "RETURN 1"
CANARY_TIMEOUT_SECS = 5


@dataclass
class HealthState:
    status: str  # "healthy" | "unhealthy"
    timestamp: int
    graph_connected: bool

    @staticmethod
    def now() -> int:
        return int(time.time())

    @classmethod
    def healthy(cls, graph_connected: bool) -> "HealthState":
        return cls(status="healthy", timestamp=cls.now(), graph_connected=graph_connected)

    @classmethod
    def unhealthy(cls, graph_connected: bool) -> "HealthState":
        return cls(status="unhealthy", timestamp=cls.now(), graph_connected=graph_connected)

    def write(self, path: str) -> None:
        """Write atomically: tmp file + os.replace, so the liveness probe
        never reads a partially-written file."""
        directory = os.path.dirname(path) or "."
        try:
            fd, tmp_path = tempfile.mkstemp(dir=directory, prefix=".polar-health-", suffix=".tmp")
            try:
                with os.fdopen(fd, "w") as f:
                    json.dump(asdict(self), f)
                os.replace(tmp_path, path)
            except Exception:
                try:
                    os.unlink(tmp_path)
                except OSError:
                    pass
                raise
        except OSError as e:
            # Mirrors the Rust write(): a failed health-state write is logged,
            # not fatal. If this keeps failing, the file goes stale and the
            # liveness probe's freshness check catches it independently.
            print(f"WARN: failed to write health state to {path}: {e}")


class GraphHealthMonitor:
    """Background thread that periodically proves Neo4j is actually
    reachable (not just that a driver object exists) and writes the
    health-state file the liveness probe reads.
    """

    def __init__(self):
        self._tick_secs = int(os.getenv(POLAR_HEALTH_TICK_SECS_ENV, DEFAULT_TICK_SECS))
        self._graph_deadline_secs = int(
            os.getenv(POLAR_HEALTH_GRAPH_DEADLINE_SECS_ENV, DEFAULT_GRAPH_DEADLINE_SECS)
        )
        self._health_file = os.getenv(POLAR_HEALTH_FILE_ENV, POLAR_HEALTH_FILE_DEFAULT)

        self._last_graph_success: float | None = None
        self._start_time = time.monotonic()
        self._stop_event = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        """Write an initial state synchronously so the liveness probe never
        finds a missing file during startup, then start the background
        tick loop."""
        Path(os.path.dirname(self._health_file) or ".").mkdir(parents=True, exist_ok=True)
        HealthState.unhealthy(graph_connected=False).write(self._health_file)

        self._thread = threading.Thread(target=self._run, name="graph-health-monitor", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop_event.set()

    def _run(self) -> None:
        while not self._stop_event.is_set():
            self._tick()
            self._stop_event.wait(self._tick_secs)

    def _tick(self) -> None:
        graph_ok = self._run_canary()
        now = time.monotonic()

        if graph_ok:
            self._last_graph_success = now

        # Deadline check: anchor on whichever is more recent -- last real
        # success, or process start if there has never been one. A single
        # failed canary does NOT make this unhealthy by itself; only
        # exceeding the deadline does.
        if self._last_graph_success is not None:
            elapsed = now - self._last_graph_success
        else:
            elapsed = now - self._start_time

        deadline_exceeded = elapsed > self._graph_deadline_secs

        if deadline_exceeded:
            reason = (
                f"graph unavailable for {elapsed:.0f}s (deadline: {self._graph_deadline_secs}s)"
                if self._last_graph_success is not None
                else f"graph never became available within {elapsed:.0f}s (deadline: {self._graph_deadline_secs}s)"
            )
            print(f"FATAL: {reason} -- exiting so Kubernetes replaces this pod "
                  f"with a fresh one (new polar-init cert issuance).")
            # Write one final heartbeat so the liveness probe doesn't fire
            # first on a coincidentally-stale file and obscure the real
            # cause in logs/events -- this exit is deliberate, not a hang.
            HealthState.healthy(graph_connected=graph_ok).write(self._health_file)
            os._exit(1)

        HealthState.healthy(graph_connected=graph_ok).write(self._health_file)

    def _run_canary(self) -> bool:
        try:
            driver = get_driver()
            with driver.session(default_access_mode="READ") as session:
                session.run(CANARY_QUERY, timeout=CANARY_TIMEOUT_SECS).consume()
            return True
        except (ServiceUnavailable, Neo4jError, OSError) as e:
            print(f"WARN: graph canary failed: {e}")
            return False
