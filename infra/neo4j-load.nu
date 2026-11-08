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

# infra/neo4j-load.nu
#
# Loads a Neo4j dump into the cluster's graph database, replacing its `neo4j` database. The `system` database, which holds the
# users and the password, is not touched, so the agents' password still works afterwards.
#
#   nu infra/neo4j-load.nu <dump file>        (just neo4j-load-dump <dump file>)
#
# Neo4j is stopped for it, since a database can be loaded only offline:
#   1. the Flux Kustomization that applies the StatefulSet, if there is one (a gitops target), is suspended: it would scale
#      Neo4j back up halfway through;
#   2. the StatefulSet is scaled to 0, and its pod waited out (the data volume attaches to one pod at a time);
#   3. a loader pod is started with Neo4j's own image, as the user Neo4j runs as (so what it writes is Neo4j's), with the data
#      volume where Neo4j has it;
#   4. the dump is streamed into it and `neo4j-admin database load neo4j --overwrite-destination` run;
#   5. the loader is deleted, Neo4j scaled back to 1 and waited for, and the Kustomization resumed.
# Step 5 runs whatever happened before it. Every kubectl call is printed before it runs.

const NS = "polar-graph"
const STS = "polar-neo4j"
const LOADER = "neo4j-loader"
const DATA_PVC = "polar-db-data"
const DATA_DIR = "/var/lib/neo4j/data"
# Where Neo4j runs, from its StatefulSet: the image's user and group.
const NEO4J_UID = 7474

def --wrapped kc [...args: string] {
    print $"+ kubectl ($args | str join ' ')"
    ^kubectl ...$args
    if $env.LAST_EXIT_CODE != 0 { error make --unspanned { msg: $"kubectl ($args | first 2 | str join ' ') failed" } }
}

# The Flux Kustomization that applies the StatefulSet, from the label Flux puts on what it applies, or null.
export def owning_kustomization [labels: record] {
    let name = ($labels | get -o "kustomize.toolkit.fluxcd.io/name")
    if $name == null { null } else { { name: $name, namespace: ($labels | get -o "kustomize.toolkit.fluxcd.io/namespace" | default "flux-system") } }
}

# The loader pod: Neo4j's image and user, the data volume where Neo4j mounts it, and scratch volumes for the dump and /tmp. Pure.
export def loader_pod [image: string] {
    {
        apiVersion: "v1", kind: "Pod"
        metadata: { name: $LOADER, namespace: $NS }
        spec: {
            restartPolicy: "Never"
            securityContext: { runAsUser: $NEO4J_UID, runAsGroup: $NEO4J_UID, fsGroup: $NEO4J_UID, runAsNonRoot: true }
            volumes: [
                { name: "data", persistentVolumeClaim: { claimName: $DATA_PVC } }
                { name: "dump", emptyDir: {} }
                # The image has no /tmp; neo4j-admin's configuration and logs go there.
                { name: "tmp", emptyDir: {} }
            ]
            containers: [{
                name: "loader", image: $image, command: ["sleep" "3600"]
                securityContext: { allowPrivilegeEscalation: false, capabilities: { drop: ["ALL"] } }
                volumeMounts: [{ name: "data", mountPath: $DATA_DIR }, { name: "dump", mountPath: "/dump" }, { name: "tmp", mountPath: "/tmp" }]
            }]
        }
    }
}

# What the loader runs: neo4j-admin with a configuration of its own that names the data directory, loading /dump/neo4j.dump.
export def load_command [] {
    [
        "mkdir -p /tmp/conf /tmp/logs"
        $"printf 'server.directories.data=($DATA_DIR)\\nserver.directories.logs=/tmp/logs\\n' > /tmp/conf/neo4j.conf"
        "NEO4J_CONF=/tmp/conf NEO4J_HOME=/opt/neo4j /opt/neo4j/bin/neo4j-admin database load neo4j --from-path=/dump --overwrite-destination=true"
    ] | str join " && "
}

def main [dump: path] {
    let dump = ($dump | path expand)
    if not ($dump | path exists) { error make --unspanned { msg: $"no dump at ($dump)" } }

    let sts = (^kubectl -n $NS get statefulset $STS -o json | from json)
    let image = ($sts.spec.template.spec.containers | first | get image)
    let flux = (owning_kustomization ($sts.metadata.labels? | default {}))
    print $"loading ($dump) into ($NS)/($STS), with its image ($image)"

    let result = (try {
        if $flux != null {
            print $"Flux applies ($STS) \(Kustomization ($flux.namespace)/($flux.name)\): suspending it until Neo4j is back"
            kc -n $flux.namespace patch kustomization $flux.name --type merge -p '{"spec":{"suspend":true}}'
        }
        kc -n $NS scale statefulset $STS --replicas=0
        kc -n $NS wait --for=delete $"pod/($STS)-0" --timeout=180s
        print $"+ kubectl apply -f - \(the loader pod: ($image), as ($NEO4J_UID), ($DATA_PVC) at ($DATA_DIR)\)"
        loader_pod $image | to json | ^kubectl apply -f -
        if $env.LAST_EXIT_CODE != 0 { error make --unspanned { msg: "the loader pod was not created" } }
        kc -n $NS wait --for=condition=Ready $"pod/($LOADER)" --timeout=180s
        print $"+ kubectl -n ($NS) exec -i ($LOADER) -- cat > /dump/neo4j.dump   \(< ($dump)\)"
        ^cat $dump | ^kubectl -n $NS exec -i $LOADER -- bash -c "cat > /dump/neo4j.dump"
        if $env.LAST_EXIT_CODE != 0 { error make --unspanned { msg: "copying the dump into the loader failed" } }
        kc -n $NS exec $LOADER -- bash -c (load_command)
        null
    } catch {|e| $e.msg })

    # Put everything back, whatever happened above.
    do { ^kubectl -n $NS delete pod $LOADER --ignore-not-found --wait=true } | complete | ignore
    print $"+ kubectl -n ($NS) delete pod ($LOADER)"
    kc -n $NS scale statefulset $STS --replicas=1
    if $flux != null { kc -n $flux.namespace patch kustomization $flux.name --type merge -p '{"spec":{"suspend":false}}' }
    kc -n $NS rollout status $"statefulset/($STS)" --timeout=300s

    if $result != null { error make --unspanned { msg: $"the dump was not loaded: ($result). Neo4j is running again, on the data it had." } }
    print "loaded. The users and the password are the ones Neo4j had: the system database is not part of the load."
}
