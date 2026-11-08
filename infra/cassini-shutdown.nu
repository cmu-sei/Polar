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

# infra/cassini-shutdown.nu
#
# Asks the Cassini broker in the cluster to shut down in order, over the wire, the way an operator
# would: with the shutdown token, as a client with a certificate the Polar CA signed. The token is
# the Secret the secrets step made (`cassini-shutdown-token`), not a constant. The broker then runs
# its five phases and exits, and its pod restarts in place, polar-init with it (docs/architecture/deployments.md, step 4).
#
#   nu infra/cassini-shutdown.nu                 # as the admin in ~/.config/polar-admin
#   nu infra/cassini-shutdown.nu --user alice
#   just cassini-shutdown
#
# It reaches the broker through a port-forward it starts and stops itself, and runs cassini-c2 with
# the environment `shutdown_env` builds.

const INFRA = (path self | path dirname)

# The name on the broker's certificate: what the client verifies it against.
export def cassini_server_name [services: record, cluster: record] {
    let s = ($services | get cassini)
    let namespace = ($cluster.namespaces | get $s.namespace)
    $"($s.dns_name).($namespace).svc.($cluster.domain)"
}

# What cassini-c2 reads to connect and to ask: the user's certificate and key and the CA that signed
# the broker's, where to find the broker (a local forward) and what to verify its certificate
# against, and the token.
export def shutdown_env [token: string, admin_dir: string, user: string, local_port: int, server_name: string] {
    {
        TLS_CLIENT_CERT: ($admin_dir | path join $"($user).crt")
        TLS_CLIENT_KEY: ($admin_dir | path join $"($user).key")
        TLS_CA_CERT: ($admin_dir | path join "ca.pem")
        BROKER_ADDR: $"127.0.0.1:($local_port)"
        CASSINI_SERVER_NAME: $server_name
        CASSINI_SHUTDOWN_TOKEN: $token
    }
}

use tools.nu

def main [
    --user: string = ""       # whose certificate to sign in with; the current user by default
    --token: string = ""      # use this instead of the cluster's Secret (to try a wrong one)
    --port: int = 18080       # the local end of the forward
    --namespace: string = "polar"
] {
    let user = (if ($user | is-empty) { $env.USER? | default "" } else { $user })
    let admin_dir = ($env.HOME | path join ".config/polar-admin")
    for f in [$"($user).crt" $"($user).key" "ca.pem"] {
        if not ($admin_dir | path join $f | path exists) {
            error make { msg: $"($admin_dir)/($f) is not there: run `just user-cert-install ($user)` or `just admin-claim` first" }
        }
    }

    let secret = if ($token | is-not-empty) { $token } else {
        let r = (^kubectl -n $namespace get secret cassini-shutdown-token -o jsonpath='{.data.token}' | complete)
        if $r.exit_code != 0 { error make { msg: $"cannot read the shutdown token: ($r.stderr | str trim)" } }
        $r.stdout | decode base64 | decode utf-8
    }
    let name = (cassini_server_name (open ($INFRA | path join "config/services.nuon")) (open ($INFRA | path join "config/cluster.nuon")))
    let env_vars = (shutdown_env $secret $admin_dir $user $port $name)

    let pid = (^bash -c $"kubectl -n ($namespace) port-forward svc/cassini-ip-svc ($port):8080 >/dev/null 2>&1 & echo $!" | str trim | into int)
    try {
        mut up = false
        for _ in 1..40 {
            if (do { ^bash -c $"exec 3<>/dev/tcp/127.0.0.1/($port)" } | complete | get exit_code) == 0 { $up = true; break }
            sleep 250ms
        }
        if not $up { error make { msg: $"the port-forward to the broker never came up on ($port)" } }
        print $"  asking the broker to shut down, as ($user)"
        let ran = (do {
            let t = (tools command "cassini-c2")
            with-env $env_vars { run-external ($t | first) ...($t | skip 1) } | complete
        })
        # cargo's own warnings are not the tool's answer
        print ($ran | get stderr | lines | where {|l| not ($l =~ '^(warning|help|\s+(-->|\||=)|\s*[0-9]+ \||\s*$)') } | str join "\n")
        ^kill $pid
        if $ran.exit_code != 0 { error make { msg: "the shutdown tool failed" } }
    } catch {|e|
        ^kill $pid
        error make { msg: $e.msg }
    }
}
