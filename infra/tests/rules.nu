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

# infra/tests/rules.nu
#
# Tests of the rules in infra/rules.nu: the checks `resolve.nu check` runs over a
# workload's health and shutdown settings. The rules are pure functions of a record,
# so each case here is a record and the problems it should produce.
#
#   nu infra/tests/rules.nu
#
# The health rules are not here: they are Rust, in the agent, and the resolver asks the agent
# itself (`<agent> --check --config FILE`, see tests/resolve.nu). What is here is what only the
# deployment knows: the grace period against the budget, how Jobs replace pods, the apply order.

use std/assert
use ../rules.nu *

const GOOD_SHUTDOWN = { budget_secs: 20, grace_margin_secs: 10, termination_grace_secs: 30 }

def has [problems: list<string>, needle: string] {
    $problems | any {|p| $p | str contains $needle }
}

def main [] {
    let tests = {
        # docs/architecture/key-custody.md, step 4: the daemon holds one key of each type, so a pod given it has at most one
        # certificate of each.
        "a pod given the custody daemon has at most one certificate of each type": {
            assert equal (custody_cert_problems ["client:/etc/tls/certs:custody:" "server:/etc/tls/git-server:custody:git.svc"]) []
            let two = (custody_cert_problems ["client:/etc/tls/certs:custody:" "client:/etc/neo4j-client-tls:custody:"])
            assert equal ($two | length) 1
            assert ($two.0 | str contains "/etc/tls/certs") $two.0
            assert ($two.0 | str contains "/etc/neo4j-client-tls") $two.0
            # Key files are not the daemon's: as many as the pod asks for.
            assert equal (custody_cert_problems ["client:/a:ecdsa-p256:" "client:/b:ecdsa-p256:"]) []
        }

        # With the daemon, the token buys only the custodian's certificate (docs/architecture/key-custody.md, "Part A's
        # bootstrap: the custodian"), so a certificate with a key file beside the daemon's is refused, as polar-init does.
        "a pod given the custody daemon has no certificate with a key file": {
            let mixed = (custody_cert_problems ["client:/etc/tls/certs:custody:" "server:/etc/tls/git-server:ecdsa-p256:git.svc"])
            assert equal ($mixed | length) 1
            assert ($mixed.0 | str contains "/etc/tls/git-server") $mixed.0
            assert ($mixed.0 | str contains "custodian") $mixed.0
        }

        # The escrow key is only the custody daemon's custodian's to fetch (key-custody.md, step 9): a pod whose polar-init
        # is asked to fetch it and that has no daemon is refused, as polar-init would refuse it (the maintainer, 2026-10-28).
        "a pod asked to escrow its key needs the custody daemon": {
            let escrow = [{ name: "POLAR_INIT_ESCROW_BOOTSTRAP", value: "true" }]
            let off = (escrow_problems false $escrow)
            assert equal ($off | length) 1
            assert ($off.0 | str contains "custody") $off.0
            assert equal (escrow_problems true $escrow) []
            assert equal (escrow_problems false []) []
            assert equal (escrow_problems false [{ name: "POLAR_INIT_ESCROW_BOOTSTRAP", value: "false" }]) []
            assert equal (escrow_problems false [{ name: "POLAR_INIT_CA_CERT", value: "true" }]) []
        }

        # docs/architecture/placement.md: a workload goes to its pool's node only on a target that declares the pool,
        # with both the selector and the toleration (nix-usernetes' contract); a pool the target lacks is refused.
        "a workload is placed only where the target declares its pool": {
            let declared = { pools: ["core" "observers" "writers"] }
            assert equal (placement_of $declared "writers") { key: "polar.dev/pool", pool: "writers" }
            assert equal (placement_problems $declared "writers" "kube-consumer") []
            # the open pool's, and any workload on a target that declares no placement: nothing
            assert equal (placement_of $declared null) null
            assert equal (placement_problems $declared null "ace") []
            assert equal (placement_of null "writers") null
            assert equal (placement_problems null "writers" "kube-consumer") []
            # a pool the target does not declare
            let lacking = (placement_problems { pools: ["core"] } "writers" "kube-consumer")
            assert equal ($lacking | length) 1
            assert ($lacking.0 | str contains "kube-consumer") $lacking.0
            assert ($lacking.0 | str contains "writers") $lacking.0
            assert equal (placement_of { pools: ["core"] } "writers") null
        }

        # A container's memory (the broker's, 2026-11-01: its limit is the memory.max its controller reads): a request
        # and a limit, each a quantity of whole bytes, Ki, Mi, Gi or Ti, the request no more than the limit.
        "a workload's memory is a request and a limit, each a quantity, the request no more than the limit": {
            assert equal (memory_bytes "1Gi") 1073741824
            assert equal (memory_bytes "512Mi") 536870912
            assert equal (memory_bytes "4096") 4096
            for bad in ["1G" "1.5Gi" "-1Gi" "lots" "" 1024] {
                assert equal (memory_bytes $bad) null $"($bad)"
            }
            assert equal (memory_problems null "cassini") []
            assert equal (memory_problems { request: "1Gi", limit: "1Gi" } "cassini") []
            assert equal (memory_problems { request: "512Mi", limit: "1Gi" } "cassini") []
            let over = (memory_problems { request: "2Gi", limit: "1Gi" } "cassini")
            assert equal ($over | length) 1
            assert ($over.0 | str contains "cassini") $over.0
            assert ($over.0 | str contains "over its limit") $over.0
            let unread = (memory_problems { request: "1Gi", limit: "1G" } "cassini")
            assert equal ($unread | length) 1
            assert ($unread.0 | str contains "limit") $unread.0
            assert equal (memory_problems { limit: "1Gi" } "cassini" | length) 1 "a request is needed"
            assert equal (memory_problems "1Gi" "cassini" | length) 1 "a record, not a quantity"
        }

        # docs/architecture/deployments.md, step 4: a pod that runs polar-init restarts in place, one at a time.
        "a Deployment whose pod runs polar-init restarts in place, one pod at a time": {
            let rule = { action: "RestartAllContainers", exitCodes: { operator: "NotIn", values: [] } }
            let good = {
                kind: "Deployment"
                metadata: { name: "kube-consumer" }
                spec: {
                    replicas: 1
                    strategy: { type: "Recreate" }
                    template: { spec: {
                        restartPolicy: "Always"
                        initContainers: [{ name: "polar-custody", restartPolicy: "Always" } { name: "polar-init" }]
                        containers: [{ name: "kube-consumer", restartPolicy: "Always", restartPolicyRules: [$rule] }]
                    } }
                }
            }
            assert equal (in_place_problems [$good]) []
            # a Deployment without polar-init (Keycloak, Jaeger) is not one of these
            let plain = ($good | update spec.template.spec.initContainers [] | update spec.template.spec.containers [{ name: "keycloak" }] | update spec.strategy { type: "RollingUpdate" })
            assert equal (in_place_problems [$plain]) []
            let cases = [
                [what needle doc];
                ["two replicas" "replicas" ($good | update spec.replicas 2)]
                ["a rolling update" "Recreate" ($good | update spec.strategy { type: "RollingUpdate" })]
                ["no strategy" "Recreate" ($good | reject spec.strategy)]
                ["no rule" "RestartAllContainers" ($good | update spec.template.spec.containers.0 { name: "kube-consumer", restartPolicy: "Always" })]
                ["exit 1 only" "every exit" ($good | update spec.template.spec.containers.0.restartPolicyRules [{ action: "RestartAllContainers", exitCodes: { operator: "In", values: [1] } }])]
                ["the container left to the pod" "restartPolicy" ($good | update spec.template.spec.containers.0 { name: "kube-consumer", restartPolicyRules: [$rule] })]
            ]
            for c in $cases {
                let p = (in_place_problems [$c.doc])
                assert equal ($p | length) 1 $c.what
                assert (has $p "kube-consumer") $c.what
                assert (has $p $c.needle) $"($c.what): ($p)"
            }
        }

        # docs/architecture/authority-model.md, "Multi-instance workloads": a workload whose entry says `instances: "many"`
        # runs many pods at once, each an instance of the one identity; the render marks its object so.
        "a multi-instance Deployment may run more than one pod, and is held to every other rule": {
            let rule = { action: "RestartAllContainers", exitCodes: { operator: "NotIn", values: [] } }
            let many = {
                kind: "Deployment"
                metadata: { name: "build-job", annotations: { $INSTANCES_ANNOTATION: "many" } }
                spec: {
                    replicas: 3
                    strategy: { type: "Recreate" }
                    template: { spec: {
                        initContainers: [{ name: "polar-init" }]
                        containers: [{ name: "build-job", restartPolicy: "Always", restartPolicyRules: [$rule] }]
                    } }
                }
            }
            assert equal (in_place_problems [$many]) []
            # Only the replica rule is lifted: a rolling update is still refused.
            let rolling = (in_place_problems [($many | update spec.strategy { type: "RollingUpdate" })])
            assert equal ($rolling | length) 1
            assert (has $rolling "Recreate") $"($rolling)"
            # An object marked anything else is one instance.
            let one = (in_place_problems [($many | update metadata.annotations { $INSTANCES_ANNOTATION: "one" })])
            assert (has $one "replicas") $"($one)"
        }

        "a run's Job runs one pod at a time unless its workload is multi-instance": {
            let job = {|parallelism, annotations| { kind: "Job", metadata: { name: "harness-producer", annotations: $annotations }, spec: { parallelism: $parallelism, podReplacementPolicy: "Failed", template: { spec: { initContainers: [{ name: "polar-init" }], containers: [{ name: "harness-producer" }] } } } } }
            assert equal (parallelism_problems [(do $job 1 {})]) []
            assert equal (parallelism_problems [(do $job 4 { $INSTANCES_ANNOTATION: "many" })]) []
            let p = (parallelism_problems [(do $job 4 {})])
            assert equal ($p | length) 1
            assert (has $p "harness-producer") $"($p)"
            assert (has $p "parallelism") $"($p)"
            # A Job without polar-init is not one of these.
            let plain = ((do $job 4 {}) | update spec.template.spec.initContainers [])
            assert equal (parallelism_problems [$plain]) []
        }

        "a workload's instances is one or many, and many only for a workload the credentials agent issues": {
            assert equal (instances_problems "kube-observer" { instances: "one", issuer: "credentials-agent", bootstrap: false }) []
            assert equal (instances_problems "harness-producer" { instances: "many", issuer: "credentials-agent", bootstrap: false }) []
            assert equal (instances_problems "x" { issuer: "cert-issuer", bootstrap: true }) [] "absent is one"
            let bad = (instances_problems "x" { instances: "several", issuer: "credentials-agent", bootstrap: false })
            assert equal ($bad | length) 1
            assert (has $bad "\"one\" or \"many\"") $"($bad)"
            # Only the credentials agent writes the instance into a certificate: a bootstrap service, or a workload still
            # issued by cert-issuer, would be one certificate whose pods supersede each other.
            assert (has (instances_problems "cassini" { instances: "many", issuer: "cert-issuer", bootstrap: true }) "credentials agent")
            assert (has (instances_problems "x" { instances: "many", issuer: "cert-issuer", bootstrap: false }) "credentials agent")
        }

        "a Job that replaces pods only after they are gone passes": {
            let docs = [{ kind: "Job", metadata: { name: "kube-consumer" }, spec: { podReplacementPolicy: "Failed" } }]
            assert equal (job_replacement_problems $docs) []
        }

        "a Job with the default policy is a problem, named": {
            let docs = [{ kind: "Job", metadata: { name: "kube-consumer" }, spec: {} }]
            let p = (job_replacement_problems $docs)
            assert equal ($p | length) 1
            assert (has $p "kube-consumer")
            assert (has $p "podReplacementPolicy")
        }

        "the other policy is a problem too": {
            let docs = [{ kind: "Job", metadata: { name: "jira-processor" }, spec: { podReplacementPolicy: "TerminatingOrFailed" } }]
            assert equal (job_replacement_problems $docs | length) 1
        }

        "a tag that moves is recognised, and a version or a digest is not": {
            assert (is_mutable_ref "10.0.2.2:5000/cassini:latest")
            assert (is_mutable_ref "10.0.2.2:5000/cassini")
            assert (is_mutable_ref "cassini")
            assert (not (is_mutable_ref "10.0.2.2:5000/neo4j:5.26.2"))
            assert (not (is_mutable_ref "10.0.2.2:5000/cassini:latest@sha256:abc"))
            assert (not (is_mutable_ref "localhost:5000/postgres:16"))
        }

        "a container on a tag that moves must be pulled Always; the problem names the object, the container and the image": {
            let job = { kind: "Job", metadata: { name: "cassini" }, spec: { template: { spec: {
                containers: [{ name: "cassini", image: "10.0.2.2:5000/cassini:latest", imagePullPolicy: "IfNotPresent" }]
                initContainers: [{ name: "polar-init", image: "10.0.2.2:5000/polar-init:latest", imagePullPolicy: "Always" }]
            } } } }
            let p = (pull_policy_problems [$job])
            assert equal ($p | length) 1
            assert (has $p "Job cassini")
            assert (has $p "container cassini")
            assert (has $p "10.0.2.2:5000/cassini:latest")
            assert (has $p "IfNotPresent")
        }

        "init containers count, an unset policy is left to Kubernetes' own default, and a version or a digest may be IfNotPresent": {
            let ok = { kind: "StatefulSet", metadata: { name: "db" }, spec: { template: { spec: {
                containers: [
                    { name: "db", image: "10.0.2.2:5000/postgres:16", imagePullPolicy: "IfNotPresent" }
                    { name: "pinned", image: "10.0.2.2:5000/x:latest@sha256:abc", imagePullPolicy: "IfNotPresent" }
                    { name: "unset", image: "10.0.2.2:5000/y:latest" }
                ]
            } } } }
            assert equal (pull_policy_problems [$ok]) []
            let bad = { kind: "Deployment", metadata: { name: "d" }, spec: { template: { spec: {
                containers: [{ name: "c", image: "10.0.2.2:5000/c:latest", imagePullPolicy: "Always" }]
                initContainers: [{ name: "i", image: "10.0.2.2:5000/polar-init:latest", imagePullPolicy: "IfNotPresent" }]
            } } } }
            assert (has (pull_policy_problems [$bad]) "container i")
        }

        "objects without a pod spec are not asked, and a CronJob's template is": {
            assert equal (pull_policy_problems [{ kind: "ConfigMap", metadata: { name: "c" }, data: {} } { kind: "Service", metadata: { name: "s" } }]) []
            let cron = { kind: "CronJob", metadata: { name: "x" }, spec: { jobTemplate: { spec: { template: { spec: {
                containers: [{ name: "c", image: "r/c:latest", imagePullPolicy: "IfNotPresent" }]
            } } } } } }
            assert equal (pull_policy_problems [$cron] | length) 1
        }

        "things that are not Jobs are not asked for the policy": {
            let docs = [
                { kind: "Deployment", metadata: { name: "cert-issuer" }, spec: {} }
                { kind: "ServiceAccount", metadata: { name: "x" } }
            ]
            assert equal (job_replacement_problems $docs) []
        }

        "the default shutdown settings have no problems": {
            assert equal (shutdown_problems "w" $GOOD_SHUTDOWN) []
        }

        "a budget under a second is a problem": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update budget_secs 0 | update termination_grace_secs 10))
            assert (has $p "shutdown.budget_secs must be a whole number of seconds, at least 1")
        }

        "the grace period must exceed the budget": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update termination_grace_secs 20))
            assert (has $p "must exceed the shutdown budget")
            assert equal (shutdown_problems "w" ($GOOD_SHUTDOWN | update termination_grace_secs 21)) []
        }

        "a rendered manifest that the apply order does not list is a problem naming it": {
            let p = (apply_order_problems ["a.yaml" "a-resources.yaml" "b.yaml"] ["a.yaml" "b.yaml"])
            assert equal ($p | length) 1
            assert (has $p "a-resources.yaml")
        }

        "a manifest in the apply order is not a problem, and an order that lists more is fine": {
            assert equal (apply_order_problems ["a.yaml"] ["x.yaml" "a.yaml" "y.yaml"]) []
        }

        "a resources manifest must be applied before the workload that mounts it": {
            let p = (apply_order_problems ["a.yaml" "a-resources.yaml"] ["a.yaml" "a-resources.yaml"])
            assert equal ($p | length) 1
            assert (has $p "before")
        }

        "the margin must be at least a second": {
            let p = (shutdown_problems "w" ($GOOD_SHUTDOWN | update grace_margin_secs 0 | update termination_grace_secs 20))
            assert (has $p "shutdown.grace_margin_secs must be at least 1")
        }
        "a workload that asks cert-issuer and is not a bootstrap identity is pending until the floor is enforced, then an error": {
            let r = [{ who: "kube-consumer", issuer: "cert-issuer", bootstrap: false }]
            let before = (issuer_problems $r false)
            assert equal $before.errors []
            assert (has $before.pending "kube-consumer")
            let after = (issuer_problems $r true)
            assert (has $after.errors "kube-consumer")
            assert equal $after.pending []
        }

        "a bootstrap identity may ask cert-issuer, and a workload on the credentials agent is never a problem": {
            let r = [
                { who: "cassini", issuer: "cert-issuer", bootstrap: true }
                { who: "kube-consumer", issuer: "credentials-agent", bootstrap: false }
            ]
            for enforced in [false true] {
                let p = (issuer_problems $r $enforced)
                assert equal $p.errors []
                assert equal $p.pending []
            }
        }

        "a bootstrap identity cannot be on the credentials agent, and an unknown issuer is an error": {
            let p = (issuer_problems [{ who: "cassini", issuer: "credentials-agent", bootstrap: true }] false)
            assert (has $p.errors "cassini")
            let q = (issuer_problems [{ who: "x", issuer: "vault", bootstrap: false }] false)
            assert (has $q.errors "vault")
        }

        "the floor is the bootstrap identities with their type and names, from what they are issued": {
            let r = [
                { who: "cassini", namespace: "polar", service_account: "cassini", issuer: "cert-issuer", bootstrap: true,
                  certs: ["server:/etc/tls/certs:ecdsa-p256:cassini-ip-svc.polar.svc.cluster.local"] }
                { who: "credentials-agent", namespace: "polar", service_account: "credentials-agent", issuer: "cert-issuer", bootstrap: true,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:", "client:/etc/neo4j-client-tls:ecdsa-p256:"] }
            ]
            let f = (floor_entries $r)
            assert equal $f.problems []
            assert equal $f.entries [
                { workload: "system:serviceaccount:polar:cassini", certType: "SERVER", names: ["cassini-ip-svc.polar.svc.cluster.local"], vouchedOnly: false, publish: [], subscribe: [], groups: [] }
                { workload: "system:serviceaccount:polar:credentials-agent", certType: "CLIENT", names: [], vouchedOnly: false, publish: [], subscribe: [], groups: [] }
            ]
        }

        "a workload on the credentials agent gets a vouched-only entry for its server names, and none for client certificates": {
            let r = [
                { who: "polar-git-server", namespace: "polar", service_account: "polar-git-server", issuer: "credentials-agent", bootstrap: false,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:", "server:/etc/tls/git-server:ecdsa-p256:polar-git-server.polar.svc.cluster.local"] }
                { who: "kube-consumer", namespace: "polar", service_account: "kube-consumer", issuer: "credentials-agent", bootstrap: false,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:"] }
            ]
            let f = (floor_entries $r)
            assert equal $f.problems []
            assert equal $f.entries [
                { workload: "system:serviceaccount:polar:polar-git-server", certType: "SERVER", names: ["polar-git-server.polar.svc.cluster.local"], vouchedOnly: true, publish: [], subscribe: [], groups: [] }
            ]
        }

        "a bootstrap service's entry carries its declared and family topics, and not the implied ones": {
            let topic = {|name, access, from| { name: $name, access: $access, from: $from } }
            let r = [
                { who: "credentials-agent", namespace: "polar", service_account: "credentials-agent", issuer: "cert-issuer", bootstrap: true,
                  certs: ["client:/etc/tls/certs:ecdsa-p256:"]
                  groups: ["cassini-client" "self-test"]
                  topics: [
                    (do $topic "polar-authority-updates" "publish" "declared")
                    (do $topic "credentials-grants" "subscribe" "declared")
                    (do $topic "agent.x.configuration" "publish" "family")
                    (do $topic "polar-authority-snapshot-requests" "publish" "implied")
                    (do $topic "agent.credentials-agent.configuration" "subscribe" "implied")
                  ] }
                # A workload on the credentials agent has its topics in the credentials agent's floor, not here.
                { who: "polar-git-server", namespace: "polar", service_account: "polar-git-server", issuer: "credentials-agent", bootstrap: false,
                  certs: ["server:/etc/tls/git-server:ecdsa-p256:polar-git-server.polar.svc.cluster.local"]
                  topics: [(do $topic "polar-git-repo-changed" "publish" "declared")] }
            ]
            let f = (floor_entries $r)
            assert equal $f.problems []
            assert equal ($f.entries | first | select publish subscribe groups) { publish: ["agent.x.configuration" "polar-authority-updates"], subscribe: ["credentials-grants"], groups: ["cassini-client" "self-test"] }
            assert equal ($f.entries | last | select publish subscribe groups) { publish: [], subscribe: [], groups: [] }
        }

        "the authority model's facts are every workload, by its one identity, with its whole set of topics and their causes": {
            let topic = {|name, access, from, extra| { name: $name, access: $access, from: $from } | merge $extra }
            let r = [
                { who: "kube-observer", namespace: "polar", service_account: "kube-observer", issuer: "credentials-agent", bootstrap: false
                  groups: ["self-test" "cassini-client"]
                  topics: [
                    (do $topic "kubernetes.cluster.consumer" "publish" "declared" { purpose: "what it sees", causes: [{ kind: "outside", name: "the Kubernetes API" }] })
                    (do $topic "polar-authority-snapshot-requests" "publish" "implied" { rule: "agent", causes: [{ kind: "registered" }] })
                    (do $topic "polar-authority-snapshot-replies" "subscribe" "implied" { rule: "agent", answers: [{ kind: "registered" }] })
                  ] }
                # A bootstrap service is in the model too, though cert-issuer issues it: the invariants run through it.
                { who: "credentials-agent-observer", namespace: "polar", service_account: "credentials-agent-observer", issuer: "cert-issuer", bootstrap: true
                  groups: ["cassini-client"]
                  topics: [(do $topic "credentials-workload-issuance-requests" "publish" "declared" { causes: [{ kind: "http", name: "/workload/issue" }] })] }
            ]
            let m = (model_facts $r "cluster.local")
            assert equal $m.format 1
            assert equal ($m.workloads | get workload) ["credentials-agent-observer" "kube-observer"]
            let kube = ($m.workloads | where workload == "kube-observer" | first)
            assert equal $kube.identity "kube-observer.polar.serviceaccount.cluster.local"
            assert equal $kube.issuer "credentials-agent"
            assert equal $kube.bootstrap false
            assert equal $kube.groups ["cassini-client" "self-test"]
            # Every topic of the set, the implied ones included, with what it says about causes.
            assert equal ($kube.topics | get name) ["kubernetes.cluster.consumer" "polar-authority-snapshot-replies" "polar-authority-snapshot-requests"]
            assert equal ($kube.topics | where name == "polar-authority-snapshot-replies" | first | get answers) [{ kind: "registered" }]
            assert equal ($kube.topics | where name == "kubernetes.cluster.consumer" | first | get causes) [{ kind: "outside", name: "the Kubernetes API" }]
            let observer = ($m.workloads | where workload == "credentials-agent-observer" | first)
            assert equal [$observer.issuer $observer.bootstrap] ["cert-issuer" true]
            # How many pods of it run at once: one unless its entry says many (authority-model.md, "Multi-instance
            # workloads"), which issuance reads to write the instance into its certificates.
            assert equal [$kube.instances $observer.instances] ["one" "one"]
            let many = (model_facts ($r | update 0 { merge { instances: "many" } }) "cluster.local")
            assert equal ($many.workloads | where workload == "kube-observer" | first | get instances) "many"
            # One identity per workload: the token's subject is nowhere in it (authority-model.md, "Decisions").
            assert (not ($m | to json | str contains "system:serviceaccount")) "a token subject in the model"
        }

        "a workload whose identity is not its name has no place in the model": {
            let r = [{ who: "kube-observer", namespace: "polar", service_account: "default", issuer: "credentials-agent", bootstrap: false, groups: [], topics: [] }]
            let refused = (try { model_facts $r "cluster.local"; null } catch {|e| $e.msg })
            assert ($refused | str contains "kube-observer") $"($refused)"
        }

        "a bootstrap identity that is issued two types cannot be written as one floor entry": {
            let r = [{ who: "x", namespace: "polar", service_account: "x", issuer: "cert-issuer", bootstrap: true,
                       certs: ["client:/a:ecdsa-p256:", "server:/b:ecdsa-p256:x.polar"] }]
            let f = (floor_entries $r)
            assert (has $f.problems "x")
        }

        "a workload still on cert-issuer that is not a bootstrap identity has no floor entry yet": {
            # Its names stay unchecked until it moves; the issuer rule is what reports it.
            let r = [{ who: "k", namespace: "polar", service_account: "k", issuer: "cert-issuer", bootstrap: false,
                       certs: ["server:/b:ecdsa-p256:k.polar"] }]
            assert equal (floor_entries $r).entries []
        }
    }

    let results = ($tests | transpose name body | each {|t|
        try {
            do $t.body
            { name: $t.name, error: null }
        } catch {|e|
            { name: $t.name, error: $e.msg }
        }
    })

    for r in $results {
        if $r.error == null {
            print $"  ok    ($r.name)"
        } else {
            print -e $"  FAIL  ($r.name): ($r.error)"
        }
    }

    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
