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
use ../deploy-repo.nu [cert_plan, container_name, ext_text, labelled, pki_dir, repo_name, repos_json, run_args, serve_dir, server_env, spec_of, CLUSTER_NAME, READER, WRITER]

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
            name: "the server answers to the cluster's name and to localhost; the pipeline writes and Flux reads, each by its own name"
            body: {
                let plan = (cert_plan)
                let server = ($plan | where name == "server" | first)
                assert equal $server.dns [$CLUSTER_NAME "localhost"]
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
            name: "the container restarts itself, mounts only the serve directory read-only, and runs the image it was given last"
            body: {
                let args = (run_args "t" 9443 "reg/polar-git-server@sha256:abc" "/p/serve" ["--tls-verify=false"])
                assert ("--restart=always" in $args)
                assert ("/p/serve:/pki:ro" in $args)
                assert ("9443:8443" in $args)
                assert equal ($args | last) "reg/polar-git-server@sha256:abc"
                assert equal (container_name "t") "polar-deploy-t"
                assert (not ($args | any {|a| $a | str contains "ca.key" }))
            }
        }
        {
            name: "the fingerprint changes with the image, the port and the target, and the label goes before the image"
            body: {
                let a = (run_args "t" 9443 "img@sha256:1" "/p" [])
                assert equal (spec_of $a) (spec_of (run_args "t" 9443 "img@sha256:1" "/p" []))
                assert ((spec_of $a) != (spec_of (run_args "t" 9443 "img@sha256:2" "/p" [])))
                assert ((spec_of $a) != (spec_of (run_args "t" 9444 "img@sha256:1" "/p" [])))
                assert ((spec_of $a) != (spec_of (run_args "u" 9443 "img@sha256:1" "/p" [])))
                let l = (labelled $a "fp")
                assert equal ($l | last) "img@sha256:1"
                assert equal ($l | get (($l | length) - 3)) "--label"
                assert equal ($l | get (($l | length) - 2)) "polar.deploy.spec=fp"
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
