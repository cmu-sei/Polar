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
#   nu infra/deploy-repo.nu ensure  <target> [--port N] [--image REF]
#   nu infra/deploy-repo.nu publish <target> [--port N] [--allow-dirty]
#
# `ensure` makes the PKI under ~/.config/polar-deploy/<target>/ if it is not there, and starts the server. It can be
# run again: nothing is made twice, and the container is replaced only when what it should be has changed.
#
# `publish` commits the target's rendered layers, the Flux objects that reconcile them (infra/deploy-objects.nu) and a
# provenance record to the deployment repository, signed with the pipeline's certificate. Nothing in it needs a
# cluster: it runs before one exists, and the signature chains to the deployment CA, never to Polar's. The signing
# uses git's x509 support (gpgsm) in a GnuPG home of its own beside the PKI, and never touches the user's ~/.gnupg or
# asks for a passphrase.
#
# Identities (the DNS name in a client certificate): `pipeline` writes the repository, `flux` reads it.

use runtime.nu [container_runtime, insecure_pull_flags]
use retry.nu [with_retries]
use layers.nu [layer_problems]
use deploy-objects.nu [BOOTSTRAP_LAYERS, annotate_config_hashes, documents, git_repository, has_job, kustomization, provenance, published_layers, readme, unpinned_images, yaml_docs]

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

# ---- publish ---------------------------------------------------------------------------------------------------

export def gnupg_dir [pki: string] { $pki | path join "gnupg" }

# The program git runs in place of gpgsm: the same, but it reads the passphrase from a file instead of asking for it.
# (The key is protected by the 700 directory it lives in; gpgsm insists on a passphrase all the same.)
export def wrapper_text [pass_file: string] {
    $"#!/usr/bin/env bash\nexec 3<'($pass_file)'\nexec gpgsm --pinentry-mode loopback --passphrase-fd 3 \"$@\"\n"
}

# The `git -c ...` arguments that sign a commit with the pipeline's certificate.
export def signing_config [wrapper: string, fingerprint: string, target: string] {
    [
        "-c" "gpg.format=x509"
        "-c" $"gpg.x509.program=($wrapper)"
        "-c" $"user.signingkey=($fingerprint)"
        "-c" $"user.name=($WRITER)"
        "-c" $"user.email=($WRITER)@polar-deploy-($target)"
    ]
}

# `git -c ...` arguments that present a deployment identity's certificate to the server, for one command.
export def tls_config [pki: string, identity: string] {
    [
        "-c" $"http.sslCAInfo=($pki | path join 'ca.pem')"
        "-c" $"http.sslCert=($pki | path join $"($identity).pem")"
        "-c" $"http.sslKey=($pki | path join $"($identity).key")"
    ]
}

# The SHA-1 fingerprint gpgsm names a certificate by.
def fingerprint_of [pem: string] {
    run_ok "reading a fingerprint" { ^openssl x509 -in $pem -noout -fingerprint -sha1 } | str trim | split row "=" | last | str replace --all ":" ""
}

def sha256_of [pem: string] {
    run_ok "reading a fingerprint" { ^openssl x509 -in $pem -noout -fingerprint -sha256 } | str trim | split row "=" | last | str replace --all ":" "" | str lowercase
}

# Makes the GnuPG home that signs: the pipeline's key and certificate, the deployment CA as the only trust. Returns
# the signing key's fingerprint and the program for git.
export def ensure_signing [pki: string] {
    if (which gpgsm | is-empty) {
        error make --unspanned { msg: "publish signs commits with the pipeline's certificate and needs gpgsm (GnuPG) on the PATH" }
    }
    let home = (gnupg_dir $pki)
    mkdir $home
    chmod 700 $home
    "allow-loopback-pinentry\n" | save --force ($home | path join "gpg-agent.conf")
    let pass = ($pki | path join "signing.pass")
    if not ($pass | path exists) {
        run_ok "making a passphrase" { ^openssl rand -hex 16 } | str trim | save --force $pass
        chmod 600 $pass
    }
    let wrapper = ($pki | path join "gpgsm-loopback")
    wrapper_text $pass | save --force $wrapper
    chmod 700 $wrapper
    let fp = (fingerprint_of ($pki | path join $"($WRITER).pem"))
    with-env { GNUPGHOME: $home } {
        # gpgsm exits 0 when nothing matches, so the answer is whether it listed anything.
        let have = (do { ^gpgsm --batch --with-colons --list-secret-keys $fp } | complete)
        if ($have.stdout | str trim | is-empty) {
            let p12 = ($pki | path join "signing.p12")
            run_ok "packaging the signing key" { ^openssl pkcs12 -export -inkey ($pki | path join $"($WRITER).key") -in ($pki | path join $"($WRITER).pem") -certfile ($pki | path join "ca.pem") -out $p12 -passout $"file:($pass)" }
            let imported = (open --raw $pass | do { ^gpgsm --batch --pinentry-mode loopback --passphrase-fd 0 --import $p12 } | complete)
            rm -f $p12
            if $imported.exit_code != 0 { error make --unspanned { msg: $"importing the signing key failed: ($imported.stderr | str trim)" } }
            let ca_fp = (fingerprint_of ($pki | path join "ca.pem"))
            $"($ca_fp) S relax\n" | save --append ($home | path join "trustlist.txt")
            run_ok "importing the CA" { ^gpgsm --batch --import ($pki | path join "ca.pem") } | ignore
        }
    }
    { fingerprint: $fp, wrapper: $wrapper, home: $home }
}

def digest_of [file: string] { open --raw $file | hash sha256 }

# Everything the published tree is made of, as files: path -> text. Pure given the facts.
export def tree_files [target: string, layers: list, texts: record, port: int, facts: record] {
    let published = (published_layers $layers)
    let rendered = $texts
    let texts = (annotate_config_hashes $rendered)
    let layer_files = ($published | each {|l|
        $l.files | where {|f| $f in ($texts | columns) } | each {|f| { path: $"layers/($l.name)/($f)", text: ($texts | get $f) } }
    } | flatten)
    let flux = ($published | each {|l|
        let docs = ($l.files | each {|f| if $f in ($texts | columns) { documents ($texts | get $f) } else { [] } } | flatten)
        let forced = ($l.files | any {|f| ($f in ($texts | columns)) and (has_job ($texts | get $f)) })
        kustomization $l $docs $forced
    })
    let source = (git_repository $target $CLUSTER_NAME $port)
    let recorded = ($facts | insert layers ($published | each {|l|
        { name: $l.name, depends_on: $l.depends_on, files: ($l.files | where {|f| $f in ($texts | columns) } | each {|f| { path: $f, sha256: ($texts | get $f | hash sha256), rendered_sha256: ($rendered | get $f | hash sha256) } }) }
    }))
    $layer_files ++ [
        { path: "flux/gitrepository.yaml", text: (yaml_docs [$source]) }
        { path: "flux/kustomizations.yaml", text: (yaml_docs $flux) }
        { path: "provenance.json", text: ((provenance $recorded) + "\n") }
        { path: "README.md", text: (readme $target) }
    ]
}

def load_config [target: string] {
    let file = ($env.PWD | path join $"infra/targets/($target)/target.nu")
    if not ($file | path exists) { error make --unspanned { msg: $"no such target: ($target)" } }
    ^$nu.current-exe $file | from nuon
}

def git_out [args: list<string>] {
    run_ok $"git ($args | first)" { ^git ...$args } | str trim
}

def "main publish" [
    target: string
    --port: int = 9443          # the port the deployment git server listens on
    --allow-dirty               # publish from a working tree with uncommitted changes (the provenance says so)
] {
    let config = (load_config $target)
    if ($config.apply_layers? | default [] | is-empty) { error make --unspanned { msg: $"the target '($target)' declares no apply_layers" } }
    let rendered = (glob ($config.output_dir | path join "*.yaml") | each {|f| $f | path basename } | sort)
    let problems = (layer_problems $config.apply_layers $rendered)
    if ($problems | is-not-empty) {
        error make --unspanned { msg: $"the render does not match the target's layers (run: nu infra/render.nu ($target)): ($problems | first)" }
    }

    let published = (published_layers $config.apply_layers)
    let texts = ($published | get files | flatten | uniq | reduce --fold {} {|f, acc| $acc | insert $f (open --raw ($config.output_dir | path join $f)) })

    if ($config.pin_images? | default false) {
        let loose = ($texts | items {|f, t| unpinned_images $t | each {|i| $"($f): ($i)" } } | flatten)
        if ($loose | is-not-empty) {
            error make --unspanned { msg: $"the render has images not pinned by digest, and this target pins them: ($loose | first 5 | str join '; ')" }
        }
    }

    let dirty = (git_out [status --porcelain] | is-not-empty)
    if $dirty and not $allow_dirty {
        error make --unspanned { msg: "the working tree has uncommitted changes, so the commit it would be published from does not say what was rendered; commit them, or pass --allow-dirty (the provenance records it)" }
    }
    let pki = (pki_dir $env.HOME $target)
    if not ($pki | path join "ca.pem" | path exists) { error make --unspanned { msg: $"no deployment PKI at ($pki): run: nu infra/deploy-repo.nu ensure ($target)" } }
    let signing = (ensure_signing $pki)

    let lock = ($config.repo_root | path join "infra" "images.lock.nuon")
    let controllers = ($config.output_dir | path join "flux-controllers.yaml")
    let facts = {
        schema: 1
        target: $target
        source: { commit: (git_out [rev-parse HEAD]), describe: (git_out [describe --tags --always]), uncommitted_changes: $dirty }
        published_by: "infra/deploy-repo.nu publish"
        image_lock: (if ($lock | path exists) { { file: "infra/images.lock.nuon", sha256: (digest_of $lock) } } else { null })
        flux_controllers: (if ($controllers | path exists) { { sha256: (digest_of $controllers), bootstrap_layers: $BOOTSTRAP_LAYERS } } else { null })
        deployment_ca_sha256: (sha256_of ($pki | path join "ca.pem"))
        signer: { identity: $WRITER, certificate_sha256: (sha256_of ($pki | path join $"($WRITER).pem")) }
    }
    let files = (tree_files $target $config.apply_layers $texts $port $facts)

    let url = $"https://localhost:($port)/(repo_name $target).git"
    let work = (mktemp -d)
    let copy = ($work | path join "repo")
    let outcome = (try {
        with-env { GNUPGHOME: $signing.home } {
            let tls = (tls_config $pki $WRITER)
            run_ok "cloning the deployment repository (is it running? nu infra/deploy-repo.nu ensure)" { ^env -u GIT_SSL_CAINFO -u SSL_CERT_FILE git ...$tls clone -q $url $copy }
            ls -a $copy | get name | where {|n| ($n | path basename) not-in [".git" "." ".."] } | each {|n| rm -rf $n } | ignore
            for f in $files {
                let dest = ($copy | path join $f.path)
                mkdir ($dest | path dirname)
                $f.text | save --force $dest
            }
            ^git -C $copy add -A
            if (git_out [-C $copy status --porcelain] | is-empty) {
                { changed: false, commit: (try { git_out [-C $copy rev-parse HEAD] } catch { "" }) }
            } else {
                let short = ($facts.source.commit | str substring 0..6)
                let note = if $dirty { ", with uncommitted changes" } else { "" }
                let msg = $"chore\(deploy\): ($target) from ($short)\n\nRendered from ($facts.source.describe) \(commit ($facts.source.commit)\)($note).\nLayers: ($published | get name | str join ', ')."
                let sign = (signing_config $signing.wrapper $signing.fingerprint $target)
                run_ok "committing" { ^git -C $copy ...$sign commit -q -S -m $msg }
                run_ok "verifying the signature" { ^git -C $copy ...$sign verify-commit HEAD }
                run_ok "pushing" { ^env -u GIT_SSL_CAINFO -u SSL_CERT_FILE git -C $copy ...$tls push -q origin HEAD:refs/heads/main }
                { changed: true, commit: (git_out [-C $copy rev-parse HEAD]) }
            }
        }
    } catch {|e| { error: $e.msg } })
    with-env { GNUPGHOME: $signing.home } { do { ^gpgconf --kill gpg-agent } | complete | ignore }
    rm -rf $work
    if ($outcome | get -o error) != null { error make --unspanned { msg: ($outcome | get error) } }

    if $outcome.changed {
        print $"  published ($files | length) files to deploy-($target) as ($outcome.commit | str substring 0..11), signed by ($WRITER) \(($signing.fingerprint | str substring 0..7)\)"
    } else {
        print $"  ok: deploy-($target) already holds this render \(($outcome.commit | str substring 0..11)\)"
    }
}

def main [] {
    print "usage: nu infra/deploy-repo.nu ensure <target> [--port N] [--image REF] | publish <target> [--port N] [--allow-dirty]"
}
