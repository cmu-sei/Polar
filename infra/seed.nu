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

# infra/seed.nu
#
# What a cluster is given before anyone has written anything of their own: signed documents in the
# schedules repository. They are signed here, at setup, by the first administrator, and never
# committed signed: a signature made once by a person would be baked into every cluster, and a
# cluster's CA is its own. Design: docs/architecture/credentials-policy.md, "Seeding the cluster".
#
# This module is the decisions (what to seed, what a body is, what to skip, what the logs say). The
# step that touches the cluster is `step_seed` in up.nu.

export const REPO = "polar-schedules"
export const GIT_HOST = "polar-git-server.polar.svc.cluster.local"
export const GIT_PORT = "8443"

# The documents a cluster starts with. `field` is the one field of the section's body that the file
# fills; `version` is 1 because this is only ever the first: a path that already exists is left alone,
# and a later change is an author's, at a higher version.
export def documents [] {
    [
        {
            path: "permanent/credentials-agent-policy.dhall"
            agent: "credentials-agent-consumer"
            section: "policy"
            version: 1
            capability: "c:update-policy"
            field: "source"
            file: "infra/seeds/signed/policy.cedar"
        }
    ]
}

# The body of a section, as the JSON `polar-sign runtime` takes: the file's text, exactly, in its field.
export def body_json [doc: record, root: string] {
    { ($doc.field): (open --raw ($root | path join $doc.file)) } | to json --raw
}

# The documents whose path is not in the repository yet.
export def missing [docs: list, existing: list] {
    $docs | where {|d| $d.path not-in $existing }
}

# `git -c ...` arguments that make git present the user's Polar certificate to the Polar git server,
# for one command and without touching the user's configuration (the same as `just polar-git-clone`).
export def git_config [user: string, dir: string] {
    [
        "-c" $"http.sslCAInfo=($dir)/ca.pem"
        "-c" $"http.sslCert=($dir)/($user).crt"
        "-c" $"http.sslKey=($dir)/($user).key"
        "-c" $"http.curloptResolve=($GIT_HOST):($GIT_PORT):127.0.0.1"
    ]
}

export def repo_url [] {
    $"https://($GIT_HOST):($GIT_PORT)/($REPO).git"
}

# What to say when the git server refuses the administrator: who it is that the server must be told
# about. The identity is the first DNS name in the user's certificate; the server learns its
# administrators from POLAR_GIT_SERVER_ADMINS in the target's config.
# What a `git clone` of the schedules repository came to, for `retry`: done; final when the git server refused
# this identity (403: trying again cannot fix it); otherwise a transport failure, which is worth another try,
# because the port-forward may be reconnecting to a pod that was just replaced.
export def clone_outcome [exit_code: int, stderr: string] {
    if $exit_code == 0 {
        { ok: true, final: false, detail: "" }
    } else {
        { ok: false, final: ($stderr | str contains "403"), detail: ($stderr | str trim) }
    }
}

# The target's config.nuon text with `identity` among the git server's administrators. The text is edited,
# not parsed and written back, so the file's comments stay. Other identities are kept (a cluster may have
# several administrators; one a reset has made dead is harmless). `found` is false when the file has no
# POLAR_GIT_SERVER_ADMINS to edit; `changed` is false when the identity was already listed.
export def with_admin [text: string, identity: string] {
    let pattern = '(name:\s*"POLAR_GIT_SERVER_ADMINS",\s*value:\s*")(?<value>[^"]*)(")'
    let hit = ($text | parse --regex $pattern)
    if ($hit | is-empty) { return { text: $text, found: false, changed: false } }
    let held = ($hit | first | get value | split row "," | each {|i| $i | str trim } | where {|i| $i != "" })
    if $identity in $held { return { text: $text, found: true, changed: false } }
    let value = ($held | append $identity | str join ",")
    {
        text: ($text | str replace --regex $pattern $'${1}($value)${3}')
        found: true
        changed: true
    }
}

export def refused_hint [identity: string, target: string] {
    $"the Polar git server does not know ($identity) as an administrator, so it will not take the seed. That identity belongs to this cluster, and a reset gives new ones: set POLAR_GIT_SERVER_ADMINS for polar-git-server in infra/targets/($target)/config.nuon to include it \(comma-separated; drop the previous cluster's\), then run this again. Running it again restarts the git server by itself, because its ConfigMap changes."
}

# Whether the credentials agent has said it published `section`'s authority: its log line for the first
# publication of anything it signs. The text is `publishing an authority update`, with the kind beside it.
export def authority_published [logs: string, section: string] {
    # The agent's log is coloured, and the colour codes sit inside `kind="policy"`.
    $logs
    | ansi strip
    | lines
    | any {|l|
        let published = ($l | str contains "publishing an authority update")
        let kind = (($l | str contains $"kind=\"($section)\"") or ($l | str contains $"kind=($section)"))
        $published and $kind
    }
}

# The first DNS name in `openssl x509 -noout -ext subjectAltName`, which is the identity the git server
# knows an account by (the account id, for a user). Null when there is none.
export def first_dns_name [san: string] {
    let found = ($san | parse --regex 'DNS:(?<name>[^,\s]+)')
    if ($found | is-empty) { null } else { $found | first | get name }
}

# The capabilities the documents need that the certificate does not carry, from its
# `openssl x509 -noout -ext subjectAltName` (a capability is a URI name, `URI:c:update-policy`). A document
# signed without one is refused by whoever receives it, so the step says so before it signs.
export def lacks [san: string, docs: list] {
    let held = ($san | parse --regex 'URI:(?<uri>[^,\s]+)' | get uri)
    $docs | get capability | uniq | where {|c| $c not-in $held }
}

export def lacks_hint [user: string, missing: list] {
    $"($user)'s certificate does not carry ($missing | str join ', '), so the credentials agent would refuse the policy. Provision an account that does \(just admin-provision gives every new administrator these\), and run this again with --user."
}
