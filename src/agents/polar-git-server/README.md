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

# polar-git-server

A git server for configuration repositories: git over HTTPS (the smart protocol), with every
connection authenticated by a client certificate. It is the home of Polar's signed-configuration
repositories, and it also runs on its own, with no Polar cluster around it, as a small
certificate-authenticated git host.

Design: [docs/architecture/git-server.md](../../../docs/architecture/git-server.md).

## What it does

- Serves the repositories its configuration declares, to the user's own `git` client.
- Takes the caller's **identity** from the verified TLS handshake: the DNS name in the client
  certificate's subject alternative names. Nothing in a request can change it.
- Decides, per repository, who may read and who may write, and who is an **administrator**
  (all access, and may create repositories while the server runs).
- Records every accepted push in `push-log.jsonl` beside the repositories: time, repository, ref,
  old and new commit, and who pushed.
- Inside a Polar cluster it also publishes each accepted push on Cassini. On its own it does not.

## Running it on its own

The same binary runs in a cluster and alone. It is a Cassini client only if its deployment says so
(`POLAR_CAPABILITIES` lists `cassini`); with the variable unset it is not, and it asks for nothing
from Polar. The log says so at start: `running without cassini` and `running on its own`.

### 1. Certificates

Any CA will do. The server needs a certificate whose name is the host clients will use, and each
person or program that connects needs a client certificate whose DNS name is its identity.

```bash
openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=git-ca" \
  -keyout ca.key -out ca.pem

make_cert() { # name, dns name, usage
  openssl req -newkey rsa:2048 -nodes -subj "/CN=$1" -keyout "$1.key" -out "$1.csr"
  printf 'subjectAltName=DNS:%s\nextendedKeyUsage=%s\n' "$2" "$3" > "$1.ext"
  openssl x509 -req -in "$1.csr" -CA ca.pem -CAkey ca.key -CAcreateserial -days 365 \
    -extfile "$1.ext" -out "$1.pem"
}
make_cert server git.example.org serverAuth     # the name clients type in the URL
make_cert alice  alice           clientAuth     # alice's identity is "alice"
```

### 2. Settings

| Variable | Meaning |
|---|---|
| `POLAR_GIT_SERVER_CERT`, `POLAR_GIT_SERVER_KEY` | the server's certificate and key |
| `POLAR_GIT_SERVER_CA_CERT` | the CA that client certificates must chain to |
| `POLAR_GIT_SERVER_REPO_ROOT` | where the repositories live; created if missing |
| `POLAR_GIT_SERVER_REPOS` | what it hosts, as JSON (below) |
| `POLAR_GIT_SERVER_PORT` | the port, `8443` by default |
| `POLAR_GIT_SERVER_ADMINS` | more administrators, comma-separated, beyond the `admins` in `POLAR_GIT_SERVER_REPOS` |

`POLAR_GIT_SERVER_REPOS` declares the repositories created at start:

```json
{"admins": ["alice"],
 "repos": [{"name": "demo", "readers": ["alice", "bob"], "writers": ["alice"]}]}
```

A repository named here is created if it is missing and never overwritten after that. A `seed`
directory per repository, committed as its first commit, is optional.

### 3. Start it and use it

```bash
POLAR_GIT_SERVER_CERT=server.pem POLAR_GIT_SERVER_KEY=server.key \
POLAR_GIT_SERVER_CA_CERT=ca.pem POLAR_GIT_SERVER_REPO_ROOT=/var/lib/git \
POLAR_GIT_SERVER_REPOS='{"admins":["alice"],"repos":[{"name":"demo","readers":["alice"],"writers":["alice"]}]}' \
  polar-git-server
```

Your `git` presents the client certificate:

```bash
git -c http.sslCert=alice.pem -c http.sslKey=alice.key -c http.sslCAInfo=ca.pem \
  clone https://git.example.org:8443/demo.git
```

(Put those three settings in `git config` once to stop typing them.) A certificate from the CA that
is not on the repository's list gets `403 not permitted`; no certificate is refused in the TLS
handshake.

### Creating a repository while it runs

An administrator can create one more repository without a restart. The call is idempotent: the
repository is created if it is missing, and its readers and writers are set to the body's either way.

```bash
curl --cert alice.pem --key alice.key --cacert ca.pem -X PUT \
  https://git.example.org:8443/admin/repos/second \
  -d '{"readers":["bob"],"writers":["alice"]}'
```

It answers `201` for a new repository. Anyone who is not an administrator is refused with `403`.
The access lists of repositories made this way are kept beside the repositories, in `runtime-acl.json`.

`polar-git-server --scaffold` prints every setting it reads, and `--help` explains the rest.

### Not yet verified

Running it as a container image outside a cluster, and joining a Polar cluster from outside by
giving it a Cassini client's settings (`POLAR_CAPABILITIES=cassini` with `TLS_CLIENT_CERT`,
`TLS_CLIENT_KEY`, `TLS_CA_CERT`, `BROKER_ADDR`, `CASSINI_SERVER_NAME`). The second is required
to be complete when `cassini` is listed: a half-configured server refuses to start.
