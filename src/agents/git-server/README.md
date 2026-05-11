# polar-git-server

A minimal local git HTTP server for Polar development. Serves local git
working trees over HTTP using `nginx` + `fcgiwrap` + `git-http-backend`.

**This is a development tool only.** It runs on the host via podman and is
NOT deployed into the Polar cluster. Agents inside the usernetes cluster
reach it via the host's LAN IP.

## Why this exists

The git-repo-observer and polar-scheduler-observer clone repos over HTTP.
During development you want local commits to be picked up immediately without
pushing to GitHub first. This container lets you serve your local working
trees as HTTP git remotes, visible to all agents in the cluster.

## Quickstart

```sh
cd src/containers/git-server

# Build the container image (first time, or after changes)
just build

# Start the server (run in a dedicated terminal or tmux pane)
just serve

# Verify both repos are reachable
just verify
```

## Clone URLs

From the host:
```
http://localhost:3000/Polar.git
http://localhost:3000/polar-schedules.git
```

From inside the usernetes cluster:
```
http://172.26.26.88:3000/Polar.git
http://172.26.26.88:3000/polar-schedules.git
```

Use `just urls` to print the cluster-facing URLs at any time.

## Served repositories

| URL path               | Host path                                      |
|------------------------|------------------------------------------------|
| `/Polar.git`           | `~/Documents/projects/Polar`                  |
| `/polar-schedules.git` | `~/Documents/projects/polar-schedules`        |

Repos are mounted **read-only**. Agents can clone and fetch but cannot push.

## Configuration

The host IP and port are set at the top of the `Justfile`:

```just
host_ip := "172.26.26.88"   # host LAN IP reachable from cluster
port    := "3000"
```

Update `host_ip` if your LAN IP changes (`ip -4 addr show <iface>`).

Repo paths are derived from `$HOME` — edit the `Justfile` if your layout differs.

## How it works

- `fcgiwrap` spawns `git-http-backend` per request via a Unix socket
- `nginx` fronts fcgiwrap on port 3000, routing `/<name>.git/*` requests
- `GIT_HTTP_EXPORT_ALL=1` bypasses the `git-daemon-export-ok` requirement,
  so normal working trees (not bare repos) are served correctly
- Repos are mounted at `/srv/git/<name>.git` inside the container

## Adding a new repo

Edit the `Justfile` `serve` recipe and add a `-v` mount:

```just
-v "/absolute/path/to/repo":/srv/git/myrepo.git:ro \
```

The repo will be available at `http://<host>:3000/myrepo.git`.

Absolute paths only. No globbing. No relative paths.
