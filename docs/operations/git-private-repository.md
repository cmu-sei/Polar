# Runbook: the git observer clones a private repository

Written 2026-10-15. The first time the whole credential path runs on a cluster: a signed `credentials` document
delivers a token to the git observer, a signed `repositories` document tells the controller to watch a private
repository, and the observer clones it and publishes its commits. Everything below the "Expect" lines has been
tested in pieces with fakes and has **not** been seen on a cluster, so this runbook says what each step should show
and what to look at when it does not.

Commands are for fish, run from the repo root (`~/Documents/projects/Polar`) unless a step says otherwise.
Nothing here prints a secret. The token goes from its Kubernetes Secret into `polar-sign` through a pipe.

## What you need

- The cluster's node on the rebuilt image (`fb694c2`, CoreDNS forwards to the host's resolver on `127.0.0.1:5300`).
- A GitHub token that can read `daveman1010221/mini-swe-agent`, kept in the Secret `git-observer-credentials`
  (keys `username` and `token`) in the `polar` namespace. **The token pasted into the first session was exposed in a
  transcript: revoke it and replace it with a fine-grained, read-only token first**, then update the Secret yourself.
- A connection fast enough for the image builds (Step 1).

```fish
sudo modprobe pkcs8_key_parser                                   # the agents' escrow key custody needs it
podman start polar-registry                                      # the images are pushed here
set -x KUBECONFIG ~/Documents/projects/nix-usernetes/kubeconfig
kubectl get nodes                                                # Expect: one node, Ready
```

## Step 1. Build every image, together

The wire formats changed (the escrow-key reply, `RepoObservationConfig`, `StoreSecretRequest`), so the credentials
agent, the observer, the controller and every agent that holds a CryptoActor (the git observer, `kube-consumer`, the secret harness)
must be rebuilt and pushed in one go.
`up.nu --dev` builds exactly what the target's manifests use, and does the whole bring-up (Step 2) after it. To build
first and bring up later:

```fish
just images-push git-observer git-controller git-processor credentials-agent-consumer credentials-agent-observer kube-consumer secret-harness
nu infra/images.nu list                                          # every image name, if one of those is not right
```

Expect: each image ends pushed to `localhost:5000`. A failing image does not stop the others; the stage ends with a
summary and a non-zero exit. Do not go on with a failure. (The first Nix build of the controller took about an
hour cold; later ones reuse the store.)

## Step 2. Bring the cluster up

```fish
nu infra/up.nu local-minimal --dev --user djshepard --email djshepard@sei.cmu.edu
```

It is resumable: after a failure or a pause, run the same command. It stops for you only where a person is needed:

- **The browser step**, if Librewolf does not hold this cluster's CA and your certificate: close the browser, let
  it import (`just user-cert-install djshepard`).
- **The Neo4j password**, only if Neo4j is still on its factory password. The `nix-neo4j` image applies
  `NEO4J_AUTH` on first start, so normally there is no step.

If the seed step stops with "the Polar git server does not know `<uuid>` as an administrator": that is your account's
identity on this cluster, and a reset gives a new one. Put it in `POLAR_GIT_SERVER_ADMINS` for `polar-git-server` in
`infra/targets/local-minimal/config.nuon` (comma-separated, dropping the previous cluster's) and run `up.nu` again;
it restarts the git server by itself. You need to be an administrator of the git server to push in Step 6.

`up.nu` moves `~/.config/polar-admin` aside when it holds another cluster's certificates (`~/.config/polar-admin.old*`).
That is why the scaffolds are exported in Step 4, after this.

```fish
kubectl -n polar get pods
```

Expect, Running: `cassini-broker`, `credentials-agent` (consumer and observer), `cert-issuer`, `polar-git-server`,
`polar-scheduler-observer`, `polar-scheduler`, `git-repo-observer`, `git-repo-controller`, `git-repo-processor`. The
old `git-repo-scheduler` should not exist; if a cluster still has it: `kubectl -n polar delete job git-repo-scheduler`
and its ServiceAccount.

Expect at the end of `up.nu`: a line saying a pod can reach the outside (the DNS fix). If it says it cannot, stop:
the clone in Step 7 cannot work. See "If the cluster cannot resolve names" below.

## Step 3. Check the pieces this path depends on, before using them

```fish
just port-forwards-up observer git                               # local 8081 (the credentials agent's gateway), 8443 (git server)
```

**3a. The observer has an escrow certificate, and the gateway serves it.** Without one there is nothing to encrypt to.

```fish
curl -s -X POST localhost:8081/agent-escrow-key -H 'content-type: application/json' \
  -d '{"identity":"git-repo-observer.polar.serviceaccount.cluster.local"}' \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["Found"]["certificate_pem"])' \
  | openssl x509 -noout -subject -issuer -ext subjectAltName,keyUsage
```

Expect: a certificate issued by `Polar Internal CA`, key usage `Key Encipherment`, and a DNS name
`git-repo-observer.polar.serviceaccount.cluster.local`.

- `"NotFound"` (the body says so): the observer has not completed its escrow-identity issuance. Look at its init container and
  at `kubectl -n polar logs -l job-name=git-repo-observer --all-containers --tail=80`.
- A 404 with an empty body: the credentials-agent-observer image is older than the tool and has no such route (Step 1).
- A different DNS name: remember it, and pass it in Steps 5 and 6 as `--escrow-identity <that name>`.
- Connection refused: the forward is down (`cat /tmp/pf-credentials-observer.log`).
- A timeout (the gateway answers 504): the consumer did not answer. Either the Cassini broker refuses the gateway
  publishing on `credentials-agent-escrow-key-requests` (no rule for that topic was found in the repo, so this is
  the first thing a cluster can show) or the consumer is down. `kubectl -n polar logs -l job-name=credentials-agent --all-containers`.

**3b. The test credential exists.** Names only; do not print values.

```fish
kubectl -n polar get secret git-observer-credentials -o jsonpath='{.data}' | python3 -c 'import sys,json; print(sorted(json.load(sys.stdin)))'
```

Expect: `['token', 'username']`.

## Step 4. Give `polar-sign` the agents' scaffolds

```fish
POLAR_AGENT_SOURCE=images nu infra/resolve.nu scaffolds ~/.config/polar-admin/scaffolds --target local-minimal
ls ~/.config/polar-admin/scaffolds | grep -E 'git-repo-(observer|controller)'
```

Expect: `git-repo-controller.json` and `git-repo-observer.json` among the files. (Leave off `POLAR_AGENT_SOURCE=images`
to ask the source tree with cargo instead of the pushed images.)

```fish
function psign; cargo run -q --manifest-path ~/Documents/projects/Polar/src/agents/Cargo.toml -p polar-sign -- $argv; end
psign template git-repo-observer                                 # lists the sections it takes
```

(`psign` is a function for this shell only; the name avoids the `ps` command. Once `polar-sign` is built or installed,
use it directly. The first run compiles it.)

## Step 5. The repository, first (so the observer has to wait)

Putting the repository first shows the wait-and-retry behaviour: the controller announces a repository whose
credential the observer does not have yet.

```fish
psign template git-repo-controller repositories --out /tmp/repos.json
$EDITOR /tmp/repos.json
```

Fill in one entry (leave `version` at 1 on a fresh cluster; it must rise with every later document):

```
"url": "https://github.com/daveman1010221/mini-swe-agent.git",
"credential": "mini-swe-agent",
```

(`credential` is commented out in the template; delete the slashes. Leave `refs` out to observe the default branch.)
Then:

```fish
set host polar-git-server.polar.svc.cluster.local
psign sign /tmp/repos.json --user djshepard \
  --repo https://$host:8443/polar-schedules.git \
  --git-config http.curloptResolve=$host:8443:127.0.0.1 --push
```

Expect: `pushed permanent/git-repo-controller/repositories.dhall (repositories for git-repo-controller, version 1, 0
secret(s) sealed)`. It needs no gateway, because there is no secret.

- `nothing was signed: N problem(s)`: each problem is printed with the field. Fix the file.
- `403` on the push: your identity is not an administrator of the git server (Step 2).
- Certificate or CA errors on the push: the tool clears `GIT_SSL_CAINFO` and `SSL_CERT_FILE` for its git commands and
  gives git your Polar certificate and `ca.pem` itself; check `~/.config/polar-admin/ca.pem` is this cluster's CA.
- `cannot read the scaffold`: Step 4.

Within about a minute, look:

```fish
kubectl -n polar logs -l job-name=git-repo-controller --tail=40 | grep -iE 'repositories section|does not read'
kubectl -n polar logs -l job-name=git-repo-observer --tail=60 | grep -iE 'waiting for the credential|not observed|could not be cloned'
```

Expect: the controller logs `repositories section loaded` with `repositories=1` (it then announces the set every 60 s,
without a log line each time); the observer logs that `https_github.com_daveman1010221_mini-swe-agent` (the repository's
id) `is waiting for the credential it names`, and no clone is attempted.

## Step 6. The credential

```fish
psign template git-repo-observer credentials --out /tmp/creds.json
$EDITOR /tmp/creds.json
```

One entry. The username is in the Secret (it is not secret; read it with
`kubectl -n polar get secret git-observer-credentials -o jsonpath='{.data.username}' | base64 -d`):

```
"name": "mini-swe-agent",              <- the name the repository gave in Step 5
"host_pattern": "github.com",
"kind": "token",
"secret": "-",                         <- read from standard input
"username": "<the username>",          <- uncomment it
```

Then pipe the token in; it is never typed, shown or written:

```fish
kubectl -n polar get secret git-observer-credentials -o jsonpath='{.data.token}' | base64 -d \
  | psign sign /tmp/creds.json --user djshepard \
    --repo https://$host:8443/polar-schedules.git \
    --git-config http.curloptResolve=$host:8443:127.0.0.1 --push
```

Expect: `pushed permanent/git-repo-observer/credentials.dhall (credentials for git-repo-observer, version 1, 1
secret(s) sealed)`. The tool asked the gateway once (`/tmp/pf-credentials-observer.log` is not where to look; the
request shows in `kubectl -n polar logs -l job-name=credentials-agent-observer --tail=20`).

- `the escrow certificate for ... cannot be believed: the certificate does not name ...`: use `--escrow-identity <the DNS name from 3a>`.
- `... does not lead to a CA this verifier trusts`: your `~/.config/polar-admin/ca.pem` is another cluster's. Step 2
  moves a stale one aside; check `openssl x509 -in ~/.config/polar-admin/ca.pem -noout -fingerprint` against
  `kubectl -n polar get configmap polar-ca -o jsonpath='{.data.ca\.pem}' | openssl x509 -noout -fingerprint`.
- `cannot reach the credentials agent's gateway`: `just port-forwards-up observer`.

Do not reuse a file that has already been signed with a lower or equal version: a document with a version no higher than
the last applied is refused. Edit the version first.

## Step 7. Watch it work

```fish
kubectl -n polar logs -l job-name=git-repo-observer --tail=100 | grep -iE 'credentials section|secret registered|escrowed|waiting|not observed|cloned|Forwarding event'
```

Expect, in this order, within about a minute of the push:

1. `credentials section applied` (with `registered=1 failed=0 withdrawn=0`) and `secret registered`.
2. `secret escrowed with the credentials agent` (a copy of the credential is kept with the binding).
3. The repository's wait ends (the section change wakes it at once): a clone, then `Forwarding event to topic ...` lines
   for the commits and the ref update.

Then, in the graph (Neo4j browser, `https://localhost:7473`; you hold the password):

```cypher
MATCH (r:GitRepository) RETURN r.id
MATCH (c:GitCommit) RETURN count(c)
```

Expect: the repository, and commits (the first walk covers up to 100 per ref). The processor logs the writes:
`kubectl -n polar logs -l job-name=git-repo-processor --tail=40`.

Each signed document also has an outcome node in the graph (`ConfigurationOutcome`, linked with `HAD_OUTCOME`); a
refused document is there with the reason and signer.

### Things worth trying once it works

- **Withdraw it.** Sign a `credentials` document at version 2 with `"credentials": []`. The observer logs
  `withdrawn=1`; the repository stops being fetched successfully (`is waiting for the credential it names`).
- **A wrong host.** Change `host_pattern` to `gitlab.com` at version 3. The repository must not be cloned, and the
  observer must say its credential is not bound to this host.
- **A change of settings.** `psign template git-repo-controller --settings`, uncomment
  `POLAR_GIT_CONTROLLER_REPUBLISH_SECS`, give it `15`, sign and push; the controller's announcements speed up.

## If something does not appear

| What you see | Where to look | Likely cause |
|---|---|---|
| Nothing changes after a push | `kubectl -n polar logs -l job-name=polar-scheduler-observer --tail=60` | it has not pulled yet (give it a minute), or it cannot read the repository |
| `...refused` / `version` in the observer's or controller's log | the `ConfigurationOutcome` node, and the agent's log | a version not higher than the last; a signer without `c:update-runtime`; a signature that does not verify (stale `ca.pem`) |
| The controller never says `repositories section loaded` | its log; `kubectl -n polar logs -l job-name=polar-scheduler --tail=60` | the document is addressed to `git-repo-controller` and was not delivered; the controller registers and asks for a replay |
| `could not register the credential` in the observer | its log (the reason is printed, never the value) | the secret does not open under this context: wrong version in the file than was signed, or encrypted to another key |
| `the credential ... is not bound to this repository's host` | observer log | `host_pattern` does not match the repository's host |
| `is not observed: a credential is only used for an https repository` | observer log | the repository URL is not plain `https://host/...` (no login, no `http`) |
| `could not be cloned` | observer log | DNS or the network (below), or the token cannot read the repository (401/403 in the message) |
| The clone works but no commits in the graph | processor log | the graph connection (Neo4j password), not this path |

### If the cluster cannot resolve names

```fish
ss -lun 'sport = :5300'                                          # the host listener must be there
podman exec u7s grep forward /etc/coredns/Corefile               # Expect: forward . 10.0.2.2:5300
```

If the Corefile says `10.0.2.3`, the node is running the old image: `just load-node-image`, then
`just reset && just up && just init && just kubeconfig` (in `nix-usernetes`). If the listener is missing, the host's
`services.resolved` setting from `/etc/nixos` is not applied.

## Afterwards

- Tell me which of the "Expect" lines held and which did not, with the log line (not the value) for any that did not.
- Delete what holds nothing secret but is no longer needed: `rm /tmp/repos.json /tmp/creds.json`,
  and the tool's clone `rm -rf ~/.cache/polar-sign/schedules` (it holds only signed documents and ciphertext).
- The documents in `polar-schedules` are ciphertext, but ciphertext in git history stays there: change the token's
  value in GitHub once the observer has stored it (`docs/architecture/signed-configuration.md`, "A property to keep
  in mind").
- Record what the cluster showed in `remaining-work.md` (GIT CREDENTIAL PATH 1) and tick what is now seen.

## What this runbook does not cover

Path 2 (proposals from other agents and their approval), the removal of a repository from a later version of
`repositories` (the observer keeps observing it until its pod restarts), the GitLab discovery flow (dropped until
proposals), the observer's network guard (private addresses are not yet refused), and per-repository status events.
