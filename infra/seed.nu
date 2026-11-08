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
# This module is the decisions (what to seed, what a body is, what to skip, what the logs say), and the
# one call that hands jira-observer's token to polar-sign on its standard input (below). The step that
# touches the cluster is `step_seed` in up.nu.

export const REPO = "polar-schedules"
export const GIT_HOST = "polar-git-server.polar.svc.cluster.local"
export const GIT_PORT = "8443"

# The documents a cluster starts with. A body comes from a file (`file`, its text in the one `field`) or is computed by
# the resolver from the catalog (`resolve`, the `resolve.nu` command up.nu asks for it; `from` says what it is made of):
# the state agent's first `schema` from the agents' scaffolds, and its writer's first `agents` from the catalog's
# workloads (docs/architecture/soft-state.md). `version` is 1 because this is only ever the first: a path that already
# exists is left alone, and a later change is an author's, at a higher version.
export def documents [] {
    [
        {
            path: "permanent/credentials-agent-policy.dhall"
            agent: "credentials-agent"
            section: "policy"
            version: 1
            capability: "c:update-policy"
            field: "source"
            file: "infra/seeds/signed/policy.cedar"
        }
        {
            path: "permanent/soft-state-observer-schema.dhall"
            agent: "soft-state-observer"
            section: "schema"
            version: 1
            capability: "c:update-runtime"
            from: "scaffolds"
            resolve: "state-schema"
        }
        {
            path: "permanent/soft-state-processor-agents.dhall"
            agent: "soft-state-processor"
            section: "agents"
            version: 1
            capability: "c:update-runtime"
            from: "catalog"
            resolve: "health-agents"
        }
    ]
}

# Whether a document's body is made from the agents' scaffolds, and not from a file.
export def from_scaffolds [doc: record] {
    ($doc.from? | default "file") == "scaffolds"
}

# Whether a document's body is computed by the resolver (from the scaffolds or the catalog), and not read from a file.
export def computed [doc: record] {
    ($doc.from? | default "file") != "file"
}

# The body of a section, as the JSON `polar-sign runtime` takes: the file's text, exactly, in its field; or, for a
# computed body, `schema` (the record its `resolve.nu` command printed), as it is.
export def body_json [doc: record, root: string, schema?: record] {
    if (computed $doc) {
        if $schema == null { error make { msg: $"($doc.section) for ($doc.agent) is made from the ($doc.from), and none was given" } }
        $schema | to json --raw
    } else {
        { ($doc.field): (open --raw ($root | path join $doc.file)) } | to json --raw
    }
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

export def refused_hint [identity: string, target: string] {
    $"the Polar git server does not know ($identity) as an administrator, so it will not take the seed. That identity belongs to this cluster: up.nu records it in infra/targets/($target)/cluster-inputs.nuon, renders and publishes again, and Flux restarts the git server with it. If it still refuses, check that file and that the git server's pod restarted after the publish."
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

# ── jira-observer's credential, sealed once from the sops files ──────────────────────────────────────────────
#
# On a fresh cluster, a `jira_token` in the target's sops files (and `jira_email`, for Jira Cloud) is sealed into
# jira-observer's signed `credentials` runtime section, version 1, and pushed with the rest of the seed. Only when
# polar-schedules has no such section: after that the token is an author's, changed with `polar-sign` and a signed push
# (docs/architecture/jira.md, "The credential"). The token goes to `polar-sign sign` on standard input and nowhere else:
# the file it signs holds `-` in its place, and no line here holds it.

export const JIRA_AGENT = "jira-observer"
export const CREDENTIALS_SECTION = "credentials"
# The entry's name, which its secret is bound to (jira-observer|credentials|<version>|<name>|secret).
export const JIRA_CREDENTIAL_NAME = "jira"
# Where `polar-sign sign --push` puts the section (polar-sign `push::place`), so a later rotation replaces this file.
export const JIRA_CREDENTIAL_PATH = "permanent/jira-observer/credentials.dhall"
# The host the target templates ship in JIRA_URL until someone sets the real one (jira-observer refuses it at start).
const JIRA_PLACEHOLDER_HOST = "example.atlassian.net"

# jira-observer's document as the render wrote it (ConfigMap `jira-observer-config` in
# `<output_dir>/jira-observer-resources.yaml`, its `config.json`), or null when the render has none.
export def jira_document [output_dir: string] {
    let file = ($output_dir | path join "jira-observer-resources.yaml")
    if not ($file | path exists) { return null }
    let text = (open --raw $file
        | split row "\n---\n"
        | each {|d| try { $d | from yaml } catch { null } }
        | where {|o| ($o | describe | str starts-with "record") and ($o.kind? == "ConfigMap") and ($o.metadata?.name? == "jira-observer-config") }
        | get -o 0.data."config.json")
    if $text == null { null } else { $text | from json }
}

# The host a credential for `url` is bound to, as jira-observer asks for one (`credentials::credential_host`): the
# lowercased host of an https address that is a name, with no login of its own, and not the placeholder. Returns
# `{ host, problem }`: `problem` says, without the URL, why there is none.
export def jira_host [url: string] {
    let parsed = (try { $url | str trim | url parse } catch { null })
    let host = ($parsed.host? | default "")
    let problem = if $parsed == null or ($host | is-empty) {
        "cannot be read as an address"
    } else if ($parsed.scheme | str lowercase) != "https" {
        "is not https, and jira-observer sends a credential only over https"
    } else if ($parsed.username | is-not-empty) or ($parsed.password | is-not-empty) {
        "carries a login of its own"
    } else if ($host | str starts-with "[") or (($host | split row "." | last) =~ '^([0-9]+|0x.*)$') {
        "names an address, and a credential is bound to a host name"
    } else if $host == $JIRA_PLACEHOLDER_HOST {
        "is still the example placeholder"
    } else { null }
    if $problem == null { { host: ($host | str lowercase), problem: null } } else { { host: null, problem: $problem } }
}

# Whether `doc` (a document of the schedules repository, as `dhall-to-json` gives it) is a runtime section for
# `agent`'s `section`. The scheduler routes a document by what it says, not by where it is, so this is how a section
# already there is known.
export def holds_section [doc: any, agent: string, section: string] {
    if not ($doc | describe | str starts-with "record") { return false }
    let p = ($doc.payload? | default {})
    if not ($p | describe | str starts-with "record") { return false }
    ($p.type? == "runtime") and ($p.target?.agent? == $agent) and ($p.section? == $section)
}

# Whether a file's text names both `agent` and `section`: only such a file is evaluated to see if it holds the section.
export def may_hold_section [text: string, agent: string, section: string] {
    ($text | str contains $"\"($agent)\"") and ($text | str contains $"\"($section)\"")
}

# Which of `paths` (relative to `root`, a clone of the schedules repository) hold a runtime section for `agent`'s
# `section`: the Dhall files under a folder the scheduler reads (`permanent`, `adhoc`, `ephemeral`, as its watcher's
# `is_schedule_file`), judged by what they evaluate to. A file that names both and does not evaluate is counted, so
# that a section is never sealed twice.
export def sections_in [root: string, paths: list<string>, agent: string, section: string] {
    $paths
    | where {|p| ($p | str ends-with ".dhall") and ($p | path split | any {|c| $c in ["permanent" "adhoc" "ephemeral"] }) }
    | where {|p| may_hold_section (open --raw ($root | path join $p)) $agent $section }
    | where {|p|
        let r = (do { ^dhall-to-json --file ($root | path join $p) } | complete)
        ($r.exit_code != 0) or (holds_section (try { $r.stdout | from json } catch { null }) $agent $section)
    }
}

# Whether the seed has anything to say about jira-observer's credential: the target runs jira-observer, and it does not
# read anonymously (`settings`, its rendered `{ url, anonymous }`). When not, nothing more is read for it.
export def jira_wanted [deployed: bool, settings: record] {
    $deployed and (not $settings.anonymous)
}

# What the seed does with jira-observer's credential, decided from `given`:
#   deployed   the target runs jira-observer
#   settings   its rendered `{ url, anonymous }` (jira-check.nu `settings_of_document`)
#   token      `jira_token` from the sops files ("" or null when none)
#   email      `jira_email` from the sops files ("" or null when none): with one, the credential is Jira Cloud's
#              user-password, the email its username; without, a Data Center or Server personal access token
#   existing   the paths of polar-schedules that already hold a jira-observer `credentials` section
#   unread     the sops files that would not decrypt, each `{ file, reason }`
#   user       the administrator who signs
# Returns `{ action, line, input }`. `action` is `skip` (no line: an anonymous target, or one without jira-observer),
# `exists`, `no-token`, `unusable` (JIRA_URL cannot carry a credential) or `seal`; `input` is the file `polar-sign sign`
# takes for a seal, the secret written `-` so that it is read from standard input. No line holds the token.
export def jira_credential [given: record] {
    if not (jira_wanted $given.deployed $given.settings) {
        return { action: "skip", line: null, input: null }
    }
    let rotate = $"polar-sign template ($JIRA_AGENT) ($CREDENTIALS_SECTION) --out <file>, fill it in, then polar-sign sign <file> --user ($given.user) --push"
    if ($given.existing | is-not-empty) {
        return {
            action: "exists"
            line: $"($JIRA_AGENT) already has a ($CREDENTIALS_SECTION) section in polar-schedules \(($given.existing | str join ', ')\), so the jira_token in the sops files is not sealed again: that is done once, on a fresh cluster. To change the token: ($rotate), at a version higher than the last"
            input: null
        }
    }
    let token = ($given.token | default "")
    if ($token | is-empty) {
        let why = if ($given.unread | is-empty) {
            "no jira_token in the target's sops files"
        } else {
            $"no jira_token could be read from the target's sops files \(($given.unread | each {|f| $'($f.file): ($f.reason)' } | str join '; ')\)"
        }
        return {
            action: "no-token"
            line: $"($why): ($JIRA_AGENT) will hold Credentials until a ($CREDENTIALS_SECTION) section is pushed. To push one: ($rotate) \(or put jira_token in the sops files and run up.nu again while polar-schedules has none\)"
            input: null
        }
    }
    let host = (jira_host ($given.settings.url | default ""))
    if $host.problem != null {
        return {
            action: "unusable"
            line: $"($JIRA_AGENT)'s JIRA_URL ($host.problem), so the jira_token in the sops files is not sealed: set it in workloads.($JIRA_AGENT).env of the target's config.nuon and run up.nu again"
            input: null
        }
    }
    let email = ($given.email | default "")
    let entry = if ($email | is-empty) {
        { name: $JIRA_CREDENTIAL_NAME, host_pattern: $host.host, kind: "token", secret: "-" }
    } else {
        { name: $JIRA_CREDENTIAL_NAME, host_pattern: $host.host, kind: "user-password", username: $email, secret: "-" }
    }
    {
        action: "seal"
        line: $"sealing the jira_token in the sops files into ($JIRA_AGENT)'s ($CREDENTIALS_SECTION) section, version 1, as a ($entry.kind) for JIRA_URL's host"
        input: ({ agent: $JIRA_AGENT, section: $CREDENTIALS_SECTION, version: 1, body: { credentials: [$entry] } } | to json)
    }
}

# The arguments of `polar-sign sign` for the seal: the input file, signed as `user`, checked against the agent's own
# scaffold, its secret sealed to the escrow certificate that names `escrow_identity`, written to `out` in the seed's
# clone. Never the token, and not `--push`: the seed pushes it with the rest of its commit.
export def sign_args [input: string, user: string, scaffold: string, escrow_identity: string, out: string] {
    ["sign" $input "--user" $user "--scaffold" $scaffold "--escrow-identity" $escrow_identity "--out" $out]
}

# Runs `sign` (`tools command "polar-sign"`) with `args`, the token on its standard input. Returns `{ exit_code, stdout,
# stderr }`, the token replaced in both should the tool ever print it.
export def seal [sign: list<string>, args: list<string>, token: string] {
    let r = (do { $token | ^($sign | first) ...($sign | skip 1) ...$args } | complete)
    let mask = {|text| if ($token | is-empty) { $text } else { $text | str replace --all $token "<the token>" } }
    { exit_code: $r.exit_code, stdout: (do $mask $r.stdout), stderr: (do $mask $r.stderr) }
}

# What a seal came to, for `retry`: done; worth another try while jira-observer has not escrowed its key yet (its pod
# is still starting) or the gateway's port-forward is reconnecting; final for anything else.
export def seal_outcome [exit_code: int, stderr: string] {
    if $exit_code == 0 { return { ok: true, final: false, detail: "" } }
    let waiting = ($stderr | str contains "has no escrow certificate") or ($stderr | str contains "cannot reach the credentials agent's gateway")
    { ok: false, final: (not $waiting), detail: ($stderr | str trim) }
}
