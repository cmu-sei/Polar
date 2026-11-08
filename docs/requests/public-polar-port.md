<!--
  ACE Tool

  Copyright 2026 Carnegie Mellon University.

  NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
  FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
  EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
  PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
  MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
  TRADEMARK, OR COPYRIGHT INFRINGEMENT.

  Licensed under a MIT (SEI)-style license, please see license.txt or contact
  permission@sei.cmu.edu for full terms.

  [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
  distribution.  Please see Copyright notice for non-US Government use and distribution.

  This Software includes and/or makes use of Third-Party Software each subject to its own license.

  DM26-0670
-->

# Bringing public Polar's work into the fork

Branch `port/public-polar-blitz-deploy`, on `main` at 9e361ffd. Not pushed. A request for the fork's owner to review: it
carries what public Polar's `blitz-deploy` branch (cmu-sei/Polar, at df69bce) added since the two histories parted at
9aea62f (2026-08-07), for the kube agents, the build processor, the operator, the CLI, the deployment library and the
build orchestrator. Nothing here is deployed; the dev cluster still runs the images built from 1b82f624.

## Where this sits in the fork's own plan

`remaining-work.md` ("MERGE UPSTREAM BEFORE MIGRATING THE BUILD PROCESSOR") already plans this: OpenAPI and GitLab onto the
builder first, then the upstream merge, then the build processor and the orchestrator, "a plan first: area by area, which
side's design holds, tests first", one fix per commit, no behaviour change inside a refactor commit. This branch does not
follow those ground rules: its commits are large ports, with tests carried and updated rather than written red-first, and
the build processor is brought over in its current form, not migrated to `polar-agent-builder`. Read it as a worked
version of that merge for the owner to take apart, in whatever order and by whichever of its pieces the plan wants.
Following that entry, Polar's controller line `MERGE (stype {stype})` in `UpdateState` (`f604cc43`) is not taken: no change
to `UpdateState` is carried.

## How it was brought over

By porting, not merging. Both sides rewrote the same files: the fork put the agents on `polar-agent-builder`, and Polar
restructured the consumer and the build processor. A merge would have conflicted in every large file, so each change was
re-made on the fork's structure, keeping what the fork has (deletion sweep, owner edges, constraints, declared topics,
Scudo, ring), and each commit names the Polar commits it carries.

| Commit | Carries | Polar commits |
|---|---|---|
| lib: the agent lifecycle events, the source-analysis events and OCILayerDiffId | shared vocabulary | 3c3a4b7, 312d5de |
| kube: cluster-scoped identity, namespaces and nodes, discovery | kube observer and consumer | 5bb6928, 242d6d3, 8d0f058, 2a5e3f1, 4466a03, 09af243 |
| build-processor: projection as pure functions, source-analysis events | build processor | 312d5de |
| deploy: the Dhall deployment library | `src/deploy` | 452eb96 to 0c6e2e4, 00b7a2c, 695c049, 7e5f8b7, a7a1953, fadce4c |
| polar-operator: the PolarAgent operator and its graph projection | operator, processor, kube projection | 3c3a4b7 to 0f975f0 |
| polar-cli | the `polar` binary | 30cf707, ec98c75, 7278e65, 857ed17, 6c1eb23, fadce4c |
| build-orchestrator | the orchestrator, from `src/fork/build-orchestrator` | 004184c to df69bce |

## Decisions that are the owner's to look at

- **Every kube graph key now carries `cluster_uid`** (Polar issue 236). The observer reads the kube-system namespace's
  UID at start and stamps it on each event. A node written before this has no `cluster_uid` and is no longer matched,
  and the unique constraints are new ones (`polar_unique_Pod_uid_cluster_uid`, ...): a graph that holds kube nodes
  needs them observed again, or migrated. `Node` is now `KubernetesNode` (it was an owner anchor labelled `Node`).
- **One events topic, not one per cluster.** Polar publishes on `polar.kubernetes.<cluster uid>.events` and the consumer
  subscribes as clusters are announced. The fork requires every topic to be an exact, declared name, so events stay on
  the one topic and carry their cluster's UID. Discovery is ported on two fixed topics (`polar.kubernetes.discovery.announce`
  and `.query`); the consumer records each announced cluster as a `KubernetesCluster` node. A topic per cluster would
  need the authority model to accept a pattern. It is untested upstream, and its handshake is not tested here either.
- **The operator overlaps decisions the fork has made since.** `remaining-work.md` says `origin/polar-operator` (`3c3a4b78`)
  "overlaps decisions made since (the health actor's verdict and restart in place, the Reaper owning housekeeping, pulled
  health in the graph) and needs a design conversation, not only a merge". It is carried because it was asked for, and it
  is not reconciled: it keeps agents alive by its own phases and back-off. `PolarAgent` is also the fork's builder trait;
  the resource is a different thing.
- **`src/deploy` stays where it is** (decided 2026-11-03, recorded in `remaining-work.md` and `infra/INFRA.md` by this
  branch; the entry that said to remove it is changed). Blitz renders its deployment from this library. `main` has since
  reworded that entry ("`src/deploy/`: remove it once nothing renders it"), so the same change has to be made to
  it when this branch is rebased; what the two systems do about each other is for the merge. It was written on 2026-10-29 to 31 and has not been
  checked against the fork's later work (pulled health, signed runtime sections). `scripts/pipeline/render-manifests.nu`
  still renders the old tree under `src/deploy/local` for Flux; that was not run against the merged library.
- **aws-lc is not in the build.** The orchestrator's S3 client is `aws-sdk-s3` with its default client off and one over
  rustls/ring (`aws-smithy-http-client`), so `Cargo.lock` has no `aws-lc-rs`.

## What was not carried

- The orchestrator's Podman backend and its dependency-analysis scheduler (public Polar's `src/agents/build-orchestrator`
  keeps them). The events the scheduler needs (`PackageAnalysisStarted`, `AnalysisFindingReported`) are in the vocabulary.
- Polar's Neo4j scenario harness for the kube consumer (`kubernetes/common/src/testing`, `pod_running_on_scenario.rs`):
  it starts its own Neo4j container and could not be run here. The reusable `polar-test-graph` fixture is the way to port it.
- Polar's removal of the `HAS_STATE` pointer from `UpdateState` and the `GraphControllerMsg::Sync` barrier, which go with
  that harness. The fork still writes `HAS_STATE`.
- Per-cluster topics (above), and public Polar's August resolver-configuration commits, which touch nothing here.

## Checked, and not checked

Run in the Linux builder from `src/agents`: `cargo check --locked --workspace --tests`; `cargo clippy -D warnings` and
`cargo fmt` on the packages that changed; `cargo test` per package for `kube-common`, `kube-observer`, `kube-consumer`,
`build-processor`, `polar-operator-common`, `polar-operator`, `agent-processor`, `polar-cli`, `build-orchestrator`,
`orchestrator-core` and `orchestrator-backend-k8s`, all passing, and for `polar` (lib), where 98 pass and 17 fail: the 17
are `graph::tests::*`, which need Neo4j in a container, and unmodified `main` has the same 17 failing (93 pass there).
`nix build` of `polarOperatorImage`, `buildOrchestratorImage` and `buildOrchestratorObjectStoreImage`. `dhall type` of
`src/deploy/tests/custody.dhall` and a render of `local/core/plan.dhall` to the committed plan.

The whole workspace's tests were not run: they would build every crate's test binaries and run the Neo4j ones, and a run
of that size was stopped.

Not run:

- **Anything that needs Neo4j in a container.** The builder's user cannot reach the Docker socket (permission denied), so
  the graph tests in `polar` fail there (see above). `build-processor`'s
  `projection_neo4j` is ignored without a container runtime and was not run.
- **`tests/object_store.rs`**, which needs an S3-compatible store.
- **One operator test is ignored**: `the_cores_agents_differ_from_an_ordinary_one_where_they_must` asserts the 2026-10-29
  core plan, and the library moved on the next day (a `polar-custody` init container ahead of `polar-init`, no escrow
  bootstrap, an `fsGroup` on the issuer). Its stated reason is on the test. The same test and the same plan are on public
  Polar's tip; whether it fails there was not run.
- **The committed `builder-agent` and `plan` examples** under `src/deploy/examples` differ from a fresh render by a few
  `POLAR_CAPABILITIES` and `POLAR_INIT_*` lines; the same files differ on public Polar's tip.
- Nothing was run against a cluster.

## For after this

- Polar's `src/fork` can go once this is reviewed. Blitz points at it in five places: `infra/README.md`,
  `infra/values/local.dhall`, `docs/ace-catch-up.md`, `docs/build-orchestrator-job-credentials.md` and
  `scripts/ci/README.md`.
- The dev cluster that ran the images built from 1b82f624 has been torn down (2026-11-03), so there is nothing to replay
  the branch onto. Deployment testing is for a fresh cluster, on images built from this branch once it is rebased and pushed
  (by the maintainer).
