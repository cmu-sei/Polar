# ACE Dashboard

A Dash-based team completion-projection dashboard. Reads work-item data from a Neo4j graph database and projects sprint/increment completion timelines using Monte Carlo and Beta-distribution simulation. See `ARCHITECTURE.md` for full technical detail.

## Running locally

```bash
direnv allow   # or: nix develop

export NEO4J_URI=bolt://<host>:7687
export NEO4J_USER=neo4j
export NEO4J_PASSWORD=<password>

python app.py
# -> http://localhost:8080
```

Optional environment variables (defaults shown):

```bash
DASH_HOST=0.0.0.0
DASH_PORT=8080
DASH_DEBUG_MODE=False   # do not enable in any shipped/deployed environment
```

## Running as a container

```bash
nix build .#ace-viz-image
podman load -i result
podman run -p 8080:8080 \
  -e NEO4J_URI=bolt://<host>:7687 \
  -e NEO4J_USER=neo4j \
  -e NEO4J_PASSWORD=<password> \
  ace-viz:latest
```

No `.env` file is used anywhere in this project — all configuration is delivered via environment variables, matching how the container is run in Kubernetes (Secrets/ConfigMaps) and locally (exported shell variables or `podman run -e`).
