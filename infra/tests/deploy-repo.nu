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

# The deployment git server's pure decisions: its PKI, its repository, its container arguments.

use std/assert
use ../deploy-repo.nu [cluster_inputs_fact, cert_plan, ext_text, mirror_dir, needs_reissue, pki_dir, repo_name, repos_json, sans_in, serve_dir, server_env, server_manifests, server_name, signing_config, tls_config, tree_files, wrapper_text, DEFAULT_PORT, NAMESPACE, READER, SERVICE, WRITER]

def main [] {
    let tests = [
        {
            name: "the PKI lives under the user's polar-deploy directory, one per target"
            body: {
                assert equal (pki_dir "/home/a" "sandbox-local") "/home/a/.config/polar-deploy/sandbox-local"
                assert equal (serve_dir "/p") "/p/serve"
            }
        }
        {
            name: "the server answers to its Service's name and to localhost; the pipeline writes and Flux reads, each by its own name"
            body: {
                let plan = (cert_plan (server_name "cluster.local"))
                let server = ($plan | where name == "server" | first)
                assert equal $server.dns ["polar-deploy-git.polar-deploy.svc.cluster.local" "localhost"]
                assert equal $server.usage "serverAuth"
                assert equal ($plan | where name == $WRITER | first | get dns) [$WRITER]
                assert equal ($plan | where name == $READER | first | get usage) "clientAuth"
                assert equal ($plan | get name | uniq | length) ($plan | length)
            }
        }
        {
            name: "the extension text names every DNS entry and the key usage"
            body: {
                let t = (ext_text { name: "server", dns: ["a.b" "localhost"], usage: "serverAuth" })
                assert ($t | str contains "subjectAltName=DNS:a.b,DNS:localhost")
                assert ($t | str contains "extendedKeyUsage=serverAuth")
            }
        }
        {
            name: "only the pipeline writes the repository, and Flux may read it but not write"
            body: {
                let j = (repos_json "t" | from json)
                assert equal $j.admins [$WRITER]
                let repo = ($j.repos | first)
                assert equal $repo.name "deploy-t"
                assert equal $repo.writers [$WRITER]
                assert ($READER in $repo.readers)
                assert ($READER not-in $repo.writers)
            }
        }
        {
            name: "the server judges its health by its own certificate, which the image's default path is not"
            body: {
                let e = (server_env "t")
                assert equal $e.POLAR_HEALTH_CERTS $e.POLAR_GIT_SERVER_CERT
            }
        }
        {
            name: "the server's name is its Service, in the cluster's own DNS domain"
            body: {
                assert equal (server_name "cluster.local") $"($SERVICE).($NAMESPACE).svc.cluster.local"
                assert equal (server_name "k8s.example.org") "polar-deploy-git.polar-deploy.svc.k8s.example.org"
            }
        }
        {
            name: "the server runs in its own namespace, on a claim, restarted when it exits, from the image it was given, and reached by its Service"
            body: {
                let m = (server_manifests "t" "reg/polar-git-server@sha256:abc" { class: "managed-csi", size: "1Gi" } "fp")
                assert equal ($m | each {|o| $o.metadata.namespace? | default $o.metadata.name } | uniq) [$NAMESPACE]
                let d = ($m | where kind == "Deployment" | first)
                let pod = $d.spec.template.spec
                let c = ($pod.containers | first)
                assert equal $c.image "reg/polar-git-server@sha256:abc"
                assert equal $d.spec.strategy.type "Recreate" "the repositories' claim is ReadWriteOnce"
                assert equal $pod.automountServiceAccountToken false
                assert equal ($c.volumeMounts | where mountPath == "/pki" | first | get readOnly) true
                assert ($c.volumeMounts | any {|v| $v.mountPath == "/tmp" }) "the health file is written under /tmp, or the server exits after about 90 seconds"
                let given = ($c.env | reduce --fold {} {|e, acc| $acc | insert $e.name $e.value })
                assert equal $given (server_env "t")
                assert equal ($d.spec.template.metadata.annotations | get "polar.dev/pki-sha256") "fp" "a re-issued certificate restarts the pod"
                let svc = ($m | where kind == "Service" | first)
                assert equal $svc.metadata.name $SERVICE
                assert equal $svc.spec.selector $d.spec.selector.matchLabels
                assert equal ($svc.spec.ports | first | get port) $DEFAULT_PORT
                assert equal ($svc.spec.ports | first | get targetPort) 8443
                let pvc = ($m | where kind == "PersistentVolumeClaim" | first)
                assert equal $pvc.spec.storageClassName "managed-csi"
                assert equal ($pod.volumes | where name == "repos" | first | get persistentVolumeClaim.claimName) $pvc.metadata.name
                assert (not ($m | to json | str contains "ca.key")) "the CA key never goes to the cluster"
                assert (not ($m | any {|o| $o.kind == "Secret" })) "the Secret is made from the files, never written here"
            }
        }
        {
            name: "a certificate is made again only when the names it carries are not the ones it should, whatever their order"
            body: {
                assert equal (sans_in "X509v3 Subject Alternative Name: \n    DNS:polar-deploy-git.polar-deploy.svc.cluster.local, DNS:localhost\n") ["polar-deploy-git.polar-deploy.svc.cluster.local" "localhost"]
                assert equal (needs_reissue ["a" "localhost"] ["localhost" "a"]) false
                assert equal (needs_reissue ["polar-deploy-git.flux-system.svc.cluster.local" "localhost"] ["polar-deploy-git.polar-deploy.svc.cluster.local" "localhost"]) true
                assert equal (needs_reissue [] ["pipeline"]) true
            }
        }
        {
            name: "the mirror of the signed history is kept beside the PKI, off the cluster"
            body: {
                assert equal (mirror_dir "/h/.config/polar-deploy/t") "/h/.config/polar-deploy/t/mirror.git"
            }
        }
        {
            name: "commits are signed as the pipeline with the deployment certificate's fingerprint, through a program that does not ask for a passphrase"
            body: {
                let c = (signing_config "/p/wrap" "ABCD" "t" | str join " ")
                assert ($c | str contains "gpg.format=x509")
                assert ($c | str contains "gpg.x509.program=/p/wrap")
                assert ($c | str contains "user.signingkey=ABCD")
                assert ($c | str contains $"user.name=($WRITER)")
                let w = (wrapper_text "/p/signing.pass")
                assert ($w | str contains "--pinentry-mode loopback")
                assert ($w | str contains "/p/signing.pass")
                assert ($w | str starts-with "#!")
            }
        }
        {
            name: "the server is addressed with the identity's own certificate and the deployment CA, never the user's configuration"
            body: {
                let a = (tls_config "/pki" "flux" | str join " ")
                assert ($a | str contains "http.sslCert=/pki/flux.pem")
                assert ($a | str contains "http.sslKey=/pki/flux.key")
                assert ($a | str contains "http.sslCAInfo=/pki/ca.pem")
            }
        }
        {
            name: "the published tree has each layer's files under it, the Flux objects, the provenance and the README, and not the bootstrap layer"
            body: {
                let layers = [
                    { name: "platform", depends_on: [], wait_for: [], files: ["ns.yaml"] }
                    { name: "flux", depends_on: ["platform"], wait_for: [], files: ["flux.yaml"] }
                    { name: "agents", depends_on: ["flux"], wait_for: [], files: ["job.yaml"] }
                ]
                let texts = { "ns.yaml": "kind: Namespace\nmetadata:\n  name: polar\n", "job.yaml": "kind: Job\nmetadata:\n  name: a\n", "flux.yaml": "kind: Deployment\nmetadata:\n  name: f\n" }
                let files = (tree_files "t" $layers $texts (server_name "cluster.local") { schema: 1, target: "t" })
                let paths = ($files | get path)
                assert ("layers/platform/ns.yaml" in $paths)
                assert ("layers/agents/job.yaml" in $paths)
                assert (not ("layers/flux/flux.yaml" in $paths))
                for need in ["flux/gitrepository.yaml" "flux/kustomizations.yaml" "provenance.json" "README.md"] { assert ($need in $paths) }
                let prov = ($files | where path == "provenance.json" | first | get text | from json)
                assert equal ($prov.layers | get name) ["platform" "agents"]
                assert equal ($prov.layers | first | get files | first | get sha256) ("kind: Namespace\nmetadata:\n  name: polar\n" | hash sha256)
                let source = ($files | where path == "flux/gitrepository.yaml" | first | get text | from yaml)
                assert equal $source.spec.url "https://polar-deploy-git.polar-deploy.svc.cluster.local:9443/deploy-t.git" "Flux reaches the server by its Service"
                let k = ($files | where path == "flux/kustomizations.yaml" | first | get text)
                assert ($k | str contains "force: true")
            }
        }
        {
            name: "the provenance names the cluster's inputs file, its digest, the cluster and the values; with no file it says none"
            body: {
                let dir = (mktemp -d)
                mkdir ($dir | path join "targets" "t")
                let file = ($dir | path join "targets" "t" "cluster-inputs.nuon")
                { cluster: "ca-A", workloads: { "polar-git-server": { env: [{ name: "POLAR_GIT_SERVER_ADMINS", value: "id-1" }] } } } | to nuon | save $file
                let fact = (cluster_inputs_fact { infra_root: $dir } "t")
                assert equal $fact.file "infra/targets/t/cluster-inputs.nuon"
                assert equal $fact.cluster_ca_sha256 "ca-A"
                assert equal $fact.sha256 (open --raw $file | hash sha256)
                assert equal ($fact.values | get "polar-git-server" | get env | first | get value) "id-1"
                rm $file
                assert equal (cluster_inputs_fact { infra_root: $dir } "t") null
                rm -rf $dir
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
