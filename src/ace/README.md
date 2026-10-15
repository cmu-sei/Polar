<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
  for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

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
