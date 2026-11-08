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

# Changelog

All notable changes to the platform are documented in this file, newest first. It is generated at each release by `nu infra/changelog.nu`, from the same classification of commits that chooses the version. A release is one version for every Polar image.

## [3.0.0] - 2026-11-02

Images built from `de146b15cb82`, the commit that set VERSION to 3.0.0.

> Multi-instance workloads (protocol 2); measured back-pressure under several publishers; pulled health in the graph, with configuration and credential problems held instead of crash-looping; the Jira pair checked issue for issue against a real Data Center across a restart, its token signed and escrowed, its settings changed at run time; the graph's writes batched, indexed and cleared of empty nodes.

### 💥 Breaking changes

- *(cassini)* The broker stamps the publisher's instance; protocol 2
  Rebuild and redeploy every image together: a 2.x client and a 3.0 broker refuse each other at registration, naming both protocol versions.
- *(jira)* Refuse what cannot be meant at start, and ask the site what it is
  JIRA_DEPLOYMENT must be cloud, server or datacenter; QUERY_DATE must be YYYY-MM-DD or YYYY-MM-DD HH:MM; the placeholder JIRA_EMAIL is refused. Each is held and reported in (:AgentHealth) as Configuration, not ignored.
- *(jira)* The credential is a signed, escrowed runtime section
  JIRA_TOKEN, JIRA_EMAIL and the jira-secret Secret are gone. On a fresh cluster up.nu seals the target's sops jira_token (and jira_email, for Cloud) into jira-observer's signed credentials section once; after that, rotate it with polar-sign template jira-observer credentials and polar-sign sign --push.
- *(graph)* Every node key binds the variable it is given
  PodState is keyed on pod_uid and valid_from, and every Kubernetes state's valid_from is an integer (epoch ms), not text. State nodes written before 3.0.0 keep their old shape and their duplicates; the uniqueness constraints made at start report those duplicates and fall back to an index. A fresh graph has none.

### 📦 Images

| Image | Change |
|---|---|
| `ace-viz:latest` | `sha256:ed7c637a795e` -> `sha256:e4285fb84857` |
| `build-processor:latest` | `sha256:7f1298582b08` -> `sha256:b0e89d29c3a2` |
| `cassini:latest` | `sha256:19471a5ed1d7` -> `sha256:7e11fcaca8a1` |
| `cert-issuer:latest` | `sha256:06ab51683dd4` -> `sha256:9abc3dbd44e5` |
| `credentials-agent-consumer:latest` | `sha256:e66933fa6a68` -> `sha256:87d5fccd5450` |
| `credentials-agent-observer:latest` | `sha256:d1bf25c5502d` -> `sha256:b8976ea1eed7` |
| `dev:latest` | `sha256:37ca3648d68e` -> `sha256:ecb0968eb3b0` |
| `git-controller:latest` | `sha256:bbcf74fb8548` -> `sha256:b40569cc771f` |
| `git-observer:latest` | `sha256:e751b943f851` -> `sha256:d6942acf2375` |
| `git-processor:latest` | `sha256:c8b6b1a20507` -> `sha256:026e40e07bed` |
| `gitlab-consumer:latest` | `sha256:f4e99b02fb47` -> `sha256:1fb7296f5d0f` |
| `gitlab-observer:latest` | `sha256:67816dde4540` -> `sha256:2ecfe332c06d` |
| `harness-producer:latest` | `sha256:475422bc9ae4` -> `sha256:99457513288a` |
| `harness-sink:latest` | `sha256:414f27babb07` -> `sha256:9c1ca98a4de4` |
| `jira-observer:latest` | `sha256:569b1477f38e` -> `sha256:8ab09583d992` |
| `jira-processor:latest` | `sha256:c85bdd156f54` -> `sha256:2ae1ecf72790` |
| `kube-consumer:latest` | `sha256:8534efcfc43b` -> `sha256:a22398138e02` |
| `kube-observer:latest` | `sha256:da8284746b5c` -> `sha256:3da8dc01be06` |
| `oci-resolver:latest` | `sha256:52527d87916c` -> `sha256:370bb8da81bd` |
| `openapi-observer:latest` | `sha256:7069531a0f1a` -> `sha256:5b74f0623f94` |
| `openapi-processor:latest` | `sha256:2ebc795c4b91` -> `sha256:99701814fd1b` |
| `polar-custody:latest` | `sha256:c59e5a859eda` -> `sha256:64081625e045` |
| `polar-git-server:latest` | `sha256:129413f8642d` -> `sha256:f5bffea5aed1` |
| `polar-init:latest` | `sha256:79d20132be9e` -> `sha256:aa19d9db61fd` |
| `polar-nu-init:latest` | `sha256:ddf4ec503bc6` -> `sha256:eb6c52abdc72` |
| `polar-tools:latest` | `sha256:0fa696960cea` -> `sha256:6cb335770027` |
| `scheduler-observer:latest` | `sha256:7fa564bb190e` -> `sha256:927e4d7a0aa5` |
| `scheduler-processor:latest` | `sha256:19a8e0d5b01a` -> `sha256:2cdb2976cf25` |
| `secret-harness-intruder:latest` | `sha256:1aad7c08ec88` -> `sha256:7af623a32cf0` |
| `secret-harness-owner:latest` | `sha256:2616e00fa0d9` -> `sha256:ae02a007d491` |
| `soft-state-observer:latest` | `sha256:376cdaf437ee` -> `sha256:bd6ecf2b0435` |
| `soft-state-processor:latest` | new, `sha256:e440f9111e14` |

### 🚀 Features

**builder**

- Pulled health, answered by the health actor's report
- A held agent tries again when a signed update arrives

**cassini**

- Cassini-client and cassini-c2 answer --version
- The stall rule ends only a subscriber that makes no progress

**cassini-broker**

- The memory controller's latency target and step are settings
- Under a container limit the budget's ceiling is half the limit
- Supersession by identity and instance

**cert-issuer**

- A multi-instance workload's certificate names its pod

**credentials-agent**

- The first escrow issuance per identity is serialized
- The revoked set says which were revoked for a compromise
- A retry is not a second request
- An escrow certificate asked again, or abandoned, is issued again
- A multi-instance certificate requires its pod's name

**crypto**

- An HTTP credential for a bound host, beside git's

**graph**

- Owners make the constraints for their labels at start

**harness**

- A sink slow before it acknowledges, so the broker feels it
- Broker settings per run, and what the broker's bounds said over it
- An image tag retags a run's every container, and the fifth set's plans

**infra**

- The broker's container gets a 1 GiB memory limit
- A catalog workload may be multi-instance
- The load harness's producer is multi-instance; the pressure plan
- U7c reads Apache's Jira anonymously
- The seed seals the Jira token from sops once, on a fresh cluster

**jira**

- Read a site anonymously with JIRA_ANONYMOUS=true
- Hold a failure a retry cannot fix through the agent's health
- Settings change at run time, by a signed update

**polar-agent-builder**

- A multi-instance pod's clean shutdown ends its session

**soft-state**

- The state agent's writer asks every agent about its health

**up**

- --import-tag brings a cluster up on a published release's images

### 🐛 Bug fixes

**cassini-broker**

- A stopped broker holds none of its names
- Bind a connection to its session before telling the client it is registered

**cert-issuer**

- Refuse a future nbf or iat and a token that lives too long

**credentials-agent**

- A held request is given the answer it was given first
- The certificate lookup answers with the escrow certificate

**git-repo-observer**

- Report a ref when it moves, not on every pass
- The worker's path reports a ref only when it moves, too

**gitlab-consumer**

- A second registration restarts the consumers instead of colliding

**images**

- --also-tag takes comma-separated text, which a shell can pass

**infra**

- Tests act only on a cluster named for them, never the ambient KUBECONFIG
- Ca.nu and load-harness.nu find binaries where cargo put them
- Jira-check reads JIRA_ANONYMOUS as the agent does

**jira**

- A configuration that cannot work keeps jira-observer up, held
- Decode changelog authors and users without an email
- Skip a property the graph refuses, not the whole issue
- Walk every page of groups, users and issues
- Classify Jira failures, honour Retry-After, resume at the failed page
- Drain the graph controller at shutdown instead of stopping it
- Find the system fields by id, not by English display name
- Reading anonymously, the users observer is not started
- A sub-task is CHILD_OF its parent by the parent's key

**kube**

- Remove the empty state-type nodes at kube-consumer's start

**polar-agent-builder**

- A submit waits for its judgement, not CALL_TIMEOUT

**polar-custody-daemon**

- A custodian refused as revoked reports its own certificate alone

**polar-init**

- A run again in the same pod goes on from what the daemon holds

### ⚡ Performance

**graph**

- Write batches and transactions as UNWIND statements

Also in this release, not listed: 28 documentation, 7 test and 4 housekeeping commits.

## [2.0.0] - 2026-10-31

Images built from `55f97ed2c3c9`, the commit that set VERSION to 2.0.0.

> Cassini now bounds its own memory: every publish and every delivery is numbered and acknowledged, the broker sets a memory budget at run time from what the machine can spare and how fast its subscribers drain, a stalled subscriber is ended after a bound instead of filling memory, and a client and broker of different protocol versions refuse each other by name. A new state agent keeps what agents need to remember across restarts, decided by each agent's identity and its signed runtime configuration, and Jira's issue pulls now resume from the graph's newest issue instead of starting over. Authority became policy: certificates carry each workload's declared topics, Cedar decides above them, and signed documents are checked against a strict schema. Every binary is one static musl artifact per target with the Scudo allocator and LLVM's memcpy, and reports its version with --version.

### 💥 Breaking changes

- *(infra)* The git server's administrators are a recorded cluster input, not an edit to a tracked file
  The Polar git server's administrators come from infra/targets/<target>/cluster-input, which up.nu records, not from an edit to a tracked file.
- *(agents)* Seven agents take their workload name as their service name
  Signed documents addressed to an agent's old name (credentials-agent-consumer and the other renamed agents) are no longer delivered: sign them again for the workload name; agent.<old>.* topics are agent.<workload>.*.
- *(agent-builder)* An agent starts only as the workload its certificate names
  Run each agent under the ServiceAccount of its workload name; under any other it refuses to start.
- *(polar-scheduler)* The scheduler handles signed documents only
- *(polar-policy)* Policies speak a strict schema, checked on arrival and at every decision
- *(broker)* The broker decides every publish and subscribe, Cedar first, then the certificate's floor
  An agent publishes and subscribes only on its declared and implied topics; a client whose identity is not a catalog workload gets a certificate with no topics and is refused. Declare a workload's topics in its agent, and put the workload in the catalog.
- *(credentials-agent)* Remove the three unsigned topics that appended whatever event they carried
  the credentials agent no longer subscribes to credentials-grants, credentials-revocations or credentials-account-provisioning, and credentials_agent_common no longer exports their constants. Grants, revocations and provisioning go only through signed admin actions on credentials-admin-action-requests.
- *(credentials-agent)* Remove the issuance floor, which the authority model replaces
  the credentials agent no longer reads CREDENTIALS_AGENT_ISSUANCE_TOPICS. It writes into a workload's certificates what CREDENTIALS_AGENT_AUTHORITY_MODEL gives that workload, and with no model a certificate carries no topics or groups.
- *(cert-issuer)* Cert-issuer is a Polar agent on the builder, speaking over Cassini without depending on it
  cert-issuer no longer reads CERT_ISSUER_CONFIG. Its settings are the variables `cert-issuer --help` lists, and the chart renders them; conf/ describes the old file format (remaining-work.md).
- *(catalog)* Cert-issuer is a catalog workload of a kind of its own, `ca`, rendered with no polar-init
  Targets deploy cert-issuer as a catalog workload of the kind `ca` (cert-issuer.yaml, cert-issuer-resources.yaml), not the chart. A cluster that ran the chart must lose the chart's cert-issuer when it switches: Flux prunes it on sandbox-local; on a target applied by up.nu it is deleted by hand.
- *(cert-issuer)* An escrow certificate is for encrypting to, and no longer an mTLS certificate
  An escrow certificate can no longer be presented as a TLS client certificate; use the workload's client certificate.
- *(polar-init)* Drop its own escrow bootstrap; the custodian fetches the key
  A pod's escrow key is fetched by its custody daemon (on by default); a target with escrow and custody switched off no longer renders.
- *(infra)* The agents as Deployments of one that restart in place
  Re-render and apply, and delete the old Jobs; tooling that looked for an agent's Job must look for its Deployment.
- *(jira-observer)* Start the first issue pull from the graph's newest issue
  jira-observer starts each issue pull from the graph's newest issue; QUERY_DATE applies only while the graph holds no issues, so a script that moves it as a cursor has no effect after the first pull and is no longer needed.
- *(cassini)* Numbered publishes, acknowledged one by one, and the client's outbox
  Rebuild and redeploy every image together: a client and a broker from before 2.0.0 cannot talk; from 2.0.0 a mismatch is refused at registration, naming both protocol versions.
- *(cassini-broker)* The memory budget is set at run time from memory and the drain rate
  CASSINI_MEMORY_BUDGET_BYTES is now a cap on the runtime budget, not its value; the broker refuses to start with less than 256 MiB (four 64 MiB frames) available.

### 📦 Images

| Image | Change |
|---|---|
| `ace-viz:latest` | `sha256:f593d481b916` -> `sha256:ed7c637a795e` |
| `build-processor:latest` | `sha256:1706f19107fc` -> `sha256:7f1298582b08` |
| `cassini:latest` | `sha256:bf9a277828e2` -> `sha256:19471a5ed1d7` |
| `cert-issuer:latest` | `sha256:9b4f104ea885` -> `sha256:06ab51683dd4` |
| `credentials-agent-consumer:latest` | `sha256:bf70f353c78c` -> `sha256:e66933fa6a68` |
| `credentials-agent-observer:latest` | `sha256:e00a2f05d872` -> `sha256:d1bf25c5502d` |
| `dev:latest` | new, `sha256:37ca3648d68e` |
| `git-controller:latest` | `sha256:8f463d6b334b` -> `sha256:bbcf74fb8548` |
| `git-observer:latest` | `sha256:7c9bdc6c3b9d` -> `sha256:e751b943f851` |
| `git-processor:latest` | `sha256:bd2b1bdba70b` -> `sha256:c8b6b1a20507` |
| `gitlab-consumer:latest` | `sha256:676c6fa4c44c` -> `sha256:f4e99b02fb47` |
| `gitlab-observer:latest` | `sha256:fb5a13a3416e` -> `sha256:67816dde4540` |
| `harness-producer:latest` | `sha256:4e875cf93533` -> `sha256:475422bc9ae4` |
| `harness-sink:latest` | `sha256:2a2e94f7a0b5` -> `sha256:414f27babb07` |
| `jira-observer:latest` | `sha256:b517469c7698` -> `sha256:569b1477f38e` |
| `jira-processor:latest` | `sha256:976918df3b03` -> `sha256:c85bdd156f54` |
| `kube-consumer:latest` | `sha256:86b578117152` -> `sha256:8534efcfc43b` |
| `kube-observer:latest` | `sha256:f7230aae267d` -> `sha256:da8284746b5c` |
| `oci-resolver:latest` | `sha256:9d69c091912f` -> `sha256:52527d87916c` |
| `openapi-observer:latest` | `sha256:5859b4e5074c` -> `sha256:7069531a0f1a` |
| `openapi-processor:latest` | `sha256:bf17c71c9f1e` -> `sha256:2ebc795c4b91` |
| `polar-custody:latest` | new, `sha256:c59e5a859eda` |
| `polar-git-server:latest` | `sha256:42ce87efaabc` -> `sha256:129413f8642d` |
| `polar-init:latest` | `sha256:a619fd616e62` -> `sha256:79d20132be9e` |
| `polar-nu-init:latest` | `sha256:e38b30b38815` -> `sha256:ddf4ec503bc6` |
| `polar-tools:latest` | `sha256:3a46b0a70f6b` -> `sha256:0fa696960cea` |
| `postgres:16` | `sha256:75adc2a65806` -> `sha256:b95b87e93ceb` |
| `scheduler-observer:latest` | `sha256:ae83174cc8d8` -> `sha256:7fa564bb190e` |
| `scheduler-processor:latest` | `sha256:59203f830e5d` -> `sha256:19a8e0d5b01a` |
| `secret-harness-intruder:latest` | new, `sha256:1aad7c08ec88` |
| `secret-harness-owner:latest` | new, `sha256:2616e00fa0d9` |
| `soft-state-observer:latest` | new, `sha256:376cdaf437ee` |

### 🚀 Features

- **admin**: 1
- **agent**: 1
- **agent-builder**: 5
- **agents**: 3
- **authority**: 1
- **broker**: 1
- **builder**: 7
- **cassini**: 9
- **cassini-broker**: 1
- **cassini-client**: 2
- **cert-issuer**: 11
- **cert-issuer-common**: 1
- **credentials**: 2
- **credentials-agent**: 10
- **crypto**: 1
- **custody**: 3
- **custody-daemon**: 1
- **dev-container**: 1
- **dev-pod**: 2
- **egress**: 2
- **escrow**: 1
- **git**: 1
- **git-repo-observer**: 2
- **graph**: 1
- **harness**: 3
- **harness-common**: 1
- **images**: 1
- **infra**: 35
- **init**: 1
- **jira**: 1
- **jira-observer**: 1
- **jira-processor**: 1
- **kube**: 1
- **load-harness**: 2
- **oci-resolver**: 2
- **pipeline**: 1
- **polar-custody**: 3
- **polar-git-server**: 1
- **polar-health**: 1
- **polar-init**: 4
- **polar-permissions**: 1
- **polar-scheduler**: 1
- **polar-scudo**: 2
- **polar-sign**: 1
- **policy**: 2
- **release**: 2
- **render**: 1
- **soft-state**: 3
- **version**: 2

<details><summary>All 144 entries</summary>

**admin**

- The first admin can revoke certificates

**agent**

- A refused topic is kept as wanted, judged again on a change of authority, and in the health file

**agent-builder**

- An agent's topics are one computed set, printed in its scaffold
- Topic families expand into exact names per served workload
- Every publish and subscribe is checked, the agent's policy first
- A declared topic may say why it waits, and a bare scaffold expands families
- Implied topics come from groups, by one function the broker will share

**agents**

- The nine topics with no other side in the catalog say why
- Every agent declares what causes each publish, and a publish without one is refused
- Every binary static for musl, allocating with mimalloc

**authority**

- The credentials agent's key is pinned under the CA, not on first use

**broker**

- Held subscriptions are judged again on a change of authority and on a resumed session

**builder**

- A topic says what causes its publish, and the builder checks that it fits
- An agent may judge a runtime section before it is replaced, given the sections in force
- An agent may speak over Cassini without its health depending on it
- A custodian's floor is its own requests and the authority
- The fresh-start marker, so a container restarted alone restarts its pod (also: polar-init)
- Agents declare the graph labels they own and read, and graph-read
- A StateActor and the computed soft-state capability; jira-observer asks through it

**cassini**

- The client hears whether the broker took a subscription or refused it
- Step 5, the broker refuses and ends a revoked certificate's sessions, and admits watchers
- One watcher per certificate, holding its place for a tenure
- A refused registration is an event, with the broker's reason
- A refused registration carries a code, and the daemon reads that
- The allocator experiment's second set, Scudo from LLVM 23 and smalloc
- Acknowledged deliveries, held by each subscriber until its client has them
- A protocol version at registration, and a mismatch refused by name
- A session shed under an exhausted budget ends, and its client is told when it returns

**cassini-broker**

- One memory budget, ingress credit, a high water per subscriber and the stall bound

**cassini-client**

- A topic gate is asked before each publish and subscribe
- The outbox is the client's credit, and its callers wait for room

**cert-issuer**

- Its floor carries the bootstrap services' declared topics
- Bootstrap services are signed with their floor's topics and groups
- It writes the CA it signs under where its client reads it, and a scaffold says when an agent does not depend on Cassini
- Every certificate it signs is recorded before it is returned
- It answers the credentials agent's requests for the record, a page at a time
- An escrow certificate has a lifetime of its own, a year
- The floor state, in shadow, and its own entry for itself alone (floor step 2)
- Held requests, and the floor obeys its state (floor step 3) (also: credentials-agent)
- An escrow certificate's key usage follows its key
- The custodian's certificate, and the custodians' space reserved
- The successor ticket (also: credentials-agent)

**cert-issuer-common**

- Certificates can carry topics (p:pub:, p:sub:) and groups (g:)

**credentials**

- The renewal rule, a pod restarted in place is issued again within a bound
- The published revoked set leaves out what has ended

**credentials-agent**

- The credentials agents declare their topics
- An issuance floor of each workload's declared topics, written by the render
- A workload's client certificate carries its floor's topics and groups
- The authority model: the deployment as facts, Datalog rules over them, and the judging of a pair
- Judge policy and groups documents by the authority model, a person written by username
- A workload's certificate carries what the authority model gives it, by its one identity
- It asks cert-issuer for its record and its ledger holds every certificate cert-issuer signed
- Every request is judged by Cedar at its request gate, and the authority model judges requests too
- Sign the session offer, and seal only to a signed one (floor step 1c)
- Answer a custodian on its workload's behalf, and only it

**crypto**

- The custody daemon holds the escrow key, and a recipient's key is believed for its certificate

**custody**

- Step 4, the broker's and the git server's listeners take the daemon's server key
- Step 5, a revoked pod's daemon destroys its keys and exits
- The custodian slot, its plan, and the escrow key that only unwraps

**custody-daemon**

- The custodian's session fetches the workload's credentials

**dev-container**

- GNU sed and grep in the dev image

**dev-pod**

- Bin/polar-dev build-image, and up asks before it changes the pod's image
- Bin/polar-dev says why the pod's SSH or the clone refuses you, and what to do

**egress**

- Polar::egress, the proxy, bypass list and CA an agent calls outside the cluster with
- Cluster.egress, a target's proxy, bypass list and CA, rendered into the agents that declare it

**escrow**

- Seal to a P-256 key with HPKE, and say which suite a secret is under

**git**

- The git agents declare their topics

**git-repo-observer**

- Clone and fetch through the egress proxy, and give libgit2 the egress CA
- A walk is published oldest first in bounded batches, its checkpoint moving after each

**graph**

- A graph writer shows Neo4j the custody daemon's key, through our neo4rs fork

**harness**

- The load harness's producer and sink are builder agents that measure, and the controller goes
- The producer's window is counted in messages too, so a saturated run measures the broker
- A plan can measure every run under each of several broker settings

**harness-common**

- What the load harness's producer and sink share, measured and tested

**images**

- Publish a release's version beside latest, and import a version by its tag

**infra**

- Up.nu writes the first admin into the git server's administrators
- Deploy-repo.nu ensure makes the deployment PKI and runs the deployment git server
- Deploy-repo.nu publish commits a target's layers, Flux objects and provenance, signed
- Flux.nu install and wait
- Up.nu gitops mode: publish, install Flux and wait in place of the apply loop
- Images.nu publish can publish a mirror, copied from its upstream for the architecture
- Agent-pods.nu asks an agent's image about itself, as a pod on the cluster
- The agents are asked about themselves on the cluster, not with podman or cargo
- The registry the nodes pull from is one setting, POLAR_NODE_REGISTRY
- Images.nu import copies Neo4j too
- A target can keep its registry in its own cluster
- The deployment git server runs in the target's cluster
- Sandbox-local keeps its registry in its cluster too
- The dev container as a pod in the cluster, reached over SSH (bin/polar-dev)
- Bin/polar-dev up says what it is doing while it waits
- Images.nu takes --target for a target's registry
- A render fetches no Dhall, from a cache built with the repo
- Neo4j is copied from where it is published when there is no archive
- Resolve.nu lists the catalog's images without reading a scaffold
- Render.nu --charts-into renders the charts alone
- Images.nu takes references from before a render, and keeps build logs back
- Every image is built or copied once, before the render
- The topic cross-check, catalog-wide in a test and per target in the render
- Both floors carry each workload's groups beside its topics
- The load harness is in the catalog, disabled until a target is measured
- Load-harness.nu runs a plan's runs as Jobs, keeps their records, and compares them
- Sandbox-local is where the broker is measured, so it runs the load harness's accounts
- Step 4, the catalog gives pods the custody daemon, behind a switch that is on by default
- The render refuses escrow without custody, and a mix of keys
- Place each workload in its node pool where the target declares pools
- The schema's Kubernetes types at dhall-kubernetes 1.35
- The scripts wait on and roll the agents as Deployments
- Check that every locked git source is fetchable anonymously
- Jira-check.nu checks jira-observer's Jira connection from inside the cluster
- Seed the state agent's schema from the scaffolds; one scaffold pass again

**init**

- A pod escrows a key only when its agent needs encryption, derived from its scaffold

**jira**

- The jira agents declare their topics

**jira-observer**

- Reach Jira through the egress settings, and trust the egress CA beside the image's roots

**jira-processor**

- Write each issue's last update as updated_ms, declare what it owns

**kube**

- The kube agents declare their topics

**load-harness**

- The broker's CPU and memory over each run
- A sink that can be slowed, and the slow-sink plan

**oci-resolver**

- The resolver declares its topics
- Pull through the egress proxy, and trust every certificate of the egress CA

**pipeline**

- Run the source check in ci.nu's static analysis phase

**polar-custody**

- The custody daemon, step 1: it holds a pod's mTLS keys and signs TLS 1.3 handshakes over a socket
- Step 2, the workload signs through the daemon in place of TLS_CLIENT_KEY
- Step 4, a slot is sealed once a certificate is installed in it

**polar-git-server**

- The git server declares its topic

**polar-health**

- Step 4, the health actor watches the custody daemon and rejuvenates on an empty one

**polar-init**

- Every pod pins cert-issuer's CA, and what the observer hands over is verified against it
- Custody step 3, a certificate whose key is the daemon's, issued with no key file
- The token buys the custodian's certificate, and the custodian the rest
- With the custody daemon, every certificate's key is its

**polar-permissions**

- A certificate's groups are the principal's, unless the groups document names it

**polar-scheduler**

- The scheduler pair declares its exact topics

**polar-scudo**

- The CRC instructions everywhere, on request, and S3 and S4
- Every binary allocates with Scudo, built by the nix-scudo flake input

**polar-sign**

- A policy is checked against Polar's schema before it is signed

**policy**

- Groups nest and roles hold capabilities, built into Cedar's hierarchy once per update
- Action::"read-state" on Label, the vocabulary of soft-state reads

**release**

- A release commits its version into VERSION, and the development version after
- An override can say what to do about a break, and ten unmarked breaks are recorded

**render**

- The credentials agent is given the authority model's facts, every workload by its one identity

**soft-state**

- The state agent's reader, soft-state-observer
- The state agent in the catalog, its floor and readers rendered
- The floor is the state agent's signed runtime section, its replies a family over the served workloads

**version**

- Every binary reports the platform version, and every image carries it
- A binary's version record names the Cassini protocol it speaks

</details>

### 🐛 Bug fixes

- **agents**: 1
- **builder**: 1
- **cassini**: 8
- **cassini-broker**: 5
- **cassini-client**: 2
- **cert-issuer**: 1
- **charts**: 1
- **credentials-agent**: 5
- **custody**: 2
- **deps**: 6
- **dev-container**: 6
- **dev-pod**: 4
- **flake**: 2
- **forwards**: 1
- **infra**: 32
- **jira**: 1
- **jira-observer**: 3
- **lib**: 1
- **load-harness**: 3
- **load-plans**: 1
- **logging**: 1
- **neo4j**: 2
- **nix**: 1
- **pipeline**: 1
- **polar-agent-builder**: 1
- **polar-init**: 2
- **polar-sign**: 1
- **polar-test-graph**: 1
- **ractor**: 1
- **release**: 2
- **scheduler-observer**: 1
- **secrets**: 1
- **test**: 1
- **up**: 1
- **Other**: 3

<details><summary>All 106 entries</summary>

**agents**

- A second registration keeps the children still running instead of failing on their names

**builder**

- The StateActor asks again itself while the state agent cannot answer yet

**cassini**

- Set TCP_USER_TIMEOUT only where the platform has it
- Only the identity that owns a session may resume it
- The control plane's ListSessions answers
- Instrument futures instead of holding entered spans across awaits
- The stall clock counts only time a subscriber spends connected above high water
- A refusal says whether it is for now, and the client keeps what is refused for now
- An exhausted budget is recovered from within the stall bound
- A client that reads and acknowledges is not cut for what the broker did not read

**cassini-broker**

- A subscription that cannot be made is refused, not a reason to shut the broker down
- A topic drops a subscriber that has ended, and keeps what it could not deliver
- A subscription is made off the topic manager, and the client always hears how it went
- A wait given up is no longer counted, and the watchdog stops when the drain begins
- A stalled connection is ended only by a second look, so its reader's facts can arrive

**cassini-client**

- A buffered publish that arrives after the outbox emptied is sent
- A deferred publish is sent again once at a time, and a bare transport says who must

**cert-issuer**

- The credentials agent's custodian waits as the credentials agent does

**charts**

- A failed container leaves its last log lines as its termination message

**credentials-agent**

- Its service name is its workload name, credentials-agent
- One holder for the ledger, so two credentials agents cannot fork its chain
- A draining consumer takes nothing new and stops what acts on its own
- An agent's P-256 escrow key is handed out from its certificate
- Its escrow topics say they wait for custodians

**custody**

- The daemon logs at info unless RUST_LOG says otherwise
- Lock memory as it is touched, not as it is mapped

**deps**

- Ctutils, rustls-webpki, webpki-roots and webpki-root-certs to their fixed versions
- Crypto-bigint 0.7.5 to 0.7.10 for RUSTSEC-2026-0387
- Libz-sys 1.1.29 to 1.1.31 for RUSTSEC-2026-0418
- Yoke-derive 0.8.3 to 0.8.4, off a yanked release
- Oci-client 0.15 to 0.18, dropping the unmaintained jwt crate
- P256 0.13 to 0.14 for Polar's own signing and escrow keys

**dev-container**

- The dev image has diff, so kubectl diff can compare
- The rendered container configs carry the license banner
- The dev image has gpgsm, and a gitops target checks for it first
- Dropbear has an ECDSA host key, for clients under a FIPS policy
- Dropbear with AES-GCM, for clients under a FIPS policy
- Root keeps its own XDG directories in a kubectl exec session

**dev-pod**

- Bin/polar-dev up keeps the class of the claims it already made
- Bin/polar-dev runs the image of the cluster's architecture
- Bin/polar-dev up stops first when the ssh agent holds no key
- Bin/polar-dev clones main by default

**flake**

- Export tlsCerts again, which the dev shell's TLS hook builds
- CloneImage is gone with the build orchestrator

**forwards**

- The observer hint names its Deployment's pod label

**infra**

- The shared secrets are encrypted to every listed key, and decrypting needs no .sops.yaml
- The deployment git server's container gets a writable /tmp
- Sandbox-local's data layer does not wait for Neo4j
- A running agent is waited for through its Service's Endpoints, not its Job
- Flux.nu wait ends a health-check wait begun under an older spec or revision
- Registry_digest asks over HTTPS before plain HTTP
- Images on a tag that moves are pulled Always, and a stale one is reported
- An agent pod's pull back-off is waited for, up to 90 seconds
- Up.nu --dev with the registry in the cluster, or images pinned by digest
- Bin/polar-dev trusts a dev pod's SSH host keys per namespace
- The dev pod's TMPDIR is /tmp, which its user can write
- Skopeo is given TMPDIR as --tmpdir, since it does not read it
- The registry's port-forward is replaced when it does not answer
- The dev pod's browser step names bin/polar-dev forward
- The images step says why it builds again, and not "no u7s node"
- A clone retried while the git server comes back says so
- The dev pod has no memory limit, as it has no CPU limit
- Bin/polar-dev says when the cluster does not answer, and the forward hints fit the dev pod
- Up.nu's prepared images hold a list, not only null
- Quieter certificate import and sync from the dev pod
- Load a Neo4j dump with the image the cluster runs, and through Flux
- The seed's re-render and re-apply print what changed, not everything again
- The dev pod trusts the checkout's .envrc, which does nothing there
- The resolve tests ask a registry the cluster has
- What a target renders and never applies is in no layer, in every layer check
- Up.nu moves another cluster's certificates aside in the dev container too
- A port-forward counts as running only when it reaches this cluster
- Images are evicted from every node of a multi-node cluster
- The targets' layers wait on the agents' Deployments
- A run is an agent the scheduler serves, and the catalog's topics count it
- User-cert-install finds Librewolf's profile on macOS too
- The port-forwards on macOS, without setsid or /proc

**jira**

- The cursor's time comes from the system field `updated`, not its English name

**jira-observer**

- Check JIRA_URL at startup and notice a base64-encoded token
- Bound connecting to Jira and each request
- Check the token first and retry a failed pull instead of exiting

**lib**

- The graph controller's settings tests run on a runtime, as ractor's v2 output port needs

**load-harness**

- A resources file of one document is read as one
- A failed run is kept and recorded, and the sweep goes on
- Find the broker by its Deployment's pod label

**load-plans**

- The baseline measures one connection, since pods of one workload share an identity

**logging**

- Export only warnings as traces unless a deployment asks for more

**neo4j**

- Give its volumes to the user it runs as, before it starts
- Its run directory, an emptyDir, so the PID file is written

**nix**

- A change to Scudo no longer rebuilds every dependency

**pipeline**

- Pipeline.dhall no longer lists the harness controller, as pipeline.nix already did not

**polar-agent-builder**

- Provisioned-ca is a deployment-only capability, as the catalog has it since 1c

**polar-init**

- The escrow bootstrap has a deadline, so a pod cannot wait in init for good
- A pod restarted in place keeps the certificate it already holds

**polar-sign**

- An empty list takes its element type from its siblings

**polar-test-graph**

- Remove the Neo4j container with its anonymous volumes

**ractor**

- Output ports no longer drop a subscriber that falls behind, so no client loses its events

**release**

- The release's commits carry the trailers whoever cuts it gives, not a fixed one
- Cert-issuer's breaking note says what 2.0.0 deploys, a Deployment, not the Job its footer named

**scheduler-observer**

- The first clone waits for a git server that does not answer yet

**secrets**

- Update Secrets from the SOPS files when they change

**test**

- Tests/charts.nu sets POLAR_NODE_REGISTRY, as render.nu does

**up**

- Step 1 checks the cluster's Kubernetes and this machine's openssl

**Other**

- Clear the lint findings that predate this work in every crate but three, which follow on their own
- Clear the rest of the lint findings, so `cargo clippy --workspace --all-targets -- -D warnings` passes
- Add fixes to put arbitrary cap on wire frame length and address potential busy-spin for accept loop †

</details>

### ⚡ Performance

**broker**

- Each connection keeps its decisions for a few seconds, and a change of authority ends them all

**cassini-broker**

- A publish's span is at debug, so the default filters do not create it

**cassini-client**

- A span records a message by its kind, not its Debug, so a publish no longer formats its payload

**infra**

- An agent's scaffold is cached under its image's digest
- The hand-written charts' Dhall renders are kept in the render cache

**polar-custody**

- Sign with ring, five times the throughput, and wipe the key ring will not

**polar-scudo**

- Every binary copies with LLVM libc's memcpy and memmove

† Classified from its wording: the commit was not written as a conventional commit.

Also in this release, not listed: 196 documentation, 16 housekeeping, 12 refactoring, 5 build and 5 test commits.

## [1.0.0] - 2026-10-16

> The first tracked release of Polar/ACE, after four years of development. It introduces a calculated platform version and this changelog, a lock that pins every image by digest, a render that is split into layers and identical on every run, Flux's controllers rendered into the repository, an optional-Cassini standalone git server, a static tools image, and images.nu import and publish for loading prebuilt images.

### 💥 Breaking changes

- *(dev-env)* Rename dropbear helpers to ssh-*
  the start-dropbear helper has been renamed to ssh-start (and ssh-stop for teardown). Any scripts, docs, or muscle memory that relied on start-dropbear need to be updated to the new names.
- *(credentials-agent)* Signed admin actions, two-phase account claim and bootstrap admin
  polar-admin-grant no longer accepts --granted-by, --revoked-by, --provisioned-by or --account-id; authorship comes from the signing certificate and the consumer mints account ids. The event model gains AdminActionRecorded. The unsigned GRANTS, REVOCATIONS and ACCOUNT_PROVISIONING subscriptions are still in place and are to be removed.

### 📦 Images

| Image | Change |
|---|---|
| `ace-viz:latest` | new, `sha256:f593d481b916` |
| `build-processor:latest` | new, `sha256:1706f19107fc` |
| `busybox:latest` | new, `sha256:f97baa533a26` |
| `cassini:latest` | new, `sha256:bf9a277828e2` |
| `cert-issuer:latest` | new, `sha256:9b4f104ea885` |
| `credentials-agent-consumer:latest` | new, `sha256:bf70f353c78c` |
| `credentials-agent-observer:latest` | new, `sha256:e00a2f05d872` |
| `flux-kustomize-controller:v1.9.6` | new, `sha256:cdab039ed285` |
| `flux-source-controller:v1.9.6` | new, `sha256:d10ea2ebeb47` |
| `git-controller:latest` | new, `sha256:8f463d6b334b` |
| `git-observer:latest` | new, `sha256:7c9bdc6c3b9d` |
| `git-processor:latest` | new, `sha256:bd2b1bdba70b` |
| `gitlab-consumer:latest` | new, `sha256:676c6fa4c44c` |
| `gitlab-observer:latest` | new, `sha256:fb5a13a3416e` |
| `harness-producer:latest` | new, `sha256:4e875cf93533` |
| `harness-sink:latest` | new, `sha256:2a2e94f7a0b5` |
| `jaeger:2.13.0` | new, `sha256:862eb898dc23` |
| `jira-observer:latest` | new, `sha256:b517469c7698` |
| `jira-processor:latest` | new, `sha256:976918df3b03` |
| `keycloak:26.7.1` | new, `sha256:7523ccfbd950` |
| `kube-consumer:latest` | new, `sha256:86b578117152` |
| `kube-observer:latest` | new, `sha256:f7230aae267d` |
| `local-path-provisioner:v0.0.35` | new, `sha256:b8b3e3f87a91` |
| `neo4j:5.26.2` | new, `sha256:30e755f597ff` |
| `oci-resolver:latest` | new, `sha256:9d69c091912f` |
| `openapi-observer:latest` | new, `sha256:5859b4e5074c` |
| `openapi-processor:latest` | new, `sha256:bf17c71c9f1e` |
| `polar-git-server:latest` | new, `sha256:42ce87efaabc` |
| `polar-init:latest` | new, `sha256:a619fd616e62` |
| `polar-nu-init:latest` | new, `sha256:e38b30b38815` |
| `polar-tools:latest` | new, `sha256:3a46b0a70f6b` |
| `postgres:16` | new, `sha256:75adc2a65806` |
| `scheduler-observer:latest` | new, `sha256:ae83174cc8d8` |
| `scheduler-processor:latest` | new, `sha256:59203f830e5d` |
| `secret-harness:latest` | new, `sha256:4f5cb01f99b3` |

### 🚀 Features

- **ace**: 1
- **agent-builder**: 2
- **auth**: 1
- **authority**: 7
- **build-orchestrator**: 2
- **builder**: 6
- **capabilities**: 2
- **cassini**: 4
- **cassini-broker**: 4
- **cassini-client**: 3
- **catalog**: 5
- **cert-issuer**: 21
- **cert-issuer-common**: 2
- **cert-issuer/client**: 1
- **commands**: 3
- **container**: 1
- **containers**: 2
- **credentials**: 10
- **credentials-agent**: 48
- **credentials-agent-admin**: 1
- **credentials-agent-common**: 2
- **credentials-agent-consumer**: 2
- **crypto**: 6
- **dev**: 2
- **flux**: 1
- **git**: 5
- **git-controller**: 4
- **git-observer**: 3
- **git-server**: 4
- **graph**: 4
- **harness**: 2
- **healthcheck**: 1
- **infra**: 50
- **init**: 1
- **jira**: 1
- **just**: 1
- **keycloak**: 1
- **kind**: 1
- **kube-consumer**: 2
- **kubernetes**: 1
- **lib**: 5
- **logging**: 2
- **neo4j**: 1
- **polar-agent-builder**: 16
- **polar-config-check**: 2
- **polar-crypto**: 8
- **polar-env**: 5
- **polar-escrow**: 1
- **polar-health**: 6
- **polar-init**: 2
- **polar-kernel-keyring**: 4
- **polar-permissions**: 1
- **polar-policy**: 2
- **polar-router**: 1
- **polar-sign**: 1
- **polar-signing**: 1
- **policy**: 3
- **policy-config**: 1
- **render**: 1
- **resolver**: 1
- **runtime**: 4
- **scheduler**: 11
- **scheduler-observer**: 1
- **sign**: 2
- **signing**: 2
- **startup**: 1
- **tests**: 1
- **tools**: 1
- **up**: 1
- **Other**: 63

<details><summary>All 371 entries</summary>

**ace**

- Integrate ACE dashboard as a Polar-native service

**agent-builder**

- A self-test so an agent that is registered but deaf is replaced
- The self-test on every agent that uses the builder, and a probe topic without colons

**auth**

- Deploy Keycloak+Postgres in-cluster; docs/dev-shell tooling; port-forward fix

**authority**

- AuthorityUpdate, signed with the credentials agent escrow key, and the state a holder keeps
- The snapshot messages, and the leaf core that decides what to do with an update or a snapshot
- The AuthorityActor keeps the permissions and policy actors current and announces changes
- Every Cassini client agent hosts the permissions and policy actors and keeps them current from the credentials agent
- Policy is asked before the certificate whether a signer may change a section, a variable or run a command
- AuthorityBook, what the credentials agent signs and holds
- An agent can hand the builder policy and permissions actors it already made

**build-orchestrator**

- Add container.dhall for git-clone init container
- Migrate clone container to nix-container-lib

**builder**

- Derive agent health config from a shared capability vocabulary
- ServiceBuilder, the infrastructure of a service that is not a Cassini client
- The ConfigurationActor owns the agent's configuration and takes versioned updates
- The ConfigurationActor verifies a signed document before it applies anything
- The policy lookup says whether no policy is loaded or none answered
- A capability can be optional, so one binary runs alone or in a cluster

**capabilities**

- C:run-command
- C:update-configuration gates configuration, c:update-policy stays for policy

**cassini**

- Implement graceful shutdown and distributed tracing
- Publisher identity, extracted from verified mTLS certs, threaded end to end
- Both ends find a dead peer in about a minute
- The broker is a PolarAgent on the builder, as a service rather than a client

**cassini-broker**

- A disconnected session is kept for ten minutes, a goodbye ends it, and then it is removed
- A new session of the same identity inherits what a vanished one held
- A new session also supersedes an older one of the same identity that still looks connected
- The shutdown has a budget, a watchdog and a grace period, and SIGTERM starts it

**cassini-client**

- The client declares the environment it reads
- A stable front for the connection, which replaces the transport itself
- A replacement transport asks to resume the last session

**catalog**

- The credentials agent consumer and observer are catalog workloads on a document
- The Cassini broker is a catalog workload, a service on a document; its hand chart is deleted
- The OCI resolver is a catalog workload on a document; its hand chart is deleted
- Polar-git-server is a catalog workload with its own image
- The scheduler observer is a catalog workload; its hand chart is deleted

**cert-issuer**

- Support multiple OIDC audiences; fix kube-observer Flux CRD handling
- Multi-issuer OIDC support
- Per-issuer identity normalization (Kubernetes + Keycloak)
- /validate endpoint + Kubernetes-vs-IdP credential classification (also: credentials-agent)
- /validate optionally returns the normalized identity, needed for client-side CSR construction
- CertType::Service -- a real, distinct X.509 type for agent-to-agent encryption identity
- Permission/PermissionScope -- the real, structured shape credentials-agent's own Grant.permissions has been deferring since it was first built
- Permission's own SAN-string encoding -- to_san_string/from_san_string, using a real crate for FQDN validation
- Extract_public_key_pem -- a certificate's own SubjectPublicKeyInfo, re-encoded as a standalone public-key PEM
- Read and embed real X.520 Subject DN fields from CSRs
- Extract and thread the standard jti claim through validation
- MTLS foundation for a new internal, token-free signing path
- Render the mtls config section from Dhall
- Handle_internal_issue -- token-free signing for allowlisted callers
- /internal/issue route and unauthenticated GET /ca
- Provisioned CA mode; CA generation moves to common
- TLS-only listener with optional client certificate
- Capability SANs and authoritative uid/mail in issued certificates
- Configurable account certificate lifetime
- The issuance floor, off until polar-init goes through the credentials agent
- Names are checked on every path, and vouched-only workloads are never signed on a token alone

**cert-issuer-common**

- Permission::extract_permissions -- the real read-side counterpart to to_san_string, genuinely general infrastructure
- Extract_own_identity, and TcpClient::as_actor_ref -- the two, real missing pieces before CryptoActor can be wired into kube-consumer (also: lib)

**cert-issuer/client**

- Add validate() to HandshakeClient, reused by credentials-agent

**commands**

- The command verifier and its time rules
- Agents declare commands, and a command actor runs and announces them
- Every agent hears its own commands, runs the declared ones, and announces each step

**container**

- Add dropbear auto-start and update help UX

**containers**

- Migrate devContainer to layered image + fix broken start.sh export + cleanup static-tools
- Migrate to nix-container-lib and Dhall-based configs

**credentials**

- The ledger numbers authority updates and remembers every revoked serial
- The authority publisher signs, numbers, publishes and applies policy, groups and revocations
- The policy and groups sections, each checked on arrival
- The consumer publishes signed policy, groups and revocations
- A pod may be issued a client and a server certificate, and not a second of either
- A workload asks for its certificates with its token sealed to an ephemeral session
- An escrowed secret can be bound to a context
- Labels on a stored secret, their validation, and the refusal reasons store will need
- The ledger keeps a secret's labels and refuses to change them
- StoreSecret asks Cedar first, with a floor for an identity's own names

**credentials-agent**

- Scaffold credentials_agent_common with CredentialsEvent
- Issuance wire types, generic Envelope, and the consumer's derived-state fold logic
- Consumer supervisor startup sequence
- Real grant/revoke topic handling -- milestone one complete
- Remaining issuance-flow prerequisites (also: cert-issuer)
- The complete issuance handler -- every piece tied together
- Wire the issuance handler into real Cassini traffic -- consumer side complete
- Replace the escrow mock with a real, generated keypair
- The observer -- POST /issue, the low-trust gateway
- Polar-admin-grant -- the admin CLI tool
- Identity-normalization flow, and a critical fix to real message delivery
- User-cert-fetcher -- the end-user client tool, closing the loop for integration testing
- Persist the escrow keypair across restarts
- Neo4j graph projection -- best-effort, never gating this agent's own health
- ProjectionActor -- real catch-up-on-reconnect, correctly owned
- Escrow public-key query -- the actual handler and actor, closing out item 2 of the secret-escrow build order
- Certificate storage (item 3), plus real revocation modeling and closed reason enums it surfaced
- Certificate-lookup query -- item 4, one agent asking another's public cert to encrypt directly to it
- Secret-storage flow -- store half of item 5's general escrow mechanism
- Thread Permission through Grant/GrantRecord, replacing the deferred Vec<String>
- Polar-admin-grant's --permission flag now takes a real Permission, not a plain string
- AgentEscrowIssuanceRequest/Reply -- wire types for the third issuance lifecycle
- AgentEscrowIssuanceCompleted event, its own derived lookup, and graph projection
- AgentEscrowIssuanceHandler -- the real handler for the third issuance lifecycle
- AgentEscrowKeyRequest/Reply wire types; split CheckAgentEscrowCertificate into two ledger messages
- AgentEscrowKeyHandler -- the real handler for the agent escrow-key query
- AgentEscrowIssuanceActor/AgentEscrowKeyActor -- supervisor wiring, completing the third issuance lifecycle end to end
- RetrieveSecretRequest/Reply -- wire types for the general secret-retrieve mechanism
- RetrieveSecretHandler -- the real decrypt-reencrypt-destroy relay
- RetrieveSecretActor -- supervisor wiring for the general secret-retrieve mechanism, plus a real, latent test bug found and fixed
- EphemeralEncryptedPayload -- the X25519 key-exchange primitive for agent escrow bootstrap
- RetrieveAgentEscrowKeypairRequest/Reply -- wire types for the agent escrow-keypair bootstrap flow
- AgentEscrowKeypairBootstrapHandler -- the real handler for the agent escrow-keypair bootstrap flow
- AgentEscrowKeypairBootstrapActor -- supervisor wiring, completing the agent escrow-keypair bootstrap flow end to end
- The consumer half's real binary and complete deployment -- the Credentials Agent finally exists as a deployable service
- The observer half's real binary and deployment -- both halves of the Credentials Agent are now genuinely deployable
- Real container-build infrastructure -- both real binaries can now actually be built and packaged into images
- Native Polar account provisioning (ledger side)
- Relay signed admin actions through the observer
- Relay signed admin actions through the observer
- Add claim subcommand for taking possession of a provisioned account
- Move the observer onto the agent builder
- Move the consumer onto the agent builder
- The consumer takes its graph settings from the builder snapshot
- The consumer and observer are PolarAgents, still reading the environment their charts give them
- The consumer and observer declare the variables they read, and the std::env reads are gone
- A partial introspection setting is a startup error, not a silently disabled check
- The gateway serves an agent's escrow certificate

**credentials-agent-admin**

- Bootstrap-ca subcommand

**credentials-agent-common**

- EscrowedSecret::encrypt_to_parsed_key -- closes the real, caught-earlier PEM-reparsing inefficiency in EncryptForRecipient (also: polar-crypto)
- DecryptOnBehalfOfRequest/Reply -- the real wire types for the new, genuine authorization case

**credentials-agent-consumer**

- DecryptOnBehalfOfHandler -- the real, complete authorization+decrypt flow
- PermissionsActor/PolicyActor/DecryptOnBehalfOfActor wired into the real, live supervisor -- the complete, real flow now runs end-to-end

**crypto**

- An agent can retrieve a named secret from the credentials agent, decrypted with its escrow key in custody
- The crypto actor opens an escrowed secret addressed to its agent
- The binding of a secret, what a host pattern matches, and the git credential decision
- The CryptoActor registers secrets and performs named operations on them
- The escrow carries the kind label, and a missing certificate is retried
- A runtime section withdraws the secrets it leaves out

**dev**

- Add Rust-based commit-msg Git hook with nix packaging
- Admin flow recipes, user cert import, updated shell banner

**flux**

- Render Flux's controllers into the repository; add the sandbox-local target

**git**

- The observer is a PolarAgent and a catalog workload; its hand chart is deleted
- The processor and the scheduler are PolarAgents and catalog workloads; the git chart is deleted
- The observer says what became of each repository, and the graph shows it
- The controller announces one set, the observer reconciles it, and proposals have a table
- The graph shows what the controller decided about each proposal

**git-controller**

- The minimal controller that announces the repositories section
- The controller replaces the git scheduler in the image, catalog and targets
- The admission decision, as a pure function, and the canonical form of a proposed URL
- The controller takes proposals, judges them under the admission rules and announces the result

**git-observer**

- The credentials runtime section and its hand-over to the CryptoActor
- A repository names its credential, and the observer waits for it
- The observer restarts a worker whose configuration changed, and reads the refs a fetch advances

**git-server**

- Polar-git-server, a PolarAgent that serves git over mTLS
- Seed from infra, create repositories at runtime, record pushes
- The administrators are named apart from the repositories, and the dev targets name the dev accounts
- Run with or without a Polar cluster, from the same binary

**graph**

- Batched UpsertNodes op; jira/process onto shared GraphControllerActor (3 of 4)
- GraphOp::Transaction; jira/process issues.rs onto shared GraphControllerActor (4 of 4, complete)
- GraphControllerMsg::Query -- a real, generic read capability
- Pods owned by a Job, StatefulSet, DaemonSet or Node get an owner edge

**harness**

- Two pods that check the secret policy from inside the cluster
- The secret is named under the owner's identity, and a store by another is refused by policy

**healthcheck**

- Implement polar-healthcheck liveness probe system (kube-consumer exemplar)

**infra**

- Introduce layered infrastructure rendering pipeline
- Add SOPS-based secrets management (also: secrets)
- Wire secrets.nu into render pipeline (also: secrets)
- Wire build-orchestrator config and add local git server (also: containers)
- Vendor-neutral image distribution via OCI registry; README rewrite
- One ServiceAccount per workload; render-time uniqueness check
- Render cert-issuer's ca.provisioning setting from Dhall
- Cert-issuer deployment supports provisioned-CA mode
- Callers reach cert-issuer over https
- Provisioned-CA wiring for credentials-agent; render-time consistency check
- Neo4j gets its certificate from polar-init; own identity 'graph'
- Bootstrap namespace and ServiceAccount, allow credentials-agent on /internal/issue
- Run the local targets on a provisioned CA
- Target-aware up.nu orchestrator and render-derived image table
- Render kube agents from a configuration catalog
- Render the jira agents from the catalog on local-minimal
- Render the kube and jira agents from the catalog on the local target
- Split image building into build, push, mirror and sync stages
- Add image rows for the remaining agents
- Rules for a workload health and shutdown settings, with tests
- The catalog sets each agent shutdown budget, and derives its termination grace period
- Jobs replace a pod only after the old one is gone, and render refuses otherwise
- Fill an agent scaffold from what the catalog gives its workload, and say what does not fit
- A workload whose agent says its own needs reads one document from a ConfigMap
- The resolver asks the built image for the agent scaffold and check, and up.nu builds those images before it renders
- Up.nu seeds the first policy, signed by the first administrator
- Each workload says where it gets its certificates, and cert-issuer's floor is written from the catalog
- Phase 6 group 1, the secret harness pair is issued through the credentials agent
- Phase 6 group 2, the single-certificate workloads are issued through the credentials agent
- A graph agent presents the pod's one client certificate to Neo4j
- Phase 6 group 3, the graph agents are issued through the credentials agent
- Phase 6 group 4, the git server's certificates are issued through the credentials agent
- Neo4j's server certificate and the ACE job's client certificate come from the credentials agent
- Phase 6 done, cert-issuer enforces its floor on local-minimal
- Up.nu checks the secrets at step 1 instead of after the build
- Up.nu waits for the observer's port-forward to answer and retries the seed clone
- Up.nu restarts what changed and leaves the rest alone
- Up.nu moves another cluster's admin certificates aside, and the seed refusal says what happens next
- The render keeps what Dhall rendered, so a resume that changed nothing takes seconds
- Step 10 looks before it asks, and stops only for what is missing
- Up.nu says at the end whether a pod can reach the outside
- A target can leave a catalog workload out with `enabled: false`
- Resolve.nu scaffolds exports every agent's scaffold; the author's flow is documented
- Use podman or docker, whichever the user has, and say so
- Group a target's manifests into layers
- An image lock, so a render can name every image by digest
- Calculate the platform version from the commits since the last release
- Generate CHANGELOG.md from the classification that chooses the version
- Release.nu gates a release, then builds, locks, writes the changelog, commits and tags
- Images.nu import and publish, named polar-<image>:<tag>-<arch>

**init**

- A workload asks the credentials agent for its certificates with its token sealed to a session

**jira**

- Support Jira Cloud auth + search API alongside existing on-prem support

**just**

- Reach the Polar git server from a developer machine

**keycloak**

- Idempotent bootstrap script (realm, client, audience mapper, test user)

**kind**

- Add local Kind cluster support with image loading and deployment

**kube-consumer**

- CryptoActor wired in -- the real, first, genuine CryptoActor user
- The first agent on the PolarAgent trait, with Cassini implied for every agent

**kubernetes**

- The real, first proof RouterActor/DeliveryMode::Router actually works end-to-end -- kube-consumer (also: polar)

**lib**

- Generic hash-chained EventLog<E> -- first Credentials Agent implementation piece
- ReplyRouter::register_with_timeout -- owns register-publish-await-cancel as one, real, atomic operation
- The graph controller declares the environment it reads
- The graph controller is handed its settings, and args get a constructor
- The builder path holds the stable client front, and publish honours the connection state

**logging**

- A deaf consumer is visible in its log
- The log filters are dynamic, and an agent follows a signed change to them

**neo4j**

- Working mTLS with JVM truststore

**polar-agent-builder**

- AgentBuilder -- the real, foundational, shared sequencing every agent's own pre_start has, so far, hand-rolled separately
- CryptoHandle/CryptoClient -- wrapping CryptoActor's raw messaging behind an ordinary, real Rust API
- HealthHandle/HealthClient -- the second, real trait-based handle
- The type-state message flow -- Outbound<T> -> Encrypted/Plaintext -> .publish(), and TcpClient::from_actor_ref (also: lib)
- AgentBuilder complete -- .with_crypto() and trait-based handles throughout, closing out this crate's own, full design
- InfrastructureSupervisor -- the real, final, foundational piece, closing out today's own, complete builder design
- One orderly shutdown for every reason an agent can end
- Refuse to start when the deployment gives a drain less time than the code needs
- One environment check, shared by startup and the render time checker
- Polar-config-check, to judge an environment before the pod exists
- One snapshot of the environment, held by the supervisor
- A capability brings the sections of the components it uses
- The Cassini client is started from the snapshot
- Configure and spawn are separate steps
- The encryption capability brings the crypto section
- The PolarAgent trait, run::<A>(), and the --help, --scaffold and --check flags

**polar-config-check**

- Describe --json, the declarations as one JSON document
- An effective view, what an environment resolves to

**polar-crypto**

- CryptoActor -- the process-local gateway to the Credentials Agent, mirroring polar-health's own established pattern
- CryptoActor::RetrieveOwnEscrowKeypair -- the retrieve half of the agent escrow-keypair bootstrap flow, wired end to end
- RSA CSR generation support, plus a full, workspace-wide TLS-backend consolidation and a real, reusable Neo4j test fixture
- CryptoActor::EnsureOwnEscrowKeypair -- the complete agent escrow-keypair bootstrap sequence, retrieve-first with a real issuance fallback
- CryptoActor's full redesign -- DecryptOwnMessage, EncryptForRecipient, EnsureBootstrapped, and wiring it into every agent's real init flow (also: polar-init)
- The crypto actor is handed its settings
- The init container hands the workload a key handle (also: polar-init)
- One actor for the life of the agent, which releases its key when it stops

**polar-env**

- Declare an environment variable once, read, validate and describe it from that
- Let a declaration give its name in a constant
- A layered Config with mutability, a report, and transactional overrides
- A Config can be read as an Env
- The configuration document, a file as an environment, and choosing between it and the process environment

**polar-escrow**

- The text a document carries a sealed secret in; observer tests need no network

**polar-health**

- Record the rejuvenation phase in the health file
- Report a verdict instead of dying, and keep the file fresh
- Give the actor a Cassini disconnect deadline
- Refuse to start on settings that contradict each other
- Let dependent crates run the health clock on virtual time
- The liveness probe reads the configuration document the agent reads; docs: the agent form is a trait

**polar-init**

- The init container declares and judges its environment first
- Say goodbye to the broker when the escrow bootstrap is done

**polar-kernel-keyring**

- The standalone Linux kernel-keyring crate, plus the fix that made it real
- The real, kernel-native OAEP fix -- CryptoActor's own escrow-decrypt path never needs PKCS1v1.5 at all
- Adopt a key another process loaded
- Unload a key so it does not outlive its use

**polar-permissions**

- PermissionsActor -- the sole, real, authoritative source of what permissions an identity actually holds

**polar-policy**

- PolicyActor -- a real, generic authorization engine, wrapping Cedar rather than hand-rolling RBAC/ABAC logic
- Real entity attribute support -- ABAC, not just bare RBAC-style principal equality

**polar-router**

- RouterActor -- the real, deliberate fix for widespread, broadcast-driven message leakage across this codebase's own supervisors

**polar-sign**

- Template, sign and --push

**polar-signing**

- Believe an agent's escrow certificate on its own merits

**policy**

- A decision is allow, deny or not determined; principals have groups and capabilities; c:update-groups
- A request carries the resource owner and the actors own clock
- A secret's namespace and labels are attributes policy can speak of

**policy-config**

- Modularize repo logic into library for reuse

**render**

- Fail when a manifest the catalog rendered is not in the target apply order, or comes after the Job that needs it

**resolver**

- The OCI resolver is a PolarAgent on the builder

**runtime**

- The c:update-runtime capability, the runtime document type in routing, and per-section replay order
- The configuration actor verifies, validates and holds runtime sections, replaced whole, versions per section
- Agents declare runtime sections, the infrastructure hands them to the configuration actor, and --scaffold exports them
- A section can answer to one capability and never to policy

**scheduler**

- Add common types and messages for polar-scheduler
- Implement Git watcher observer to monitor schedule repository
- Implement ScheduleInfoProcessor core logic
- Add RootSupervisor to orchestrate scheduler components
- Add Nix container images for observer and processor
- The observer is a PolarAgent that pulls when the git server says so
- The processor is a PolarAgent and a catalog workload; its hand chart is deleted
- Signed configuration is delivered to the agent it is for
- Agents say what they did with a signed document, and the graph records it
- An agent that registers is sent the configuration held for it; a stale document no longer overwrites the latest
- The polar scheduler observer answers an agent that registers from its checkout; the graph is not read for state

**scheduler-observer**

- Add GitOps support and Dhall→JSON evaluation

**sign**

- Polar-sign makes and checks signed configuration documents
- Polar-sign runtime signs a section for an agent

**signing**

- Polar-signing, signed configuration documents
- Every signed payload carries a type, and a receiver accepts only the kind it expects

**startup**

- Re-factor container startup to better support multiple workflows

**tests**

- Add cassini-types test suite and update dev tooling

**tools**

- Ship the command-line tools as static binaries in a polar-tools image

**up**

- Leave healthy unchanged Jobs running on a re-run, and wait for Cassini before the clients

**Other**

- Add uids to pods, don't mark all containers as deleted †
- Add curl †
- Add meta observer †
- Add findutils to ci container †
- Add meta consumer, todos †
- Add grype, syft steps to ci †
- Add gitlab instance base_url to artifact download path †
- Adding in jira group functionality †
- Adding in users observer/consume †
- Add base interval env var to deployment, fetch and parse sboms in provenacne agent †
- Add shutdown and cleanup logic, counting producers †
- Add tracing to registration, publsh flows, add TODO comments †
- Add first swing at control plane, docs †
- Add cannonical reference nodes to container images for provenance †
- Adding in flat yaml and dhall files for a local k8s install †
- Adding the additional resources to the role †
- Add provenance deployment, dhall constants lib, type defs †
- Add simple backoff logic to cassini client, cassini harness uses broker registration as healthcheck †
- Add graph controller actor to centralize graph opertations, add unit and integration tests †
- Create trait for the graphcontroller, implement for the linker agent †
- Add resolver json to its image †
- Add kubernetes graph controller and nodekeys, tests, docs †
- Add sbom artifact classification and retreival logic to resolver, tests, helpers, TOODs †
- Implement first pass at git commit observation and scheduling w/ smoke test (unstable) †
- Implement graph controller for git repo observation data †
- Replace containers.nix with nix-container-lib
- Replace containers.nix with nix-container-lib
- Add polar-agent container with pi_agent_rust and llama.cpp ROCm
- Add cassini client binary †
- Add daeomo mode, integration tests, docs for cassini client †
- Add AI tooling setup and initial workspace documentation
- Auto-select llama.cpp build variant based on host GPU architecture
- Switch nvidia agent container to llama-cpp with cuda
- Introduce cassini client abstraction, integrate build orchestrator with cassini client †
- Add Justfile, wire new workspace agents, integrate build-orchestrator
- Introduce build agent processor †
- Add cassini-broker test suite, agent task system, and infra updates
- Working static analysis pipeline with bash and nushell runners
- Add pipeline.dhall, mount tls certificates in orchestrator build jobs, make local dhall manifests more DRY and composable †
- Integrate teammate's Neo4j mTLS changes with configurable TLS
- Local cluster deployment pipeline
- Add nushell functions to extract oci layer information from nix built tarballs, emit artifact creation events with layer info, ingest container created events in the linker †
- Neo4j mTLS + rustls provider fixes for all consumer agents
- Add podman backend to build orchestrator, add unit tests, tweak nushell pipeline †
- Add dhall-to-nix to the dev container
- Add store-and-forward offline resilience to CassiniClient
- Store-and-forward daemon with deferred broker connection (closes #187)
- Working agent container
- Change dhall pinning recipe in Justfile to track main
- Add statefulness to ci.nu pipeline to track build_id, tighten provenance chain logic from commit to artifact, TODOs, update deployment dhalls to remove git scheduler †
- Polar-init single init container + healthcheck rejuvenation state machine
- Cassini rejuvenation via HealthCheckActor + per-cert-type lifetime in cert-issuer
- Per-identity cert lifetime overrides + jira polar-init migration
- Gitlab polar-init + HealthCheckActor rollout; jira Cloud fixes (WIP)
- Add license information to package nodes,further represent package status and security advisiories in the graph,stop using cargo aduit in CI †
- Kube-observer healthcheck rollout; graph availability deadline; bounded drain-on-exit
- Complete healthcheck/rejuvenation rollout, Deployment->Job conversion, and container healthcheck wiring for all remaining agents
- Adding in additional special characters that need replaced †
- Adding in sanitize keys for properties and other identifiers. Added special charater to check †
- Callers verify cert-issuer against a pinned Polar CA
- Cassini deadline default 120 s, and the best-effort broker decided, with what it loses pinned by tests
- Kube-observer, jira-observer and jira-processor are PolarAgents reading a ConfigMap document
- Every agent logs and traces as its deployment says, and every workload sends its traces to Jaeger

</details>

### 🐛 Bug fixes

- **ace**: 2
- **agents**: 1
- **build**: 1
- **builder**: 1
- **cassini**: 2
- **cassini-broker**: 5
- **cassini-client**: 4
- **cert-client**: 1
- **cert-issuer**: 5
- **containers**: 5
- **crate**: 1
- **credentials**: 4
- **credentials-agent**: 1
- **credentials-agent-observer**: 1
- **deps**: 2
- **git**: 5
- **gitlab**: 1
- **graph**: 2
- **images**: 1
- **infra**: 13
- **infra/neo4j**: 1
- **jira**: 1
- **jira-observer**: 2
- **jira-processor**: 2
- **just**: 5
- **justfile**: 3
- **kube**: 2
- **kube-consumer**: 1
- **kube-observer**: 1
- **kubernetes**: 2
- **lib**: 1
- **polar-agent-builder**: 1
- **polar-crypto**: 1
- **polar-health**: 5
- **polar-healthcheck**: 1
- **polar-sign**: 1
- **policy-config**: 1
- **resolver**: 2
- **scheduler**: 1
- **scripts**: 1
- **static-analysis**: 1
- **up**: 4
- **Other**: 47

<details><summary>All 145 entries</summary>

**ace**

- Survive Neo4j startup races and refresh stale dropdown data
- Eliminate pandas dependency in figures.py; guard sparse-data crashes in callbacks.py

**agents**

- Stop idle-triggered false health failures; graceful graph-controller shutdown

**build**

- Resolve post-merge compilation errors across agents

**builder**

- List the git server's two capabilities as deployment-only

**cassini**

- Resolve subscriber-tracking leak and broken disconnect propagation
- A refused shutdown request is said to be refused; the follow-ups from the migration

**cassini-broker**

- Send RegistrationResponse on new-session registration path
- A resumed client is replayed what it missed
- Answer a new session once, and close connections when stopped
- An instance that lost its session does not displace the one that took over
- The health actor ending starts the orderly shutdown instead of cutting it off

**cassini-client**

- Log successful registration
- A registration answer received while registered is accepted and reported
- A transport the facade supervises ends when its connection is lost, so the facade knows
- Each connection attempt is bounded, so a broker that returns is found within seconds

**cert-client**

- Use the rustls backend when presenting a client identity

**cert-issuer**

- Move ValidateResponse to cert_issuer_common, matching IssueRequest/Response/Error's existing placement
- Embed Permission-shaped extra SANs as SanType::URI, not DnsName -- plus a real, adjacent panic fixed
- Package.nix pointed at a directory that doesn't exist
- Stop logging bearer tokens and CSRs in tracing spans
- Build the image without a fake chroot, so it builds for arm64 under emulation

**containers**

- Improve dev container robustness and layering
- Resolve cross-platform portability issues in dev container builds
- Configure Nix at runtime based on container architecture
- Materialize nix db from store to writable overlay on startup
- Add cacert, tmp, and SSL env to git/jira/scheduler images

**crate**

- Rename cassini broker component

**credentials**

- RetrieveSecret checks policy and encrypts to the requester escrow key, not its mTLS key
- Only the identity that stored a secret may store that name again
- An issuance is recorded under the identity the certificate carries
- The service identity certificate is recorded under the identity it carries

**credentials-agent**

- Read TLS_CLIENT_CERT and TLS_CLIENT_KEY in the admin CLI, ignoring empty values

**credentials-agent-observer**

- Wire HealthCheckActor, fixing repeated liveness-probe kills

**deps**

- Add proptest to dev dependencies
- Update rustls to 0.23.46 and settle the cargo-deny findings

**git**

- Correct TcpClientMessage::Subscribe usage
- Fix git consumer binary name and observer cache root (also: infra)
- The observer clone cache is an emptyDir, and the migration is verified on the cluster
- One repository that cannot start no longer ends the observer; StopWorker works; harness can publish synthetic events
- The defaults and docs a scaffold gives are what a template should show

**gitlab**

- Correct RNG method and suppress enum warnings

**graph**

- Every schedule node key names its node by the variable it is given
- The unknown-owner warning says which owner it dropped

**images**

- Evict the node cached copy after every push, not only manifest-driven ones

**infra**

- Fix neo4j-bolt-ca ConfigMap migration and secrets pipeline (also: secrets)
- Sync local target's cert-issuer config to current multi-issuer shape
- Strip stale sops-style pin, re-freeze stale constants.dhall import hash
- Mount the provisioned CA Secret where cert-issuer reads it
- Print image-table errors as one clean line; record the staged image build
- Give the credentials agents the time to drain that their code needs
- The scaffold route removes the agent image a pull replaced
- Step 10 recognises a Neo4j still on its default password by what Neo4j really answers
- Step 10 waits for a Neo4j that was just started instead of failing
- The agent check pulls an agent by its image, not by its crate
- Up.nu loses no generation when a Deployment does not exist yet
- The Neo4j chart escapes a conf file's quotes, and ${ , for the Dhall string it goes into
- Release.nu checks render determinism once the lock exists

**infra/neo4j**

- Trust only the Polar CA in the JVM keystore

**jira**

- Correct irrefutable if let and RNG method

**jira-observer**

- Fail loudly on bad responses, poll at start, and pull issues incrementally
- Report the cause of a failed request, not just its top-level message

**jira-processor**

- Update a stale test assertion for sanitize_label's own, real property-key sanitization
- Complete the builder shutdown handshake so rejuvenation exits cleanly

**just**

- User-cert-install removes the old Polar certificates before importing
- Find the Librewolf profile under ~/.config/librewolf too
- User-cert-install no longer dies silently when ~/.librewolf is missing
- The admin account admin-provision makes can sign configuration and commands
- Admin-provision can name the account that signs, because the bootstrap certificate is spent

**justfile**

- Force linux platform for third-party image pulls, not host-auto-detected
- Make u7s-specific image-cache cleanup opt-out for other cluster runtimes
- Cluster-build-all never built credentials-agent; add keycloak/postgres registry pushes

**kube**

- Close missing parenthesis in PodState cypher pattern
- Exit non-zero when an agent fails to start

**kube-consumer**

- Complete the rejuvenation shutdown handshake and never exit 0

**kube-observer**

- Exit non-zero when the supervisor stops; no todo! on process-group events

**kubernetes**

- Update pod consumer/observer logic - Handle Graph::connect errors gracefully - Fix imports and unused variable warnings - Ensure consistent neo4rs query handling
- Correct variable naming and silence dead code

**lib**

- Correct graph MERGE statement formatting

**polar-agent-builder**

- A replaced Cassini client no longer replaces the crypto actor

**polar-crypto**

- Subscribe to reply topics on registration, not at construction

**polar-health**

- Refresh the health file on every tick in every phase
- Run one tick chain from startup and never block the handler
- Stop swallowing health file write failures
- Bound the dependency checks, run them together, and keep them off the tick
- Say the environment variable names when a relation fails

**polar-healthcheck**

- Make the liveness probe a freshness check and prove it with a kubelet simulation

**polar-sign**

- A template's commented-out field can be taken back into the file

**policy-config**

- Improve test stability and unicode handling

**resolver**

- Remove insecure HTTP credential candidates
- The registries to speak plain http to are declared, and the target names the development registry

**scheduler**

- Clean up warnings in mock agents

**scripts**

- Update neo4rs to 0.9.0-rc.8 and adjust Graph::connect call

**static-analysis**

- Make the runner run, let it run a subset, and give four crates their license

**up**

- A pod that is being deleted does not satisfy a wait
- A SOPS file that will not decrypt stops the secrets step only if a secret is missing, and the targets share their helpers
- The seed checks the signers capability first and runs before the browser step
- Read the coloured log, commit conventionally, and name the seeder as a git admin

**Other**

- Fix skopeo copy paths †
- Fix skopeo copy paths better †
- Fix dangling todo †
- Cleanup, fix bug reading licenses †
- Fix acr upload paths †
- Fix gitlab endpoint †
- Fixing the sending of data to write_all, add in changelog implementation †
- Fix dependency pathing issues with the openapi agent to support CI build †
- Revert gitlab graphql schema and cynic query enum to use lower snake case to fix deserialization bug, attempt to ignore warning †
- Augment provenance resolver credential resolution, containerimagerefs don't get ids til they're resolved, cleanup, todos †
- Fix start.sh script, rm ci image def, update oci repo path †
- Fix containers.nix, update flux oci manifests †
- Start.sh, fix containers.nix †
- Consolidate sandbox enviornment manifests, fix oci secret †
- Add safe directory in gitlab-ci.yml, fix shortsha †
- Fix infinish subscribe ack loop, collect metrics †
- Add additional NodeKey variants for graph controller, fix cassini logging, cleanup †
- Handle resolved sboms, tests, cleanup and update logic †
- Fix self-referential git commit parent relationships, cleanup git observation, rm compose.dhall †
- Apply cargo clippy --fix across entire workspace
- Add sudo phase for llama-server
- Fix regression from graph controller refactor, add prefixes to gitnodekey cypher †
- CopyToRoot → contents, run-dev-container improvements, cluster tooling
- Restore devShell functionality lost in containers.nix removal
- Restore gen-certs.nix from before e2a9cee
- Local deploy config and mTLS setup
- Use DOCKER_AUTH_JSON for registry secret, managed-csi storage class
- Containerd 2.2.x passwd symlink bug in all agent images
- Wire cert-manager and missing images into cluster workflow
- Get all Polar agents running in local cluster
- Wire cargoArtifacts into cassini and build-orchestrator builds
- Correct typo in dhall container-lib import path
- Remove broken ci-container
- Correct dhall container-lib import paths and restore update-dhall-pins recipe
- Correct nushell module imports and workspace-root discovery in pipeline scripts
- Shell issue with ssh
- Restore tcp_client to git scheduler state, re-export get_neo_config from polar crate root
- Update local flux kustomization to encode polar deployment ordering, fix bug in types library that mounted service account tokens as a dir †
- Fix cert-issuer image names, add KUBERNETES_CA_CERT env, add pids-limit to nix-usernetes †
- Format core pipeline module, fix flake derivation name for ociResolver, tweak build-procesor schema to use buildExecution as type †
- Correct erroneous kube agent publishing logic, oci-resolver no longer requires addtional config beyond docker config.json, ++integration tests for resolver †
- Delete-then-recreate Jobs on apply (immutable pod templates)
- Cassini-client session resume bug + kube-observer disconnect/reconnect handling
- Fix code smell silently ingesting nulls for sbom dependency graphing, enable cassini client to take publish payloads via stdin †
- Fixes for jira to be able to operate with changes from cassini and limit amount of data being pulled †
- Fix cross-arch dev builds: native rust target + guarded podman load †
- Give each agent on the builder a shutdown budget that fits its own drain

</details>

† Classified from its wording: the commit was not written as a conventional commit.

Also in this release, not listed: 164 documentation, 77 housekeeping, 61 refactoring, 21 test, 3 style and 3 build commits.

<details><summary>136 changes were not written as conventional commits and are counted as patches</summary>

- inherti commonuser (`5455d24`)
- typo (`0690e62`)
- update derivationname (`19d9c75`)
- update binary name (`4b9ac00`)
- mark pods and their containers as deleted, cargo hakari manage deps (`a9e2d5a`)
- use outputports to publish/subscribe to client registration events, remove multiple warnings, comments, todo (`ecb3571`)
- build ci cotnainer from scratch, configure nixbldusers (`36ad215`)
- copy polar-ci (`f38e765`)
- bump gitlab observer interval, add policy.json (`ab0125e`)
- separate skopeo copy calls (`fdf54be`)
- mkdir var/tmp (`11532c7`)
- extract image repo path from tag metadata, remove todo, cleanup (`a7b27cb`)
- bump base_interval (`eb1c455`)
- Update lib.rs (`a600c4f`)
- rm underscore (`9223f7e`)
- update gitlab endpoint (`9134818`)
- link resources observed in gitlab  to the instance node, docs, dhall stuff (`de04d9c`)
- bump interval (`d5042d8`)
- Update .gitlab-ci.yml (`a970142`)
- disable grype scan (`25d5830`)
- disable grype scan (`1bf204b`)
- Initial jira observer implementation to print project names. (`2cb50c0`)
- An initial jira-consumer that processes the Project data and pushes into neo4j. (`458785f`)
- scrape and ingest gitlab package file metadata (`49dc831`)
- use polar-dev env for ci, docs, add pkgs to dev env (`5350d6c`)
- call start.sh for ci entrypoint (`a16b059`)
- call start.sh outside of entrypoint (`b01d6b9`)
- generate sboms from cargo.toml, save them as artifacts in gitlab ci, docs (`35eb072`)
- forget static-tools.sh (`20efb58`)
- quiet cyclonedx, add policyJson to devimage (`28cf313`)
- mkdir var/tmp (`b1079ce`)
- login to acr (`d63dc8e`)
- make base interavl a string (`fc1b0d5`)
- Initial parsing of jira issue data, no preprocessing and sending as a string. (`1bfadde`)
- 'nixify' provenance agent to containerize it. parse sboms, handle backoff (`1f6cd3b`)
- Initial implementation of sending issue/building cypher for neo4j. (`d9d10e3`)
- Few updates move to 5 min loop, spelling correction. (`42ed806`)
- Initial implementation of a Jira observer/consumer (#80) (`f9cc4a4`)
- generisize example openapi agent, add package.nix for services (`f25edb6`)
- Updating neo4j labels for issue attributes, adding in changelog (`d254727`)
- explicit cassini client disconnect no longer triggers timeout flow, stop tcp client on disconnect (`136b1d7`)
- split cassini harness source and sink, add healthcheck, docs (`ebbce97`)
- start producer and sink agents with to run test (`3feea32`)
- update test harness shutdown protocol. Components shut each other down. (`0cfe8e6`)
- validate message checksums after cassini test harness runs (`6bc87fc`)
- update workspace.nix, cassini test producer stops when sink isn't present (`f5cdd71`)
- unignore cassini, make broker messages use results, cassini uses otel tracing lib (`a3c83c8`)
- continue adding tracing to publish and subscribe flows, add todos for future refactoring, rm optional values (`dc1776a`)
- continue adding tracing to client disconnect operations, add todos, comments (`b934b25`)
- continue adding tracing for client unsubscribe, disconnect. update docs (`2920872`)
- continue confiugring tracing, drop span guards before async calls, comments (`201504a`)
- continue tracing work, remove usage of call() from pub/sub flows (`7da5ccf`)
- refine tracing effort to start new context/traces for distinc operations (`abcabfd`)
- update maskfile to be more verbose, remove eval-systgem flag from nix commands. Update other docs (`7e235cf`)
- repair jira agent, rm unused dependencies on cassini (`ed7b7ce`)
- route cassini test harness exec flow through control plane, add timeout to producer (`8afc347`)
- consumers emit artifact discovery messages and add image reference nodes, redesign provenance resololution approach to generalized data model, docs (`9a9724f`)
- don't upload containers unless they're built on main (`1dcc8a1`)
- finish wiring consumer agents to dispatch logic, resolver agent uses proxy certs (`d690cc6`)
- start writing k8s manifests for provenance agents, disable vulnix b/c of firewalls in CI (`52bc221`)
- Updating role so k8s pulls the data. (`a93d41f`)
- Updating yamls/dhall to latest (`fe55058`)
- begin standardizing dhall types for local minikube deployments, combine manifest (`07aa074`)
- continue consolidating dhall configurations, move conf under src/conf (`df5fb7a`)
- use oras client in CI to upload rendered manifests to oci registries (`57e7781`)
- rm unneeded parameter to render script, explicitly set user variable in start.sh (`f07cda9`)
- disable start.sh (`de71d81`)
- start.sh (`1b2615f`)
- append newline to nix.conf in start.sh (`4b9c196`)
- mk oras login explicit, start-dev- starts 0.1.0 (`29465bd`)
- update sanbdox cassini dhall (`0d2a5c4`)
- restore oci-secret, SOPS prefers single secret files (`5fa73e5`)
- load docker config json into resolver secret (`9666eba`)
- restore ci script (`c848ffc`)
- tighten up provenance logic, consumer agents forward events directly to linker, improve deployment dhall (`2276ef1`)
- render manifests in plaintext, add default trust store to resolver agent, don't set DOCKER_CONFIG var, gitlab consumer won't emit provenance event (`058b2e2`)
- resolver agent handles missing manifests and other errors better, consolidate sandbox manifests further (`d5f7eaf`)
- cassini loads environment variables safely and fails gracefully when requred values not present. (`27e1b7a`)
- initial commit of logger code, unstable (`6a3f75c`)
- initial refactors to make broker more reslilient, idempotent topic creation (`c8866af`)
- finish refactoring idempotent topic and subscriber logic (`a9041c8`)
- define new test plan configuration, add controlmanager to cassini to enable introspection of state, update test harness (`67b48a3`)
- return test plan to original stated goals, refactor harness controller to support it, update cassini logging. (`33b9c9c`)
- cassini listenrs notify managers when clients disconnect, other code cleanup. dev container ensures owned cargo target dir (`4efe01e`)
- continue refactor of cassini test harness to include impvoements from cassini's TCP flow. (unstable) (`0f31f82`)
- continue refactoring cassini harness producer to use library improvements. (unstable) (`d2c2875`)
- finalize cassini test harness refactor, cleanup and emit client events (`a79005e`)
- cassini harness shuts down cleaner, unstable (`afaec12`)
- open http port for jaeger in docker compose (`e02c6ed`)
- use explcit message types in resolver agent fns, cleanup (`5be3f65`)
- Stop using serde_json values for graphcontroller query parameters (`836bc67`)
- disable rocksdb as part of the logger so it doesn't get built (`fc4744e`)
- separate provenance agent deployments (`a9496ae`)
- improve provenance modeling and idempotency, create OCIRegistries, update cypher queries and linkages to and from runtime artifacts (`104193a`)
- generisize graph controller for various kinds of graphops and node keys (`2f43c16`)
- make graphop compilation a standard lib function, add toods, gitlab agents create GitlabPipelineArtifact nodes (`bb0bb7e`)
- resolver agent uses json isntead of dhall as primary configuration (`97cf504`)
- Control gitlab queries (#125) (`beff444`)
- first swing at representing k8s states within the knowledge graph (`4a9ae2c`)
- represent pod containers' changing states, relationships to declared volumes (`2de42e8`)
- many improvements and changes to local deployments and agents (`7b7ede9`)
- ignore unused client events in agents (`0754515`)
- build cassini client separately from the lib (`788f6ac`)
- integrate build orchestrator (unstable) and configuration schemas into workspace, docs, cleanup for cassini client cli (`7047c54`)
- update job processor graph schema, move graphnodekeys to shared submodule, graphcontroller takes boxed nodekey, eliminating isolated vocabulary (`fb9015e`)
- tie orchestrator build jobs to k8s jobs using graphIdentity node keys, docs, define OCIArtifactCreatedEvent schema (`cb42ef9`)
- first pass at nix attested build, mv nu scripts, define core pipeline lib, etc. (`5a23492`)
- ingest sbom dependency data and cargo binaries at build time, persist them within the graph using the linker (`6b331ff`)
- subscribe linker to binary lined topic to persist binaries and their relationships in the knolwedge grpah (`e73650e`)
- commit docs for build orchestration and pipeline provenance flow, generate sboms for container images in the polar pipeline and prepare to ingest them (`41f63e7`)
- Use podman's API directly instead of compat API to orchestrate pods (`e4b8511`)
- use single remote import of the dhall container lib, rm deprecated flag in pipeline script (`117cc25`)
- use single remote import of the dhall container lib, rm deprecated flag in pipeline script (again) and unify pipeline nushell scripts (`bf3f782`)
- first pass of TDD implementation of certificate issueance agent and protocol, delete old docs and add spec for cert-issuer, update ci-container, add step-ca server to compose (`1bbea73`)
- Finalize first cut for certificate iIssuance client and server (#193) (`a1ab2c1`)
- repo cleanupm, ++certClient to ci and init environments, deprecate build-orchestrator, update deployment dhall lib  to support integration testing for cert-issuer (`c87b16e`)
- finalize implementation of mtls for neo4j deployments with polar agents, cert-issuance supports additional optional SANs for certificates, tests, cleanup (`965998e`)
- Extend Kube Agents to Watch Flux CRD Events (#206) (`4f6d748`)
- do not ignore rendered container nix definitions- they need version control sometimes.. (`6d04c88`)
- standardize, package, and publish dhall types library (`688497a`)
- infra: replace cert-manager with cert-issuer; migrate all agents to cert-client init container model (`6d1a557`)
- infra: freeze dhall imports and wire neo4j cert-issuer integration (`6838047`)
- infra: neo4j cert-issuer mTLS, agent neo4j client certs, browser mTLS (`a536e09`)
- infra: add neo4j-install-certs and neo4j-load-dump just recipes (`9425c83`)
- Operationalize build execution events (#216) (`363cf0a`)
- Closed the software supply chain provenance graph from source commit to running deployment. (`eaf81ab`)
- retire git agent scheduling in favor of deployment time configuraiton, add discover repo test binary, seed-graph script for demonstrating data modeling (`45eefd2`)
- type k8s processor's GraphOperable trait against CassiniClient trait instead of concrete TcpClient (`47c3433`)
- use nushell standard logging lib, update cert-issuer recipies in justfile, stop cassini-client from silently failing in daemon mode and update its nushell module (`e06a560`)
- use nushell standard logging lib, update cert-issuer recipies in justfile, stop cassini-client from silently failing in daemon mode and update its nushell module (`653afe2`)
- refine ci.nu pipeline script to have better cassini-client daemon handling, logging, resilience (`cc60053`)
- numerous improvements to ci scripts, pipeline flows. Code cleanup, justfile recipies (`91dd3e0`)
- Updating to allow duplicate fields to ignore if there is no value when overwriting duplicate fields. (`7273331`)
- security(graph): validate every remaining injection surface in the shared graph library (`b62c5c6`)
- Redesign dashboard views and input forms (`6ce5dd4`)
- Made changes with Dave approval (`d75d342`)

</details>
