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

# Egress: a proxy and a CA for the calls outside the cluster

Written 2026-10-30, as built. Some networks let a pod out only through an HTTPS proxy, and some inspect TLS
with a CA of their own that the image does not trust. A target says so in one place, `cluster.egress`, and the
agents that call outside the cluster are given it. A target that says nothing renders exactly as it did before,
and needs nothing new.

## The setting

Under `cluster:` in `infra/targets/<target>/config.nuon` (merged over `infra/config/cluster.nuon`, which has only
a commented description of it and no default, `infra/config/cluster.nuon:67`). Every key is optional:

```nuon
egress: {
  proxy: "http://proxy.corp.example:3128"       # http:// or https://, a host and a port, no user or password
  no_proxy: [".corp.example", "10.20.0.0/16"]   # reached directly, beside the defaults
  ca_file: "trust/corp-root-ca.pem"             # PEM CA certificates, a path inside infra/targets/<target>/
}
```

`egress` absent, `{}`, or every key null is no egress. The CA file is committed in the target's directory; it holds
CA certificates, which are public.

`settings_of` (`infra/egress.nu:207`) judges it before anything is rendered and refuses, saying why:

| Refused | Message (abridged) |
|---|---|
| `egress` not a record | `cluster.egress is <type>: it is a record with any of proxy, no_proxy, ca_file` |
| an unknown key | `cluster.egress.<k> is not a setting: ...` |
| a proxy that is not `http(s)://host:port` (a trailing `/` at most) | `cluster.egress.proxy is '<v>': a proxy is an http:// or https:// URL with a host and a port, ...` |
| a proxy with a user or password | `cluster.egress.proxy names a user or password: it is written into a ConfigMap, ...` (the value is not repeated) |
| `no_proxy` without `proxy` | `... there is nothing to bypass` |
| `no_proxy` not a list | `cluster.egress.no_proxy is <type>: it is a list of hosts, .domain suffixes, IP addresses and CIDRs` |
| an entry that is not a host, `.suffix`, address or CIDR | `cluster.egress.no_proxy entry '<e>' is not a host, ... (no commas, spaces or empty entries)` |
| `*` | `... which sends nothing through the proxy: remove cluster.egress.proxy instead` |
| a `ca_file` that is absolute or has `..` | `cluster.egress.ca_file is '<p>': it is a path inside infra/targets/<t>` |
| one that is not a file | `cluster.egress.ca_file <p> does not exist (it is read relative to infra/targets/<t>)` |
| one over 1 MiB (a ConfigMap's limit) | `... is <n> bytes, more than a ConfigMap holds` |
| one holding a private key | `... holds a private key: it must hold only CA certificates, and it is published in a ConfigMap` |
| one with no `CERTIFICATE` block, or a block that is not base64 | `... holds no PEM certificate ...`, `... holds a CERTIFICATE block that is not base64` |

It returns `{ proxy, no_proxy, ca_pem }`, the bypass list with its defaults (`no_proxy_of`, `infra/egress.nu:128`):
`localhost`, `127.0.0.1`, `::1`, `.svc`, `.<cluster.domain>`, then the target's entries, each once, in order. The
defaults keep every Service of the cluster (Cassini, the graph, the git server, the credentials agent) off the proxy.

## How it stays opt-in

The agent's code decides. An agent that calls outside the cluster registers `polar::egress::SECTION`
(`src/agents/lib/src/egress.rs:57`) among its sections, so its scaffold declares `POLAR_EGRESS_PROXY`,
`POLAR_EGRESS_NO_PROXY` and `POLAR_EGRESS_CA_CERT`. The resolver gives egress only to a workload whose scaffold
declares `POLAR_EGRESS_PROXY` (`egress_entries`, `infra/egress.nu:241`); there is no catalog flag. Every other
pod is unchanged whatever the target sets. A catalog entry or a target's `env` that names a `POLAR_EGRESS_*`
variable is refused (`infra/resolve.nu:473`): the setting is the cluster's.

## What a pod is given

`resolve` appends the entries after the authority-model and floor blocks, before the document is filled
(`infra/resolve.nu:589`):

- with a proxy, `POLAR_EGRESS_PROXY` and `POLAR_EGRESS_NO_PROXY` (comma-joined), written into the agent's
  configuration document like any other variable;
- with a CA, the ConfigMap `<workload>-egress-ca` (key `ca.pem`) in `<workload>-resources.yaml`
  (`resources.config_maps`, `infra/resolve.nu:662`; `main resources` lists the document first,
  `infra/resolve.nu:1092`), a volume `polar-egress-ca` of it, a read-only mount at `/etc/polar-egress-ca`, and
  `POLAR_EGRESS_CA_CERT=/etc/polar-egress-ca/ca.pem`.

Nothing else changes: not the init container, its environment or mounts, nor custody. `render-catalog.nu` already
renders a workload's `config_maps`, the resources file is applied before the workload, and `apply.nu` and the
published tree fingerprint every ConfigMap a pod mounts, so a changed CA restarts the pod. The agent judges its
document at render time (`--check`): the relations refuse a proxy that cannot be one and a bypass list without a
proxy. The CA file itself is read when the agent starts.

## What each agent does with it

`polar::egress::Egress` (`src/agents/lib/src/egress.rs`):

- `ca_bundle` (`:160`) reads every certificate of the file (`Certificate::from_pem_bundle`). Set and unreadable,
  empty, or holding a certificate that does not parse stops the agent, with the variable and the path.
- `configure` (`:191`) adds them to a reqwest `ClientBuilder` beside the system's roots. It keeps reqwest's default
  TLS backend, native-tls, which reads the image's `SSL_CERT_FILE` bundle; `use_rustls_tls` would trust
  webpki-roots in its place. With a proxy it sets `Proxy::all` with the bypass list; without one it calls
  `no_proxy()`, so `HTTPS_PROXY` in the process never decides the way out.
- `pem_blocks` (`:176`) gives each certificate as PEM text of its own, for oci-client.
- `proxy_for` (`:211`) answers, for a URL, the proxy to use by the same rules reqwest follows (hyper-util's
  `Matcher`): none for a bypassed host, an ssh remote, or with no proxy set.

The agents that declare the section:

- **jira-observer**: `get_client` (`src/agents/jira/observe/src/supervisor.rs:161`) is the existing client with its
  connect and request timeouts, configured by `Egress::configure`. `PROXY_CA_CERT` is gone; it used rustls, which
  dropped the image's roots, and read only the first certificate of the file.
- **oci-registry-resolver**: `client_config` (`src/agents/resolver/src/supervisor.rs:67`) sets oci-client's
  `https_proxy`, `http_proxy` and `no_proxy`; `oci_client` (`:105`) gives one `Certificate { Pem }` per PEM block
  (oci-client reads only the first certificate of each) and makes the client with `try_from`, since `new` falls back
  to a default client, without the CA, on an error. `PROXY_CA_CERT` is gone.
- **git-repo-observer**: at start (`src/agents/git/observe/src/supervisor.rs:155`) a CA is read through `ca_bundle`
  and then given to libgit2 once, `git2::opts::set_ssl_cert_file` (`trust_ca_file`,
  `src/agents/git/observe/src/lib.rs:98`), before any worker exists: it is global to the process, and libgit2 adds it
  to OpenSSL's default locations. Each worker is given `proxy_for` its repository's URL
  (`src/agents/git/observe/src/lib.rs:293`), and `build_fetch_options` (`:562`) sets `ProxyOptions::url` for the clone
  and every fetch; with none, libgit2 connects directly.

The outside-access probe at the end of `up.nu` is skipped for a target with a proxy (`egress_report`,
`infra/egress.nu:82`; `infra/up.nu:770`): it runs git in polar-scheduler-observer, which does not use it.

## Found on the way

- nixpkgs' OpenSSL reads `NIX_SSL_CERT_FILE`, not `SSL_CERT_FILE`, for its default file (shown with `openssl
  s_client` in the dev shell). reqwest's native-tls still honours `SSL_CERT_FILE`, through openssl-probe, but libgit2
  in the dev shell does not see it at all; in the image it uses OpenSSL's built-in default file. The git test sets both
  variables.
- reqwest 0.12.28 is built here with `default-tls`, `rustls-tls-webpki-roots` and `system-proxy`;
  `TlsBackend::default()` is native-tls when both are compiled and `http3` is off.

## Tests

`infra/tests/egress.nu` (every refusal, the defaults, two properties over 200 random cases: the bypass list starts with
the defaults with no duplicate and keeps the rest in order; nothing is given without egress and the declaration),
`infra/tests/resolve.nu` (egress absent and `{}` resolve every workload of local, local-minimal and sandbox-local the
same; with a proxy and a CA exactly the three agents gain exactly the egress entries), `src/agents/lib/tests/egress.rs`
(in child processes: the egress CA trusted beside `SSL_CERT_FILE`, no egress ignoring `HTTPS_PROXY`; a two-certificate
bundle; a call outside arriving at a proxy as `CONNECT host:443`; a property that cluster hosts bypass the proxy), and
each agent's own.

## Not done

- No proxy credentials: the value is written into a ConfigMap. A proxy that needs them would need a Secret.
- The CA comes only from a committed file; an existing ConfigMap or Secret in the cluster cannot be named.
- gitlab-observer still reads `PROXY_CA_CERT` from the process (nothing sets it; its chart's always-None plumbing is
  gone), and so does `polar::get_web_client`, which nothing calls. The gitlab agents, openapi and build-processor
  declare the section when they move onto the agent builder.
- With no proxy set, oci-client's reqwest client still consults the process's `HTTP(S)_PROXY` (it has no switch for
  the system proxy). A Polar pod sets neither.
- The render asks for the proxy's port spelled out; the agent's own check accepts a URL without one (the scheme's).
- The init container (polar-init) and the custody daemon reach only the cluster, so they are given nothing.
