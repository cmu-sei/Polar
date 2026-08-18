from dash import Dash
from layout import create_layout
from config import app_color
from utils.dropdown_utils import (
    assemble_dropdown_selection,
    assemble_pi_selection_initial,
    assemble_pi_selection_training,
)
from utils.health import GraphHealthMonitor
from callbacks import register_callbacks
import dash_bootstrap_components as dbc
import os
import time


def _fetch_team_options_with_retry(max_wait_secs: int = 60, interval_secs: int = 3) -> list:
    """
    assemble_dropdown_selection() hits Neo4j directly with no retry of its
    own. If Neo4j isn't reachable yet at process start (e.g. right after a
    scale-up, or during a brief network blip) this call would otherwise
    raise before the Dash server ever binds to a port -- and before
    GraphHealthMonitor gets a chance to start, so the self-healing safety
    net we rely on once the app is running can't help with a startup-time
    failure. Poll patiently instead, matching polar-init's own
    don't-fail-on-transient-unavailability convention.
    """
    deadline = time.monotonic() + max_wait_secs
    last_error = None
    while time.monotonic() < deadline:
        try:
            return assemble_dropdown_selection()
        except Exception as e:
            last_error = e
            print(f"WARN: startup team fetch failed, retrying: {e}")
            time.sleep(interval_secs)
    raise RuntimeError(
        f"Could not fetch initial team list from Neo4j after {max_wait_secs}s"
    ) from last_error


# ------------ App Setup ------------
app = Dash(__name__, external_stylesheets=[dbc.themes.BOOTSTRAP]  )
server = app.server

# ------------ Build Dropdown Options ------------
pi_initial_options = []
team_options = _fetch_team_options_with_retry()
pi_training_options = []

# ------------ Start Background Health Monitor ------------
# Proves Neo4j is actually reachable (not just that a driver object was
# constructed) and writes the health-state file the Kubernetes liveness
# probe (polar_healthcheck.py) reads. See utils/health.py for the full
# design rationale.
health_monitor = GraphHealthMonitor()
health_monitor.start()

# ------------ Set Layout ------------
app.layout = create_layout(pi_initial_options, team_options, pi_training_options)

# ------------ Register Callbacks ------------
register_callbacks(app)

# ------------ Run Server ------------
if __name__ == "__main__":
    host = os.getenv("DASH_HOST", "0.0.0.0")
    port = int(os.getenv("DASH_PORT", "8050"))
    debug = os.getenv("DASH_DEBUG_MODE", "False").lower() in ("1", "true", "yes")
    app.run(host = host, port = port, debug = debug)
