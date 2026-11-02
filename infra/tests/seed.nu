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

# infra/tests/seed.nu
#
# Tests of the decisions in infra/seed.nu.
#
#   nu infra/tests/seed.nu

use std/assert
use ../seed.nu *

def root [] { $env.FILE_PWD | path dirname | path dirname }

# A token no real site issues, and long enough that no other text holds it by chance.
const TOKEN = "NjQ2-test-token-Zq81xKpW0r"

# What the seed is given about jira-observer on a fresh cluster with a token, as `jira_credential` takes it.
def given [] {
    {
        deployed: true
        settings: { url: "https://jira.example.org", anonymous: false }
        token: $TOKEN
        email: ""
        existing: []
        unread: []
        user: "alice"
    }
}

# jira-observer's document as the render writes it, in a resources file with the ConfigMap beside other objects.
def rendered [dir: string, variables: list] {
    let document = { sections: [{ name: "jira", variables: $variables }] }
    let objects = [
        { apiVersion: "v1", kind: "ServiceAccount", metadata: { name: "jira-observer", namespace: "polar" } }
        # Another ConfigMap of the workload's, first: only the document's is read.
        { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "jira-observer-other", namespace: "polar" }, data: { "config.json": "{\"sections\": []}" } }
        { apiVersion: "v1", kind: "ConfigMap", metadata: { name: "jira-observer-config", namespace: "polar" }, data: { "config.json": ($document | to json) } }
    ]
    $objects | each {|o| $o | to yaml } | str join "\n---\n" | save --force ($dir | path join "jira-observer-resources.yaml")
}

def main [] {
    let tests = [
        {
            name: "jira-observer's credential is sealed on a fresh cluster with a token: version 1, one entry named jira, the secret read from standard input"
            body: {
                let r = (jira_credential (given))
                assert equal $r.action "seal"
                let input = ($r.input | from json)
                assert equal $input.agent "jira-observer"
                assert equal $input.section "credentials"
                assert equal $input.version 1
                assert equal ($input.body.credentials | length) 1
                let entry = ($input.body.credentials | first)
                assert equal $entry { name: "jira", host_pattern: "jira.example.org", kind: "token", secret: "-" }
                assert ($r.line | str contains "sealing") $r.line
            }
        }
        {
            name: "with an email in the sops files the credential is a user-password, the email its username (Jira Cloud)"
            body: {
                let r = (jira_credential ((given) | upsert email "someone@example.org"))
                assert equal $r.action "seal"
                let entry = ($r.input | from json | get body.credentials | first)
                assert equal $entry.kind "user-password"
                assert equal $entry.username "someone@example.org"
                assert equal $entry.secret "-"
            }
        }
        {
            name: "host_pattern is the host of the rendered JIRA_URL: lowercased, without its port, path or scheme"
            body: {
                for c in [
                    ["https://jira.example.org" "jira.example.org"]
                    ["https://issues.apache.org/jira" "issues.apache.org"]
                    ["https://Jira.Example.ORG:8443/jira/" "jira.example.org"]
                    ["https://site.atlassian.net" "site.atlassian.net"]
                ] {
                    let r = (jira_credential ((given) | upsert settings.url $c.0))
                    assert equal $r.action "seal" $c.0
                    assert equal ($r.input | from json | get body.credentials.0.host_pattern) $c.1 $c.0
                    assert equal (jira_host $c.0).host $c.1 $c.0
                }
            }
        }
        {
            name: "a JIRA_URL the agent would send no credential to is not sealed for, and the line says why without the URL"
            body: {
                for url in [
                    "http://jira.example.org"
                    "https://user:pw@jira.example.org"
                    "https://10.0.0.5"
                    "https://example.atlassian.net"
                    ""
                    "not a url"
                ] {
                    let r = (jira_credential ((given) | upsert settings.url $url))
                    assert equal $r.action "unusable" $url
                    assert equal $r.input null $url
                    assert (($r.line | lines | length) == 1) $url
                    assert ($r.line | str contains "not sealed") $r.line
                    if ($url | is-not-empty) and ($url != "not a url") { assert (not ($r.line | str contains $url)) $r.line }
                    assert ((jira_host $url).host == null) $url
                }
            }
        }
        {
            name: "a cluster whose polar-schedules already has jira-observer's credentials is left alone, with one line saying how to rotate"
            body: {
                let r = (jira_credential ((given) | upsert existing ["permanent/jira-observer/credentials.dhall"]))
                assert equal $r.action "exists"
                assert equal $r.input null
                assert equal ($r.line | lines | length) 1
                assert ($r.line | str contains "permanent/jira-observer/credentials.dhall") $r.line
                assert ($r.line | str contains "polar-sign template jira-observer credentials") $r.line
                assert ($r.line | str contains "polar-sign sign") $r.line
                assert ($r.line | str contains "--push") $r.line
                # It is there whether or not the sops files have a token, and whatever JIRA_URL says.
                assert equal (jira_credential ((given) | upsert existing ["adhoc/x.dhall"] | upsert token "")).action "exists"
                assert equal (jira_credential ((given) | upsert existing ["adhoc/x.dhall"] | upsert settings.url "http://x")).action "exists"
            }
        }
        {
            name: "with no token in the sops files: one line that jira-observer holds Credentials until a section is pushed, and how to push one"
            body: {
                let r = (jira_credential ((given) | upsert token ""))
                assert equal $r.action "no-token"
                assert equal $r.input null
                assert equal ($r.line | lines | length) 1
                assert ($r.line | str contains "jira_token") $r.line
                assert ($r.line | str contains "Credentials") $r.line
                assert ($r.line | str contains "polar-sign template jira-observer credentials") $r.line
                assert ($r.line | str contains "--push") $r.line
                assert equal (jira_credential ((given) | upsert token null)).action "no-token"
                # A sops file that would not decrypt is named, with why, never a value.
                let unread = (jira_credential ((given) | upsert token "" | upsert unread [{ file: "user.sops.yaml", reason: "no identity matched" }]))
                assert equal $unread.action "no-token"
                assert ($unread.line | str contains "user.sops.yaml") $unread.line
                assert ($unread.line | str contains "no identity matched") $unread.line
            }
        }
        {
            name: "an anonymous target, and one without jira-observer, are skipped in silence"
            body: {
                for g in [
                    ((given) | upsert settings.anonymous true)
                    ((given) | upsert deployed false)
                    ((given) | upsert settings.anonymous true | upsert token "")
                    ((given) | upsert deployed false | upsert existing ["permanent/jira-observer/credentials.dhall"])
                ] {
                    let r = (jira_credential $g)
                    assert equal $r.action "skip"
                    assert equal $r.line null
                    assert equal $r.input null
                }
            }
        }
        {
            name: "the token is in no line the step prints and in nothing it writes: the input to sign holds `-` in its place"
            body: {
                for g in [
                    (given)
                    ((given) | upsert email "someone@example.org")
                    ((given) | upsert existing ["permanent/jira-observer/credentials.dhall"])
                    ((given) | upsert settings.url "http://jira.example.org")
                    ((given) | upsert settings.anonymous true)
                ] {
                    let r = (jira_credential $g)
                    assert (not ($r.line | default "" | str contains $TOKEN)) $r.action
                    assert (not ($r.input | default "" | str contains $TOKEN)) $r.action
                }
            }
        }
        {
            name: "polar-sign is given the token on standard input only: not in its arguments, and not in what it printed"
            body: {
                let dir = (mktemp -d)
                let record = ($dir | path join "call")
                # Stands in for polar-sign: keeps its arguments and its standard input, and prints the input back on
                # stderr, as a careless tool might.
                let fake = ["sh" "-c" 'printf "%s\n" "$@" > "$0.args"; cat > "$0.stdin"; printf "got %s\n" "$(cat "$0.stdin")" >&2' $record]
                let args = (sign_args ($dir | path join "in.json") "alice" ($dir | path join "s.json") "jira-observer.polar.serviceaccount.cluster.local" ($dir | path join "out.dhall"))
                let r = (seal $fake $args $TOKEN)
                assert equal $r.exit_code 0
                let argv = (open --raw $"($record).args")
                assert (not ($argv | str contains $TOKEN)) $argv
                assert (not ($args | any {|a| $a | str contains $TOKEN }))
                assert equal (open --raw $"($record).stdin") $TOKEN
                assert (not ($r.stderr | str contains $TOKEN)) $r.stderr
                assert (not ($r.stdout | str contains $TOKEN)) $r.stdout
                assert ($r.stderr | str contains "got") $r.stderr
                rm -rf $dir
            }
        }
        {
            name: "polar-sign signs the file as the admin, with the agent's scaffold and escrow identity, into the clone"
            body: {
                let args = (sign_args "/w/in.json" "alice" "/w/s/jira-observer.json" "jira-observer.polar.serviceaccount.cluster.local" "/w/repo/permanent/jira-observer/credentials.dhall")
                assert equal ($args | first 2) ["sign" "/w/in.json"]
                let flag = {|name| $args | get (($args | enumerate | where item == $name | first | get index) + 1) }
                assert equal (do $flag "--user") "alice"
                assert equal (do $flag "--scaffold") "/w/s/jira-observer.json"
                assert equal (do $flag "--escrow-identity") "jira-observer.polar.serviceaccount.cluster.local"
                assert equal (do $flag "--out") "/w/repo/permanent/jira-observer/credentials.dhall"
                assert (not ("--push" in $args)) "the seed pushes it with the rest of its commit"
                assert equal $JIRA_CREDENTIAL_PATH "permanent/jira-observer/credentials.dhall"
            }
        }
        {
            name: "a seal waits while jira-observer has not escrowed its key yet or the gateway's forward is reconnecting, and stops at any other refusal"
            body: {
                assert (seal_outcome 0 "wrote x").ok
                let waiting = (seal_outcome 1 "  `credentials[0].secret`: the credentials agent has no escrow certificate for `jira-observer.polar.serviceaccount.cluster.local`: the agent has not started on this cluster yet, or is not called that\nError: nothing was signed")
                assert (not $waiting.ok)
                assert (not $waiting.final)
                let forward = (seal_outcome 1 "  `credentials[0].secret`: cannot reach the credentials agent's gateway at http://localhost:8081 (is the port-forward up?): error sending request")
                assert (not $forward.final)
                let refused = (seal_outcome 1 "  `credentials[0].host_pattern`: a host pattern is a bare host name\nError: nothing was signed: 1 problem(s)")
                assert (not $refused.ok)
                assert $refused.final
                assert ($refused.detail | str contains "host pattern") $refused.detail
            }
        }
        {
            name: "a document is jira-observer's credentials section by what it says, wherever it is in the repository"
            body: {
                let doc = {|agent, section| { payload: { type: "runtime", target: { agent: $agent }, section: $section, version: 3, body: {} }, signature: {} } }
                assert (holds_section (do $doc "jira-observer" "credentials") "jira-observer" "credentials")
                assert (not (holds_section (do $doc "git-repo-observer" "credentials") "jira-observer" "credentials"))
                assert (not (holds_section (do $doc "jira-observer" "settings") "jira-observer" "credentials"))
                assert (not (holds_section { payload: { type: "configuration", target: { agent: "jira-observer" } } } "jira-observer" "credentials"))
                assert (not (holds_section {} "jira-observer" "credentials"))
                assert (not (holds_section null "jira-observer" "credentials"))
                # Only a file that names both is read further.
                assert (may_hold_section "agent = \"jira-observer\" ... section = \"credentials\"" "jira-observer" "credentials")
                assert (not (may_hold_section "agent = \"jira-observer\" ... section = \"schema\"" "jira-observer" "credentials"))
                assert (not (may_hold_section "agent = \"git-repo-observer\", section = \"credentials\"" "jira-observer" "credentials"))
            }
        }
        {
            name: "the repository's credentials sections are found by evaluating what names them, in the folders the scheduler reads"
            body: {
                let dir = (mktemp -d)
                let doc = {|agent, section| $"-- Signed by `polar-sign`.\n{ payload = { body = { credentials = [] : List {} }, id = \"x\", section = \"($section)\", target = { agent = \"($agent)\" }, type = \"runtime\", version = 2 }, signature = { algorithm = \"a\", value = \"v\" } }\n" }
                let files = {
                    "permanent/jira-observer/credentials.dhall": (do $doc "jira-observer" "credentials")
                    "adhoc/rotated.dhall": (do $doc "jira-observer" "credentials")
                    "permanent/git-repo-observer/credentials.dhall": (do $doc "git-repo-observer" "credentials")
                    "permanent/jira-observer/configuration.dhall": (do $doc "jira-observer" "settings")
                    # Names both in its text, and is another agent's section by what it says.
                    "permanent/git-repo-observer/mentions.dhall": ((do $doc "git-repo-observer" "credentials") | str replace 'id = "x"' 'id = "jira-observer"')
                    "elsewhere/jira-observer-credentials.dhall": (do $doc "jira-observer" "credentials")
                    "permanent/notes.txt": (do $doc "jira-observer" "credentials")
                    # Names both and does not evaluate: counted, so that the section is never sealed twice.
                    "ephemeral/broken.dhall": "{ section = \"credentials\", agent = \"jira-observer\""
                }
                for f in ($files | transpose path text) {
                    mkdir ($dir | path join $f.path | path dirname)
                    $f.text | save ($dir | path join $f.path)
                }
                let found = (sections_in $dir ($files | columns) "jira-observer" "credentials")
                assert equal ($found | sort) ["adhoc/rotated.dhall" "ephemeral/broken.dhall" "permanent/jira-observer/credentials.dhall"]
                assert equal (sections_in $dir ["permanent/git-repo-observer/credentials.dhall"] "jira-observer" "credentials") []
                assert equal (sections_in $dir [] "jira-observer" "credentials") []
                # The documents are real Dhall: what is found is found by evaluating it, not by failing to.
                let evaluated = (^dhall-to-json --file ($dir | path join "permanent/jira-observer/credentials.dhall") | from json)
                assert (holds_section $evaluated "jira-observer" "credentials")
                rm -rf $dir
            }
        }
        {
            name: "the settings are read from jira-observer's rendered document; none when the render has no such document"
            body: {
                let dir = (mktemp -d)
                assert equal (jira_document $dir) null
                rendered $dir [{ name: "JIRA_URL", value: "https://issues.apache.org/jira" } { name: "JIRA_ANONYMOUS", value: "true" }]
                let doc = (jira_document $dir)
                assert equal ($doc.sections.0.variables | where name == "JIRA_URL" | get 0.value) "https://issues.apache.org/jira"
                rm -rf $dir
            }
        }
        {
            name: "a clone that worked is done, and one refused with 403 is final: trying again cannot fix a refusal"
            body: {
                assert equal (clone_outcome 0 "").ok true
                let refused = (clone_outcome 128 "fatal: unable to access 'https://x/': The requested URL returned error: 403")
                assert (not $refused.ok)
                assert $refused.final
            }
        }
        {
            name: "a clone that failed in transport is worth another try: the forward may be reconnecting to a replaced pod"
            body: {
                for stderr in [
                    "fatal: unable to access 'https://x/': TLS connect error: error:0A000126:SSL routines::unexpected eof while reading"
                    "fatal: unable to access 'https://x/': Send failure: Broken pipe"
                    "fatal: unable to access 'https://x/': Failed to connect to localhost port 8443 after 0 ms: Couldn't connect to server"
                    "fatal: unable to access 'https://x/': Recv failure: Connection reset by peer"
                ] {
                    let r = (clone_outcome 128 $stderr)
                    assert (not $r.ok) $stderr
                    assert (not $r.final) $stderr
                    assert ($r.detail | str contains "fatal") $stderr
                }
            }
        }
        {
            name: "every seed document has a place of its own and a section it fills"
            body: {
                let docs = (documents)
                assert (($docs | length) > 0)
                assert equal ($docs | get path | uniq | length) ($docs | length)
                for d in $docs {
                    assert ($d.path | str ends-with ".dhall")
                    assert equal $d.version 1
                    if not (computed $d) {
                        assert (((root) | path join $d.file) | path exists) $"($d.file) is missing"
                    }
                }
            }
        }
        {
            name: "the first policy is seeded for the credentials agent's policy section"
            body: {
                let p = (documents | where section == "policy" | first)
                assert equal $p.agent "credentials-agent"
                assert equal $p.field "source"
            }
        }
        {
            name: "the state agent's first schema is seeded from the scaffolds, signed for c:update-runtime, and is the schema as given"
            body: {
                let d = (documents | where section == "schema" | first)
                assert equal $d.agent "soft-state-observer"
                assert equal $d.capability "c:update-runtime"
                assert (from_scaffolds $d)
                let schema = { workloads: [{ workload: "jira-observer", identity: "jira-observer.polar.serviceaccount.cluster.local", owns: [], reads: ["JiraIssue"] }] }
                assert equal (body_json $d (root) $schema | from json) $schema
                let r = (try { body_json $d (root); "made" } catch {|e| $e.msg })
                assert ($r | str contains "none was given") $r
                assert (not (from_scaffolds (documents | where section == "policy" | first)))
            }
        }
        {
            name: "the writer's first agents are seeded from the catalog by the resolver, signed for c:update-runtime, and are the body as given"
            body: {
                let d = (documents | where section == "agents" | first)
                assert equal $d.agent "soft-state-processor"
                assert equal $d.capability "c:update-runtime"
                assert (computed $d)
                assert (not (from_scaffolds $d))
                let agents = { workloads: [{ workload: "jira-observer", identity: "jira-observer.polar.serviceaccount.cluster.local" }] }
                assert equal (body_json $d (root) $agents | from json) $agents
                let r = (try { body_json $d (root); "made" } catch {|e| $e.msg })
                assert ($r | str contains "made from the catalog, and none was given") $r
            }
        }
        {
            name: "every computed body names a resolver command that exists, and no file"
            body: {
                let resolver = (open --raw ((root) | path join "infra/resolve.nu"))
                for d in (documents | where {|d| computed $d }) {
                    assert ($resolver | str contains $"def \"main ($d.resolve)\"") $"resolve.nu has no ($d.resolve)"
                    assert (($d.file? | default null) == null) $d.path
                }
                assert (not (computed (documents | where section == "policy" | first)))
            }
        }
        {
            name: "a body holds the file's text exactly, in the one field"
            body: {
                let p = (documents | first)
                let body = (body_json $p (root) | from json)
                assert equal ($body | columns) [$p.field]
                assert equal ($body | get $p.field) (open --raw ((root) | path join $p.file))
            }
        }
        {
            name: "a body with quotes, backslashes and newlines survives"
            body: {
                let dir = (mktemp -d)
                "permit (principal == Identity::\"a\\\"b\", action, resource);\n// \\ done\n" | save ($dir | path join "p.cedar")
                let doc = { field: "source", file: "p.cedar" }
                let back = (body_json $doc $dir | from json)
                assert equal $back.source (open --raw ($dir | path join "p.cedar"))
            }
        }
        {
            name: "only what is not in the repository yet is seeded"
            body: {
                let docs = (documents)
                assert equal (missing $docs [] | length) ($docs | length)
                assert equal (missing $docs ($docs | get path)) []
                assert equal (missing $docs ["permanent/other.dhall"] | length) ($docs | length)
            }
        }
        {
            name: "git is given the user's own certificate, key and the CA, and told where the server's name lives"
            body: {
                let args = (git_config "alice" "/home/a/.config/polar-admin" | str join " ")
                assert ($args | str contains "http.sslCert=/home/a/.config/polar-admin/alice.crt")
                assert ($args | str contains "http.sslKey=/home/a/.config/polar-admin/alice.key")
                assert ($args | str contains "http.sslCAInfo=/home/a/.config/polar-admin/ca.pem")
                assert ($args | str contains "polar-git-server.polar.svc.cluster.local:8443:127.0.0.1")
            }
        }
        {
            name: "the repository is addressed by the cluster's own name for the server, which is on its certificate"
            body: { assert equal (repo_url) "https://polar-git-server.polar.svc.cluster.local:8443/polar-schedules.git" }
        }
        {
            name: "a refusal names the identity and the file the cluster's administrators are kept in"
            body: {
                let hint = (refused_hint "66b3f4c8-f024" "local-minimal")
                assert ($hint | str contains "66b3f4c8-f024")
                assert ($hint | str contains "infra/targets/local-minimal/cluster-inputs.nuon")
                assert ($hint | str contains "restarts the git server")
            }
        }
        {
            name: "an account is known to the git server by the first DNS name of its certificate"
            body: {
                let san = "X509v3 Subject Alternative Name:\n    DNS:66b3f4c8-f024-4207-b70b-9fadf7badc92, DNS:alice.example, email:alice@example.org"
                assert equal (first_dns_name $san) "66b3f4c8-f024-4207-b70b-9fadf7badc92"
                assert equal (first_dns_name "X509v3 Subject Alternative Name:\n    email:a@b.c") null
                assert equal (first_dns_name "") null
            }
        }
        {
            name: "a seed document is signed for the capability its section needs, and every one of them has one"
            body: {
                for d in (documents) { assert ($d.capability | str starts-with "c:") }
                assert equal (documents | where section == "policy" | first | get capability) "c:update-policy"
            }
        }
        {
            name: "a certificate without the capability is told so before anything is signed, one with it is not"
            body: {
                let without = "X509v3 Subject Alternative Name:\n    DNS:abc, URI:p:u2s:x, URI:c:provision-account, URI:c:grant"
                let with = "X509v3 Subject Alternative Name:\n    DNS:abc, URI:c:grant, URI:c:update-policy, URI:c:update-runtime"
                assert equal (lacks $without (documents)) ["c:update-policy" "c:update-runtime"]
                assert equal (lacks $with (documents)) []
                assert ((lacks_hint "alice" ["c:update-policy"]) | str contains "c:update-policy")
            }
        }
        {
            name: "the credentials agent having published the policy is read from its log, and not from another kind or a refusal"
            body: {
                let said = "2026-10-12T10:00:00Z  INFO credentials_agent_consumer::authority_publisher: publishing an authority update kind=\"policy\" version=1 sequence=1"
                assert (authority_published $said "policy")
                assert (not (authority_published $said "groups"))
                assert (not (authority_published "WARN not published: it would not replace what is held" "policy"))
                assert (not (authority_published "" "policy"))
                # As the agent really writes it: coloured, the field name in italics and the = dimmed.
                let esc = (char --integer 27)
                let coloured = $"($esc)[2m2026-10-12T20:50:53Z($esc)[0m ($esc)[32m INFO($esc)[0m publishing an authority update ($esc)[3mkind($esc)[0m($esc)[2m=($esc)[0m\"policy\" ($esc)[3mversion($esc)[0m($esc)[2m=($esc)[0m1"
                assert (authority_published $coloured "policy")
                assert (not (authority_published $coloured "groups"))
                assert (authority_published $"noise\n($said)\nmore noise" "policy")
            }
        }
    ]

    let results = ($tests | each {|t|
        let r = (try { do $t.body; null } catch {|e| $e.msg })
        { name: $t.name, error: $r }
    })
    for r in $results {
        if $r.error == null {
            print $"  ok    ($r.name)"
        } else {
            print -e $"  FAIL  ($r.name): ($r.error)"
        }
    }
    let failed = ($results | where error != null | length)
    if $failed > 0 {
        print -e $"($failed) of ($results | length) failed"
        exit 1
    }
    print $"all ($results | length) passed"
}
