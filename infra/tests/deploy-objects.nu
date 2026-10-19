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

# What goes into a deployment repository besides the manifests: which layers Flux reconciles, how it is told to
# wait for each, and the checks that keep an unpinned image out.

use std/assert
use ../deploy-objects.nu [documents, git_repository, has_job, health_check, kustomization, published_layers, unpinned_images, yaml_docs]

def fails_with [needle: string, body: closure] {
    let r = (try { do $body; "no error" } catch {|e| $e.msg })
    assert ($r | str contains $needle) $"expected an error containing '($needle)', got: ($r)"
}

const JOB = "# a comment\n---\napiVersion: batch/v1\nkind: Job\nmetadata:\n  name: cassini\n  namespace: polar\nspec:\n  template:\n    metadata:\n      labels:\n        app: cassini\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: cassini\n"
const DEPLOY = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: cert-issuer\n  namespace: polar\nspec:\n  template:\n    metadata:\n      labels:\n        name: cert-issuer\n"

def layers [] {
    [
        { name: "platform", depends_on: [], wait_for: [], files: ["a.yaml"] }
        { name: "flux", depends_on: ["platform"], wait_for: [{ kind: "pods", ns: "flux-system", ref: "app=source-controller", label: "s" }], files: ["f.yaml"] }
        { name: "cert-issuer", depends_on: ["flux"], wait_for: [], files: ["c.yaml"] }
        { name: "agents", depends_on: ["cert-issuer" "flux"], wait_for: [], files: ["g.yaml"] }
    ]
}

def main [] {
    let tests = [
        {
            name: "Flux's own layer is not published, and what depended on it depends on what it did"
            body: {
                let p = (published_layers (layers))
                assert equal ($p | get name) ["platform" "cert-issuer" "agents"]
                assert equal ($p | where name == "cert-issuer" | first | get depends_on) ["platform"]
                assert equal ($p | where name == "agents" | first | get depends_on) ["cert-issuer" "platform"]
            }
        }
        {
            name: "a file with several documents is read document by document, comments and empty ones skipped"
            body: {
                let d = (documents $JOB)
                assert equal ($d | length) 2
                assert equal ($d | get kind) ["Job" "Service"]
                assert (has_job $JOB)
                assert (not (has_job $DEPLOY))
            }
        }
        {
            name: "an image not pinned by digest is found, one pinned is not"
            body: {
                let text = "    image: 10.0.2.2:5000/x@sha256:abc\n  - image: \"10.0.2.2:5000/y:latest\"\n        image: z:1\n"
                assert equal (unpinned_images $text) ["10.0.2.2:5000/y:latest" "z:1"]
                assert equal (unpinned_images "image: a@sha256:1\n") []
            }
        }
        {
            name: "a Job's pods are waited for as that Job, a Deployment's by its label, a StatefulSet by its name"
            body: {
                let docs = ((documents $JOB) ++ (documents $DEPLOY))
                let job = (health_check { kind: "pods", ns: "polar", ref: "job-name=cassini", label: "c" } $docs)
                assert equal $job { apiVersion: "batch/v1", kind: "Job", name: "cassini", namespace: "polar" }
                let dep = (health_check { kind: "pods", ns: "polar", ref: "name=cert-issuer", label: "c" } $docs)
                assert equal ($dep | select kind name namespace) { kind: "Deployment", name: "cert-issuer", namespace: "polar" }
                let sts = (health_check { kind: "statefulset", ns: "polar-graph", ref: "statefulset/polar-neo4j", label: "n" } $docs)
                assert equal ($sts | select kind name) { kind: "StatefulSet", name: "polar-neo4j" }
            }
        }
        {
            name: "waiting for something the layer does not make is an error, not a wait that never ends"
            body: {
                let docs = (documents $DEPLOY)
                fails_with "no such Job" { health_check { kind: "pods", ns: "polar", ref: "job-name=nope", label: "n" } $docs }
                fails_with "makes them" { health_check { kind: "pods", ns: "polar", ref: "app=nope", label: "n" } $docs }
            }
        }
        {
            name: "a layer with Jobs is forced and gets a health expression for Jobs; one without is neither"
            body: {
                let layer = { name: "cassini", depends_on: ["data"], wait_for: [{ kind: "pods", ns: "polar", ref: "job-name=cassini", label: "c" }], files: ["c.yaml"] }
                let k = (kustomization $layer (documents $JOB) true)
                assert equal $k.metadata.name "polar-cassini"
                assert equal $k.spec.path "./layers/cassini"
                assert equal $k.spec.dependsOn [{ name: "polar-data" }]
                assert $k.spec.force
                assert equal ($k.spec.healthChecks | first | get kind) "Job"
                assert equal ($k.spec.healthCheckExprs | first | get kind) "Job"
                assert ($k.spec.prune)
                let plain = (kustomization { name: "platform", depends_on: [], wait_for: [], files: ["a.yaml"] } [] false)
                assert (($plain.spec | get -o force) == null)
                assert (($plain.spec | get -o dependsOn) == null)
                assert (($plain.spec | get -o healthChecks) == null)
            }
        }
        {
            name: "the source is the deployment repository by the name on the server's certificate, read with the flux identity's secret"
            body: {
                let g = (git_repository "sandbox-local" "polar-deploy-git.flux-system.svc.cluster.local" 9443)
                assert equal $g.spec.url "https://polar-deploy-git.flux-system.svc.cluster.local:9443/deploy-sandbox-local.git"
                assert equal $g.spec.secretRef.name "deploy-repo-auth"
                assert equal $g.spec.ref.branch "main"
            }
        }
        {
            name: "documents are joined with separators and read back as the same objects"
            body: {
                let docs = [{ a: 1 } { b: [1 2] }]
                let text = (yaml_docs $docs)
                assert equal (documents $text) $docs
                assert ($text | str ends-with "\n")
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
