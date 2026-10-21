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

# Whether Neo4j accepts a client certificate carrying a workload's floor (docs/architecture/policy-enforcement.md, stage 3
# and open point C): its TLS stack is the JDK's, whose default limit on one handshake message (32 KiB) is the tightest in
# Polar's path. A Neo4j container requires client certificates on Bolt, and `openssl s_client` presents ones with more
# and more topic names; the handshake succeeding is Neo4j accepting the certificate. Needs podman and the neo4j:5 image,
# and skips without them. Takes about a minute.

use std/assert

const IMAGE = "docker.io/library/neo4j:5"

# A certificate with `names` URI names (topics, each about 57 bytes in DER) besides its identity, signed by the CA in
# `dir`, written to <dir>/<file>.pem and .key.
def issue [dir: string, file: string, identity: string, names: int, usage: string] {
    let sans = ([$"DNS:($identity)"] ++ (0..<$names | each {|i| $"URI:p:pub:polar.example.a-rather-long-topic-name-($i | fill --alignment right --character '0' --width 4).events" }) | str join ",")
    let ext = ($dir | path join $"($file).ext")
    $"subjectAltName=($sans)\nextendedKeyUsage=($usage)\n" | save --force $ext
    ^openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -keyout ($dir | path join $"($file).key") -subj $"/CN=($identity)" -out ($dir | path join $"($file).csr") err> /dev/null
    ^openssl x509 -req -in ($dir | path join $"($file).csr") -CA ($dir | path join "ca.pem") -CAkey ($dir | path join "ca.key") -CAcreateserial -days 1 -extfile $ext -out ($dir | path join $"($file).pem") err> /dev/null
    ^openssl x509 -in ($dir | path join $"($file).pem") -outform DER | bytes length
}

# Whether a TLS handshake with Neo4j's Bolt port, presenting <dir>/<file>.pem, succeeds. TLS 1.2, where the server
# judges the client's certificate inside the handshake, so a refusal fails it (in 1.3 it comes after, and a client
# that closes at once may never read it).
def handshake [dir: string, port: int, file: string] {
    let r = (do { "" | ^openssl s_client -tls1_2 -connect $"127.0.0.1:($port)" -servername localhost -CAfile ($dir | path join "ca.pem") -cert ($dir | path join $"($file).pem") -key ($dir | path join $"($file).key") -verify_return_error -brief } | complete)
    ($r.exit_code == 0) and not (($r.stdout + $r.stderr) =~ '(?i)alert|error')
}

def main [] {
    if (which podman | is-empty) or (which openssl | is-empty) {
        print "skipped: needs podman and openssl"
        return
    }
    if (do { ^podman image exists $IMAGE } | complete).exit_code != 0 {
        print $"skipped: ($IMAGE) is not pulled \(podman pull ($IMAGE)\)"
        return
    }
    let dir = (mktemp --directory)
    ^openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -keyout ($dir | path join "ca.key") -subj "/CN=test ca" -days 1 -out ($dir | path join "ca.pem") err> /dev/null
    issue $dir "server" "localhost" 0 "serverAuth" | ignore
    let bolt = ($dir | path join "bolt")
    mkdir ($bolt | path join "trusted") ($bolt | path join "revoked")
    cp ($dir | path join "server.pem") ($bolt | path join "public.crt")
    ^openssl pkcs8 -topk8 -nocrypt -in ($dir | path join "server.key") -out ($bolt | path join "private.key")
    cp ($dir | path join "ca.pem") ($bolt | path join "trusted" "ca.crt")
    ^chmod -R a+rX $bolt
    # A certificate from a CA Neo4j does not trust, which it must refuse: what shows the check can fail.
    let other = ($dir | path join "other")
    mkdir $other
    ^openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -keyout ($other | path join "ca.key") -subj "/CN=another ca" -days 1 -out ($other | path join "ca.pem") err> /dev/null
    issue $other "stranger" "stranger.polar.serviceaccount.cluster.local" 0 "clientAuth" | ignore
    cp ($other | path join "stranger.pem") ($dir | path join "stranger.pem")
    cp ($other | path join "stranger.key") ($dir | path join "stranger.key")
    let sizes = { small: (issue $dir "small" "kube-consumer.polar.serviceaccount.cluster.local" 34 "clientAuth"), hundreds: (issue $dir "hundreds" "big.polar.serviceaccount.cluster.local" 300 "clientAuth"), huge: (issue $dir "huge" "huge.polar.serviceaccount.cluster.local" 1050 "clientAuth") }
    let name = $"polar-cert-size-(random chars --length 6 | str lowercase)"
    let started = (do { ^podman run -d --rm --name $name --log-driver k8s-file -p 127.0.0.1::7687 -v $"($bolt):/ssl/bolt:ro,Z" -e NEO4J_AUTH=none -e NEO4J_dbms_ssl_policy_bolt_enabled=true -e NEO4J_dbms_ssl_policy_bolt_base__directory=/ssl/bolt -e NEO4J_dbms_ssl_policy_bolt_private__key=private.key -e NEO4J_dbms_ssl_policy_bolt_public__certificate=public.crt -e NEO4J_dbms_ssl_policy_bolt_client__auth=REQUIRE -e NEO4J_server_bolt_tls__level=REQUIRED $IMAGE } | complete)
    if $started.exit_code != 0 {
        rm -rf $dir
        error make --unspanned { msg: $"Neo4j did not start: ($started.stderr | str trim)" }
    }
    let results = (try {
        let deadline = ((date now) + 120sec)
        while not ((^podman logs $name | complete).stdout =~ "Bolt enabled") {
            if (date now) > $deadline { error make --unspanned { msg: "Neo4j did not enable Bolt within two minutes" } }
            sleep 1sec
        }
        let port = (^podman port $name 7687 | str trim | split row ":" | last | into int)
        {
            stranger: (handshake $dir $port "stranger")
            small: (handshake $dir $port "small")
            hundreds: (handshake $dir $port "hundreds")
            huge: (handshake $dir $port "huge")
        }
    } catch {|e| { error: $e.msg } })
    ^podman rm -f $name | ignore
    rm -rf $dir

    if ($results.error? != null) { print -e $"  FAIL  ($results.error)"; exit 1 }
    print $"  the largest real set, 34 names: ($sizes.small) bytes, accepted: ($results.small)"
    print $"  300 names: ($sizes.hundreds) bytes, accepted: ($results.hundreds)"
    print $"  1050 names: ($sizes.huge) bytes, accepted: ($results.huge) \(rustls' own limit is near here\)"
    let tests = [
        { name: "Neo4j refuses a client certificate from a CA it does not trust (the check can fail)", ok: (not $results.stranger) }
        { name: "Neo4j accepts a client certificate with the largest real set of topic names", ok: $results.small }
        { name: "Neo4j accepts a client certificate with a few hundred topic names", ok: $results.hundreds }
    ]
    for t in $tests { if $t.ok { print $"  ok    ($t.name)" } else { print -e $"  FAIL  ($t.name)" } }
    let failed = ($tests | where ok == false | length)
    if $failed > 0 { print -e $"($failed) of ($tests | length) failed"; exit 1 }
    print $"all ($tests | length) passed"
}
