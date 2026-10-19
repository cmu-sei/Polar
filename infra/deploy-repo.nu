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
# The deployment git server: where the rendered manifests are committed, signed, for Flux to read. It runs in the
# target's cluster, in a namespace of its own (`polar-deploy`) that no layer holds, so Flux neither applies nor prunes it,
# with a PKI of its own (not Polar's CA), so that what Polar is made from can be reasoned about from before Polar exists.
# The PKI's CA key never leaves this machine: the cluster holds only the server's certificate and key and the CA
# certificate. Design: docs/architecture/cluster-bootstrap.md.
#
#   nu infra/deploy-repo.nu ensure  <target> [--port N] [--image REF]
#   nu infra/deploy-repo.nu publish <target> [--port N] [--allow-dirty]
#
# `ensure` makes the PKI under ~/.config/polar-deploy/<target>/ if it is not there, applies the server, and starts the
# port-forward this machine reaches it through (`just port-forwards-up deploy`, to localhost:<port>). Flux reaches it by
# its Service. It can be run again: nothing is made twice. A server whose repository is empty (a new cluster) is given
# the history of the mirror `publish` keeps beside the PKI, so a cluster that is gone does not take the signed record
# with it.
#
# `publish` commits the target's rendered layers, the Flux objects that reconcile them (infra/deploy-objects.nu) and a
# provenance record to the deployment repository, signed with the pipeline's certificate. Nothing in it needs a
# cluster: it runs before one exists, and the signature chains to the deployment CA, never to Polar's. The signing
# uses git's x509 support (gpgsm) in a GnuPG home of its own beside the PKI, and never touches the user's ~/.gnupg or
# asks for a passphrase.
#
# Identities (the DNS name in a client certificate): `pipeline` writes the repository, `flux` reads it.

use retry.nu [with_retries]
use registry.nu [node_registry, push_registry]
use digest.nu [registry_digest]
use cluster-registry.nu [cluster_of]
use layers.nu [layer_problems]
use cluster-inputs.nu
use deploy-objects.nu [BOOTSTRAP_LAYERS, annotate_config_hashes, documents, git_repository, has_job, kustomization, provenance, published_layers, readme, unpinned_images, yaml_docs]

# The port of the server's Service, and of this machine's end of the forward to it unless --port says otherwise.
export const DEFAULT_PORT = 9443
export const NAMESPACE = "polar-deploy"
export const SERVICE = "polar-deploy-git"
export const WRITER = "pipeline"
export const READER = "flux"

# The name Flux is given, and the server's certificate carries: its Service, in the cluster's DNS domain.
export def server_name [domain: string] { $"($SERVICE).($NAMESPACE).svc.($domain)" }

export def repo_name [target: string] { $"deploy-($target)" }

export def pki_dir [home: string, target: string] {
    $home | path join ".config" "polar-deploy" $target
}

# What the server is given: the CA certificate, its own certificate and its key. The CA's key is never in it.
export def serve_dir [pki: string] { $pki | path join "serve" }

# The bare repository `publish` keeps beside the PKI: the signed history, off the cluster.
export def mirror_dir [pki: string] { $pki | path join "mirror.git" }

# The certificates the PKI holds. The server answers to its name in the cluster (Flux) and to localhost (this machine, through
# the forward).
export def cert_plan [server: string] {
    [
        { name: "server", dns: [$server "localhost"], usage: "serverAuth" }
        { name: $WRITER, dns: [$WRITER], usage: "clientAuth" }
        { name: $READER, dns: [$READER], usage: "clientAuth" }
    ]
}

# `POLAR_GIT_SERVER_REPOS`: one repository, written by the pipeline and read by Flux and the pipeline.
export def repos_json [target: string] {
    { admins: [$WRITER], repos: [{ name: (repo_name $target), readers: [$READER $WRITER], writers: [$WRITER] }] }
    | to json --raw
}

# The server's environment. The health certificate is the server's own: without it the server judges itself unhealthy and
# exits after 30 seconds.
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

# The server, for the pod to run: its namespace, the claim its repositories live on, the Deployment and the Service Flux reaches it
# by. `image` is a digest reference, so what runs is what was looked up; `pki_sha256` is a digest of what the PKI Secret holds, in
# the pod template, so a re-issued certificate restarts the pod. The Secret itself is made from the files (`ensure`), never here.
# Restarted when it exits, as it does, by design, when its certificate is near expiry.
export def server_manifests [target: string, image: string, storage: record, pki_sha256: string] {
    let labels = { app: $SERVICE }
    [
        { apiVersion: "v1", kind: "Namespace", metadata: { name: $NAMESPACE } }
        {
            apiVersion: "v1", kind: "PersistentVolumeClaim"
            metadata: { name: $"($SERVICE)-repos", namespace: $NAMESPACE }
            spec: { accessModes: ["ReadWriteOnce"], storageClassName: $storage.class, resources: { requests: { storage: $storage.size } } }
        }
        {
            apiVersion: "apps/v1", kind: "Deployment"
            metadata: { name: $SERVICE, namespace: $NAMESPACE, labels: $labels }
            spec: {
                replicas: 1
                strategy: { type: "Recreate" }
                selector: { matchLabels: $labels }
                template: {
                    metadata: { labels: $labels, annotations: { "polar.dev/pki-sha256": $pki_sha256 } }
                    spec: {
                        automountServiceAccountToken: false
                        securityContext: { runAsNonRoot: true, runAsUser: 1000, runAsGroup: 1000, fsGroup: 1000, seccompProfile: { type: "RuntimeDefault" } }
                        containers: [{
                            name: "server"
                            image: $image
                            ports: [{ containerPort: 8443 }]
                            env: (server_env $target | items {|k, v| { name: $k, value: $v } })
                            readinessProbe: { tcpSocket: { port: 8443 }, periodSeconds: 5 }
                            securityContext: { allowPrivilegeEscalation: false, capabilities: { drop: ["ALL"] } }
                            # The health actor writes its state under /tmp; without it the server judges itself unhealthy and exits.
                            volumeMounts: [
                                { name: "pki", mountPath: "/pki", readOnly: true }
                                { name: "repos", mountPath: "/repos" }
                                { name: "tmp", mountPath: "/tmp" }
                            ]
                        }]
                        volumes: [
                            { name: "pki", secret: { secretName: $"($SERVICE)-tls" } }
                            { name: "repos", persistentVolumeClaim: { claimName: $"($SERVICE)-repos" } }
                            { name: "tmp", emptyDir: {} }
                        ]
                    }
                }
            }
        }
        {
            apiVersion: "v1", kind: "Service"
            metadata: { name: $SERVICE, namespace: $NAMESPACE }
            spec: { selector: $labels, ports: [{ name: "https", port: $DEFAULT_PORT, targetPort: 8443, protocol: "TCP" }] }
        }
    ]
}

# The DNS names a certificate carries, as `openssl x509 -ext subjectAltName` prints them ("DNS:a, DNS:b"). Pure.
export def sans_in [text: string] {
    $text | parse --regex 'DNS:(?<name>[^,\s]+)' | get name
}

# Whether a certificate that carries `have` must be made again to carry `want`: the same names in any order are the same.
export def needs_reissue [have: list<string>, want: list<string>] {
    ($have | sort | uniq) != ($want | sort | uniq)
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

# Makes the CA and the certificates that are not there yet, and makes again one whose names are not the ones it should carry (the
# server's, when the cluster's domain changes). The CA key stays in `pki`; only `serve/` goes to the cluster.
export def ensure_pki [pki: string, target: string, server: string] {
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
    for c in (cert_plan $server) {
        let pem = ($pki | path join $"($c.name).pem")
        let key = ($pki | path join $"($c.name).key")
        if ($pem | path exists) and ($key | path exists) {
            let have = (sans_in (run_ok $"reading ($c.name)'s names" { ^openssl x509 -in $pem -noout -ext subjectAltName }))
            if not (needs_reissue $have $c.dns) { continue }
            rm -f $pem $key
        }
        let csr = ($pki | path join $"($c.name).csr")
        let ext = ($pki | path join $"($c.name).ext")
        ext_text $c | save --force $ext
        run_ok $"making ($c.name)'s key" { ^openssl req -newkey rsa:3072 -nodes -subj $"/CN=($c.name)" -keyout $key -out $csr }
        run_ok $"signing ($c.name)'s certificate" { ^openssl x509 -req -in $csr -CA $ca_pem -CAkey $ca_key -CAcreateserial -days 365 -extfile $ext -out $pem }
        rm -f $csr $ext
        chmod 600 $key
        $made = ($made | append $c.name)
    }
    cp $ca_pem ($serve | path join "ca.pem")
    cp ($pki | path join "server.pem") ($serve | path join "server.pem")
    cp ($pki | path join "server.key") ($serve | path join "server.key")
    chmod 600 ($serve | path join "server.key")
    $made
}

def kubectl_apply [objects: list] {
    let r = ({ apiVersion: "v1", kind: "List", items: $objects } | to json | ^kubectl apply -f - | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"the cluster refused the deployment git server's objects: ($r.stderr | str trim)" } }
}

def forward_running [] {
    (do { ^pgrep -f $"port-forward -n ($NAMESPACE) svc/($SERVICE)" } | complete).exit_code == 0
}

# The commit the server's main branch is at, or "" when it has none.
def server_head [pki: string, target: string, port: int] {
    let r = (do { ^env -u GIT_SSL_CAINFO -u SSL_CERT_FILE git ...(tls_config $pki $WRITER) ls-remote $"https://localhost:($port)/(repo_name $target).git" refs/heads/main } | complete)
    if $r.exit_code != 0 { error make --unspanned { msg: $"cannot read the deployment repository: ($r.stderr | str trim)" } }
    $r.stdout | lines | where {|l| $l | str contains "refs/heads/main" } | each {|l| $l | split row "\t" | first } | get -o 0 | default ""
}

def "main ensure" [
    target: string
    --port: int = 9443          # this machine's end of the forward to the server
    --image: string = ""        # the server's image as this machine pushed it (default: polar-git-server:latest in the push registry)
] {
    let cluster = (cluster_of $target)
    let server = (server_name $cluster.domain)
    let pki = (pki_dir $env.HOME $target)
    let made = (ensure_pki $pki $target $server)
    if ($made | is-empty) { print $"  ok: the deployment PKI is in ($pki)" } else { print $"  made in ($pki): ($made | str join ', ')" }

    # The image by the digest the registry serves, under the name the nodes pull it by.
    let pushed = if ($image | is-empty) { $"(push_registry $target)/polar-git-server:latest" } else { $image }
    let digest = (registry_digest $pushed)
    if $digest == null { error make --unspanned { msg: $"($pushed) is not in the registry, or the registry does not answer: load or push it first" } }
    let node_ref = $"(node_registry $target)/polar-git-server@($digest)"

    let serve = (serve_dir $pki)
    let files = ["server.pem" "server.key" "ca.pem"]
    let pki_sha256 = ($files | each {|f| open --raw ($serve | path join $f) | hash sha256 } | str join "" | hash sha256)
    kubectl_apply (server_manifests $target $node_ref $cluster.storage $pki_sha256 | first 1)
    # The certificate and key go from the files into the Secret without being printed or written anywhere else.
    let secret = (run_ok "building the server's Secret" {
        ^kubectl -n $NAMESPACE create secret generic $"($SERVICE)-tls" ...($files | each {|f| $"--from-file=($f)=($serve | path join $f)" }) --dry-run=client -o yaml
    })
    let applied = ($secret | do { ^kubectl apply -f - } | complete)
    if $applied.exit_code != 0 { error make --unspanned { msg: "applying the server's Secret failed" } }
    kubectl_apply (server_manifests $target $node_ref $cluster.storage $pki_sha256 | skip 1)
    run_ok "waiting for the deployment git server" { ^kubectl -n $NAMESPACE rollout status $"deployment/($SERVICE)" --timeout=5m } | ignore
    print $"  ok: the deployment git server runs in the cluster as ($server):($DEFAULT_PORT) \(($node_ref | split row '@' | last | str substring 0..18)\)"

    if not (forward_running) {
        # Something else on the port (the host container this server replaced, `polar-deploy-<target>`) would answer in its place,
        # with a certificate from the same PKI, and be published to.
        # Anything that accepts a connection counts, a server that then refuses the TLS handshake for want of a client certificate
        # too. Asked of 127.0.0.1: `localhost` can resolve to ::1 first, where nothing answers and a connect waits out its timeout.
        let busy = (do { ^timeout 3 bash -c $"exec 3<>/dev/tcp/127.0.0.1/($port)" } | complete)
        if $busy.exit_code == 0 {
            error make --unspanned { msg: $"something already answers at localhost:($port), and it is not the forward to the server in the cluster: stop it \(the old host container: podman rm -f polar-deploy-($target)\), or pass --port" }
        }
        let started = (with-env { POLAR_DEPLOY_FORWARD_PORT: ($port | into string) } { do { ^just port-forwards-up deploy } | complete })
        if $started.exit_code != 0 { error make --unspanned { msg: $"starting the deployment git server's port-forward failed: ($started.stderr | str trim)" } }
    }
    # Up means it answers the pipeline's certificate over TLS with its own name checked.
    let url = $"https://localhost:($port)/(repo_name $target).git/info/refs?service=git-upload-pack"
    let up = (with_retries 20 2sec {|n|
        let r = (do { ^curl -s -m 5 --cacert ($pki | path join "ca.pem") --cert ($pki | path join $"($WRITER).pem") --key ($pki | path join $"($WRITER).key") -o /dev/null -w "%{http_code}" $url } | complete)
        { ok: ($r.stdout == "200"), final: false, detail: $"answered ($r.stdout) ($r.stderr | str trim)" }
    })
    if not $up.ok { error make --unspanned { msg: $"the deployment git server did not answer at localhost:($port) after ($up.tries) tries: ($up.detail). See: kubectl -n ($NAMESPACE) logs deployment/($SERVICE), /tmp/pf-deploy.log" } }
    print $"  ok: it answers at https://localhost:($port)/ as ($WRITER)"

    # A new server (a new cluster) has no history: it is given the mirror's, so Flux finds the record as it was signed.
    let mirror = (mirror_dir $pki)
    if (server_head $pki $target $port | is-empty) and ($mirror | path exists) {
        let held = (do { ^git -C $mirror rev-parse --verify -q refs/heads/main } | complete)
        if $held.exit_code == 0 {
            run_ok "giving the server the mirror's history" { ^env -u GIT_SSL_CAINFO -u SSL_CERT_FILE git -C $mirror ...(tls_config $pki $WRITER) push -q $"https://localhost:($port)/(repo_name $target).git" refs/heads/main:refs/heads/main }
            print $"  ok: the server was empty and now holds the mirror's history, at ($held.stdout | str trim | str substring 0..11)"
        }
    }
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
export def tree_files [target: string, layers: list, texts: record, server: string, facts: record] {
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
    let source = (git_repository $target $server $DEFAULT_PORT)
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

# What the cluster supplied to this render (its git server administrators): the file, its digest, the cluster it is tied to, and the
# values, so the signed record says exactly which identities were rendered in. Null when there were none.
export def cluster_inputs_fact [config: record, target: string] {
    let path = (cluster-inputs path_of $config.infra_root $target)
    if not ($path | path exists) { return null }
    let held = (cluster-inputs read_inputs $path)
    {
        file: $"infra/targets/($target)/cluster-inputs.nuon"
        sha256: (open --raw $path | hash sha256)
        cluster_ca_sha256: ($held | get -o cluster)
        values: ($held | get -o workloads | default {})
    }
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
    --port: int = 9443          # this machine's end of the forward to the deployment git server
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
        cluster_inputs: (cluster_inputs_fact $config $target)
        image_lock: (if ($lock | path exists) { { file: "infra/images.lock.nuon", sha256: (digest_of $lock) } } else { null })
        flux_controllers: (if ($controllers | path exists) { { sha256: (digest_of $controllers), bootstrap_layers: $BOOTSTRAP_LAYERS } } else { null })
        deployment_ca_sha256: (sha256_of ($pki | path join "ca.pem"))
        signer: { identity: $WRITER, certificate_sha256: (sha256_of ($pki | path join $"($WRITER).pem")) }
    }
    let files = (tree_files $target $config.apply_layers $texts (server_name (cluster_of $target).domain) $facts)

    let url = $"https://localhost:($port)/(repo_name $target).git"
    let work = (mktemp -d)
    let copy = ($work | path join "repo")
    let outcome = (try {
        let result = (with-env { GNUPGHOME: $signing.home } {
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
        })
        # The signed history, kept off the cluster as well: `ensure` gives a new server this mirror's.
        if ($result.commit | is-not-empty) {
            let mirror = (mirror_dir $pki)
            if not ($mirror | path exists) { run_ok "making the mirror" { ^git init -q --bare $mirror } | ignore }
            run_ok "updating the mirror" { ^git -C $copy push -q --force $mirror HEAD:refs/heads/main } | ignore
        }
        $result
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
