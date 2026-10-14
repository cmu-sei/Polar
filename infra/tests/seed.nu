#!/usr/bin/env nu
# infra/tests/seed.nu
#
# Tests of the decisions in infra/seed.nu.
#
#   nu infra/tests/seed.nu

use std/assert
use ../seed.nu *

def root [] { $env.FILE_PWD | path dirname | path dirname }

def main [] {
    let tests = [
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
                    assert (((root) | path join $d.file) | path exists) $"($d.file) is missing"
                }
            }
        }
        {
            name: "the first policy is seeded for the credentials agent's policy section"
            body: {
                let p = (documents | where section == "policy" | first)
                assert equal $p.agent "credentials-agent-consumer"
                assert equal $p.field "source"
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
            name: "a refusal names the identity and the file to change"
            body: {
                let hint = (refused_hint "66b3f4c8-f024" "local-minimal")
                assert ($hint | str contains "66b3f4c8-f024")
                assert ($hint | str contains "POLAR_GIT_SERVER_ADMINS")
                assert ($hint | str contains "infra/targets/local-minimal/config.nuon")
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
                assert equal (lacks $without (documents)) ["c:update-policy"]
                assert equal (lacks $with (documents)) []
                assert ((lacks_hint "djshepard" ["c:update-policy"]) | str contains "c:update-policy")
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
