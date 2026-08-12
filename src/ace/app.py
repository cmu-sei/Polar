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

# ------------ App Setup ------------
app = Dash(__name__, external_stylesheets=[dbc.themes.BOOTSTRAP]  )
server = app.server

# ------------ Build Dropdown Options ------------
pi_initial_options = []
team_options = assemble_dropdown_selection()
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
    port = int(os.getenv("DASH_PORT", "8080"))
    debug = os.getenv("DASH_DEBUG_MODE", "False").lower() in ("1", "true", "yes")
    app.run(host = host, port = port, debug = debug)
