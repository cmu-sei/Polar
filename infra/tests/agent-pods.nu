#!/usr/bin/env nu

# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# What the agent pods are made of and how their answers are read (infra/agent-pods.nu). The pure parts only; the cluster
# side is exercised by every render and by tests/resolve.nu, which ask the agents through it.

use std/assert
use ../agent-pods.nu [agent_pod, check_problems, command_of_config, deny_all_policy, document_config_map, namespace_manifest, pod_outcome, scaffold_answer, stale_namespaces, DOCUMENT_PATH, SCRATCH_VALUE]

def terminated [code: int] { { status: { phase: (if $code == 0 { "Succeeded" } else { "Failed" }), containerStatuses: [{ state: { terminated: { exitCode: $code, reason: "Completed" } } }] } } }
def waiting [reason: string] { { status: { phase: "Pending", containerStatuses: [{ state: { waiting: { reason: $reason, message: "back-off pulling image" } } }] } } }

def main [] {
    let tests = [
        {
            name: "the pod runs the image's own command followed by the question, once, with nothing it does not need"
            body: {
                let p = (agent_pod "ns1" "scaffold-cassini" "10.0.2.2:5000/cassini@sha256:abc" ["/bin/cassini-server" "--scaffold"])
                let c = ($p.spec.containers | first)
                assert equal $c.command ["/bin/cassini-server" "--scaffold"]
                assert ("args" not-in ($c | columns)) "args would replace the image's Cmd"
                assert equal $c.image "10.0.2.2:5000/cassini@sha256:abc"
                assert equal $p.spec.restartPolicy "Never"
                assert equal $p.spec.automountServiceAccountToken false
                assert equal $p.metadata.namespace "ns1"
                assert equal $c.env []
            }
        }
        {
            name: "the pod meets the restricted Pod Security profile and has a writable /tmp on a read-only root"
            body: {
                let p = (agent_pod "ns1" "x" "img@sha256:abc" ["/bin/a"])
                let c = ($p.spec.containers | first)
                assert equal $p.spec.securityContext.runAsNonRoot true
                assert ($p.spec.securityContext.runAsUser > 0)
                assert equal $p.spec.securityContext.seccompProfile.type "RuntimeDefault"
                assert equal $c.securityContext.allowPrivilegeEscalation false
                assert equal $c.securityContext.capabilities.drop ["ALL"]
                assert equal $c.securityContext.readOnlyRootFilesystem true
                assert equal ($c.volumeMounts | where mountPath == "/tmp" | length) 1
                assert equal ($p.spec.volumes | where name == "tmp" | first | get emptyDir) {}
            }
        }
        {
            name: "a check pod mounts its document where the agent is told to read it, and is given the variables it names"
            body: {
                let p = (agent_pod "ns1" "check-kube" "img@sha256:abc" ["/bin/a" "--check" "--config" $DOCUMENT_PATH] "check-kube" [{ name: "TOKEN", value: "x" }])
                let c = ($p.spec.containers | first)
                let m = ($c.volumeMounts | where mountPath == $DOCUMENT_PATH | first)
                assert equal $m.subPath "config.json"
                assert equal ($p.spec.volumes | where name == $m.name | first | get configMap.name) "check-kube"
                assert equal $c.env [{ name: "TOKEN", value: "x" }]
                let cm = (document_config_map "ns1" "check-kube" "{\"format\":1}")
                assert equal ($cm.data | get "config.json") "{\"format\":1}"
            }
        }
        {
            name: "the scratch namespace is labelled to be found again, dated, restricted, and closed to the network"
            body: {
                let n = (namespace_manifest "polar-agent-abc123" "2026-10-19T13:00:00-04:00")
                assert equal ($n.metadata.labels | get "polar.dev/scratch") $SCRATCH_VALUE
                assert equal ($n.metadata.labels | get "pod-security.kubernetes.io/enforce") "restricted"
                assert equal ($n.metadata.annotations | get "polar.dev/created-at") "2026-10-19T13:00:00-04:00"
                let np = (deny_all_policy "polar-agent-abc123")
                assert equal $np.spec.podSelector {}
                assert equal $np.spec.policyTypes ["Ingress" "Egress"]
                assert ("ingress" not-in ($np.spec | columns)) "no ingress rule: nothing is allowed"
                assert ("egress" not-in ($np.spec | columns)) "no egress rule: nothing is allowed"
            }
        }
        {
            name: "the image's command is its Entrypoint then its Cmd, and an image with neither is refused"
            body: {
                assert equal (command_of_config { config: { Cmd: ["/bin/cassini-server"] } }) ["/bin/cassini-server"]
                assert equal (command_of_config { config: { Entrypoint: ["/bin/tini" "--"], Cmd: ["/bin/a"] } }) ["/bin/tini" "--" "/bin/a"]
                assert ((try { command_of_config { config: {} }; false } catch { true }))
            }
        }
        {
            name: "a pod is done when its container has exited, stuck when its image cannot be pulled, and running otherwise"
            body: {
                assert equal (pod_outcome (terminated 0)).state "done"
                assert equal (pod_outcome (terminated 0)).exit 0
                assert equal (pod_outcome (terminated 1)).exit 1
                assert equal (pod_outcome (waiting "ImagePullBackOff")).state "backoff" "the kubelet pulls again by itself; run_pods times the back-off"
                assert equal (pod_outcome (waiting "ErrImagePull")).state "stuck" "a refused pull, not a throttled one"
                assert ((pod_outcome (waiting "ErrImagePull")).reason | str starts-with "ErrImagePull")
                assert equal (pod_outcome (waiting "ContainerCreating")).state "running"
                # a throttled pull is the kubelet's rate limit, which it retries by itself: not stuck
                let throttled = { status: { containerStatuses: [{ state: { waiting: { reason: "ErrImagePull", message: "pull QPS exceeded" } } }] } }
                assert equal (pod_outcome $throttled).state "running"
                let refused = { status: { containerStatuses: [{ state: { waiting: { reason: "ErrImagePull", message: "manifest unknown" } } }] } }
                assert equal (pod_outcome $refused).state "stuck"
                assert equal (pod_outcome { status: { phase: "Pending" } }).state "running"
                assert equal (pod_outcome {}).state "running"
                let deadline = (pod_outcome { status: { phase: "Failed", reason: "DeadlineExceeded" } })
                assert equal $deadline.state "done"
                assert equal $deadline.exit null
            }
        }
        {
            name: "a scaffold is the agent's JSON, and anything else, or a failure, is refused and names the agent"
            body: {
                assert equal (scaffold_answer "cassini" 0 "{\"agent\":{\"name\":\"cassini\"}}") "{\"agent\":{\"name\":\"cassini\"}}"
                for bad in [
                    { exit: 1, logs: "{\"agent\":{}}" }
                    { exit: null, logs: "" }
                    { exit: 0, logs: "INFO starting\n{\"agent\":{}}" }
                    { exit: 0, logs: "" }
                    { exit: 0, logs: "[1,2]" }
                ] {
                    let msg = (try { scaffold_answer "cassini" $bad.exit $bad.logs; "accepted" } catch {|e| $e.msg })
                    assert ($msg | str starts-with "cassini:") $"($bad | to nuon): ($msg)"
                }
            }
        }
        {
            name: "a check's problems are its ERROR lines, a sound document has none, and a failure that names none is one"
            body: {
                assert equal (check_problems "kube-observer" 0 "ok: the configuration is sound\n") []
                assert equal (check_problems "kube-observer" 1 "ERROR: missing field `format`\nERROR: budget too small\n") ["kube-observer: missing field `format`" "kube-observer: budget too small"]
                let silent = (check_problems "kube-observer" 2 "panicked at main.rs\n")
                assert equal ($silent | length) 1
                assert (($silent | first) | str contains "exit code 2")
                assert (((check_problems "kube-observer" null "") | first) | str contains "without an exit code")
            }
        }
        {
            name: "a scratch namespace is stale only when it is older than an hour, by its own date or, lacking one, the cluster's"
            body: {
                let now = ("2026-10-19T14:00:00Z" | into datetime)
                let ns = {|name, created, k8s| { metadata: { name: $name, annotations: (if $created == null { {} } else { { "polar.dev/created-at": $created } }), creationTimestamp: $k8s } } }
                let all = [
                    (do $ns "old" "2026-10-19T12:30:00Z" "2026-10-19T12:30:00Z")
                    (do $ns "fresh" "2026-10-19T13:45:00Z" "2026-10-19T13:45:00Z")
                    (do $ns "undated-old" null "2026-10-19T10:00:00Z")
                    (do $ns "undated-fresh" null "2026-10-19T13:59:00Z")
                ]
                assert equal (stale_namespaces $all $now) ["old" "undated-old"]
                assert equal (stale_namespaces [] $now) []
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
    })
    for r in $results {
        if $r.error == null { print $"  ok    ($r.name)" } else { print -e $"  FAIL  ($r.name): ($r.error)" }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
