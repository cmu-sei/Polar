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

# infra/deploy-repo.nu
#
# The deployment git server: where the rendered manifests are committed, signed, for Flux to read. It runs as a
# container on the host, outside the cluster, with a PKI of its own (not Polar's CA), so that what a cluster is
# made from can be reasoned about from before the cluster exists. Design: docs/architecture/cluster-bootstrap.md.
#
#   nu infra/deploy-repo.nu ensure <target> [--port N] [--image REF]
#
# `ensure` makes the PKI under ~/.config/polar-deploy/<target>/ if it is not there, and starts the server. It can be
# run again: nothing is made twice, and the container is replaced only when what it should be has changed.
#
# Identities (the DNS name in a client certificate): `pipeline` writes the repository, `flux` reads it.

use runtime.nu [container_runtime, insecure_pull_flags]
use retry.nu [with_retries]

export const DEFAULT_PORT = 9443
# The name Flux is given. It is a Service in the cluster whose Endpoints are the host, so it needs no host resolution.
export const CLUSTER_NAME = "polar-deploy-git.flux-system.svc.cluster.local"
export const WRITER = "pipeline"
export const READER = "flux"

export def container_name [target: string] { $"polar-deploy-($target)" }
export def repo_name [target: string] { $"deploy-($target)" }
export def volume_name [target: string] { $"polar-deploy-($target)-repos" }

export def pki_dir [home: string, target: string] {
    $home | path join ".config" "polar-deploy" $target
}

# What the server mounts: the CA certificate, its own certificate and its key. The CA's key is never in it.
export def serve_dir [pki: string] { $pki | path join "serve" }

# The certificates the PKI holds. The server answers to the cluster's name and to localhost (the host's own git).
export def cert_plan [] {
    [
        { name: "server", dns: [$CLUSTER_NAME "localhost"], usage: "serverAuth" }
        { name: $WRITER, dns: [$WRITER], usage: "clientAuth" }
        { name: $READER, dns: [$READER], usage: "clientAuth" }
    ]
}

# `POLAR_GIT_SERVER_REPOS`: one repository, written by the pipeline and read by Flux and the pipeline.
export def repos_json [target: string] {
    { admins: [$WRITER], repos: [{ name: (repo_name $target), readers: [$READER $WRITER], writers: [$WRITER] }] }
    | to json --raw
}

# The server's environment inside the container. The health certificate is the server's own: the image's default
# path does not exist on a host, and without it the server judges itself unhealthy and exits after 30 seconds.
export def server_env [target: string] {
    {
        POLAR_GIT_SERVER_CERT: "/pki/server.pem"
        POLAR_GIT_SERVER_KEY: "/pki/server.key"
        POLAR_GIT_SERVER_CA_CERT: "/pki/ca.pem"
        POLAR_GIT_SERVER_REPO_ROOT: "/repos"
        POLAR_GIT_SERVER_REPOS: (repos_json $target)
        POLAR_HEALTH_CERTS: "/pki/server.pem"
        RUST_LOG: "info"
    }
}

# The arguments of `<runtime> run` for the server. `image` should be a digest reference, so what runs is what was
# looked at. `--restart=always` because the server exits, by design, when its certificate is near expiry.
export def run_args [target: string, port: int, image: string, serve: string, pull_flags: list<string>] {
    let env_flags = (server_env $target | items {|k, v| ["-e" $"($k)=($v)"] } | flatten)
    [
        "run" "-d" "--name" (container_name $target) "--replace" "--restart=always"
        "-p" $"($port):8443"
        "-v" $"($serve):/pki:ro"
        "-v" $"(volume_name $target):/repos"
        "--label" $"polar.deploy.target=($target)"
        ...$env_flags
        ...$pull_flags
        $image
    ]
}

# A fingerprint of what the container should be, kept on it as a label, so `ensure` can tell it is already right.
export def spec_of [args: list<string>] {
    $args | to json --raw | hash sha256
}

# `args` with the fingerprint as a label, put before the image (the last argument).
export def labelled [args: list<string>, spec: string] {
    ($args | drop 1) ++ ["--label" $"polar.deploy.spec=($spec)"] ++ [($args | last)]
}

# The openssl extension text for one certificate.
export def ext_text [cert: record] {
    let sans = ($cert.dns | each {|d| $"DNS:($d)" } | str join ",")
    $"subjectAltName=($sans)\nextendedKeyUsage=($cert.usage)\n"
}

def run_ok [what: string, cmd: closure] {
    let r = (do $cmd | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"($what) failed: ($r.stderr | str trim)" } }
    $r.stdout
}

# Makes the CA and the certificates that are not there yet. The CA key stays in `pki`; only `serve/` is mounted.
export def ensure_pki [pki: string, target: string] {
    mkdir $pki
    chmod 700 $pki
    let serve = (serve_dir $pki)
    mkdir $serve
    let ca_key = ($pki | path join "ca.key")
    let ca_pem = ($pki | path join "ca.pem")
    mut made = []
    if not ($ca_pem | path exists) {
        run_ok "making the CA" { ^openssl req -x509 -newkey rsa:3072 -nodes -days 3650 -subj $"/CN=polar-deploy-($target)-ca" -keyout $ca_key -out $ca_pem }
        chmod 600 $ca_key
        $made = ($made | append "ca")
    }
    for c in (cert_plan) {
        let pem = ($pki | path join $"($c.name).pem")
        let key = ($pki | path join $"($c.name).key")
        if ($pem | path exists) and ($key | path exists) { continue }
        let csr = ($pki | path join $"($c.name).csr")
        let ext = ($pki | path join $"($c.name).ext")
        ext_text $c | save --force $ext
        run_ok $"making ($c.name)'s key" { ^openssl req -newkey rsa:3072 -nodes -subj $"/CN=($c.name)" -keyout $key -out $csr }
        run_ok $"signing ($c.name)'s certificate" { ^openssl x509 -req -in $csr -CA $ca_pem -CAkey $ca_key -CAcreateserial -days 365 -extfile $ext -out $pem }
        rm -f $csr $ext
        chmod 600 $key
        $made = ($made | append $c.name)
    }
    # The container runs as another user than the one who made these, so what it reads is readable to it. The directory
    # above is 700, so nobody else on this machine can get to them.
    cp $ca_pem ($serve | path join "ca.pem")
    cp ($pki | path join "server.pem") ($serve | path join "server.pem")
    cp ($pki | path join "server.key") ($serve | path join "server.key")
    chmod 644 ($serve | path join "server.key")
    chmod 755 $serve
    $made
}

# The digest the registry serves for the server's image, as a reference to run.
def image_by_digest [ref: string] {
    let digest = (run_ok $"looking up ($ref)" { ^skopeo inspect --tls-verify=false --format "{{.Digest}}" $"docker://($ref)" } | str trim)
    let repo = ($ref | split row ":" | drop 1 | str join ":")
    $"($repo)@($digest)"
}

def container_spec [runtime: string, name: string] {
    let r = (do { ^$runtime inspect $name --format '{{ index .Config.Labels "polar.deploy.spec" }} {{.State.Running}}' } | complete)
    if $r.exit_code != 0 { return null }
    let parts = ($r.stdout | str trim | split row " ")
    { spec: ($parts | first), running: (($parts | last) == "true") }
}

def "main ensure" [
    target: string
    --port: int = 9443          # the host port the server listens on
    --image: string = ""        # the server's image (default: polar-git-server:latest in the node registry)
] {
    let runtime = (container_runtime)
    let pki = (pki_dir $env.HOME $target)
    let made = (ensure_pki $pki $target)
    if ($made | is-empty) { print $"  ok: the deployment PKI is in ($pki)" } else { print $"  made in ($pki): ($made | str join ', ')" }

    let registry = ($env.POLAR_REGISTRY? | default "localhost:5000")
    let ref = if ($image | is-empty) { $"($registry)/polar-git-server:latest" } else { $image }
    let pinned = (image_by_digest $ref)
    let pull = (insecure_pull_flags $runtime ["--tls-verify=false"])
    let args = (run_args $target $port $pinned (serve_dir $pki) $pull)
    let spec = (spec_of $args)
    let name = (container_name $target)

    let have = (container_spec $runtime $name)
    if $have != null and $have.running and $have.spec == $spec {
        print $"  ok: ($name) is running ($pinned)"
    } else {
        let with_label = (labelled $args $spec)
        run_ok $"starting ($name)" { ^$runtime ...$with_label }
        print $"  started ($name) from ($pinned) on port ($port)"
    }

    # Up means it answers the pipeline's certificate over TLS with its own name checked.
    let url = $"https://localhost:($port)/(repo_name $target).git/info/refs?service=git-upload-pack"
    let up = (with_retries 15 2sec {|n|
        let r = (do { ^curl -s -m 5 --cacert ($pki | path join "ca.pem") --cert ($pki | path join $"($WRITER).pem") --key ($pki | path join $"($WRITER).key") -o /dev/null -w "%{http_code}" $url } | complete)
        { ok: ($r.stdout == "200"), final: false, detail: $"answered ($r.stdout) ($r.stderr | str trim)" }
    })
    if not $up.ok { error make --unspanned { msg: $"($name) did not answer after ($up.tries) tries: ($up.detail). See: ($runtime) logs ($name)" } }
    print $"  ok: ($name) answers at https://localhost:($port)/ as ($WRITER)"
}

def main [] {
    print "usage: nu infra/deploy-repo.nu ensure <target> [--port N] [--image REF]"
}
