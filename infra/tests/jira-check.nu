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

# infra/tests/jira-check.nu
#
# The Jira check's pure parts: what it says of a URL, a token and an address, how it reads nu's and openssl's answers,
# that its masking leaves no host, URL, email, address or token in a line, what the facts are taken to mean, and the
# pod it makes (the token from the Secret, nothing on a command line). Examples, and loops over generated cases. Every
# value is made up; nothing reaches a cluster or a Jira.
#
#   nu infra/tests/jira-check.nu

use std/assert
use ../jira-check.nu *

# Generated cases per loop.
const CASES = 200

# A random printable ASCII string (33..126, no space) of `n` characters.
def printable [n: int] { 1..$n | each { random int 33..126 | char --integer $in } | str join }

# A random host: two to four lowercase labels and a top-level name, at least one dot.
def random_host [] {
    let labels = (1..(random int 2..4) | each { random chars --length (random int 1..12) | str lowercase })
    $labels | append (["net" "com" "org" "test" "atlassian.net"] | get (random int 0..4)) | str join "."
}

# An IPv4 address in /proc/net/tcp's form: one little-endian word.
def proc_hex [octets: list<int>] { $octets | reverse | each {|o| $o | format number | get lowerhex | str replace "0x" "" | fill -a r -c "0" -w 2 } | str join | str uppercase }

# The facts the pod gathers, for a sound site; each test changes what it is about.
def sound [] {
    {
        nu: "0.115.1"
        url: (url_facts "https://site.atlassian.net")
        deployment: (deployment_fact "")
        auth: "Basic"
        email_placeholder: false
        token: (token_facts "ATATT3xFfGF0-made_up-token")
        address: { class: "public", source: "the address openssl connected to" }
        tls_verify: "0 (ok)"
        endpoints: [
            { path: "/rest/api/2/serverInfo", status: 200, error: null, detail: null, anonymous: null, projects: null, deployment_type: "Cloud" }
            { path: "/rest/api/2/myself", status: 200, error: null, detail: null, anonymous: false, projects: null, deployment_type: null }
            { path: "/rest/api/2/project", status: 200, error: null, detail: null, anonymous: null, projects: 3, deployment_type: null }
        ]
    }
}

def with_statuses [facts: record, statuses: list] {
    $facts | update endpoints ($facts.endpoints | enumerate | each {|e| $e.item | update status ($statuses | get $e.index) })
}

def failing [facts: record, kind: string] {
    $facts | update endpoints ($facts.endpoints | enumerate | each {|e|
        $e.item | update status null | update error (if $e.index == 0 { $kind } else { "not tried" }) | update projects null | update anonymous null | update deployment_type null
    })
}

def says [lines: list, needle: string] { $lines | any {|l| $l | str contains $needle } }

def main [] {
    let tests = {
        "the URL's shape is told without the URL": {
            let p = (url_facts "https://example.atlassian.net")
            assert equal $p { set: true, https: true, trailing_slash: false, rest: false, atlassian: true, placeholder: true }
            let s = (url_facts "https://jira.corp.test/")
            assert equal [$s.trailing_slash $s.atlassian $s.placeholder] [true false false]
            assert (url_facts "https://site.atlassian.net/rest/api/2").rest
            assert (not (url_facts "http://jira.corp.test").https)
            assert (not (url_facts "").set)
            # a URL with no scheme has no host to judge
            let bare = (url_facts "site.atlassian.net")
            assert equal [$bare.set $bare.https $bare.atlassian] [true false false]
            assert (not (url_facts "https://notatlassian.net").atlassian)
            for h in ["example.org" "jira.example.com" "x.example" "EXAMPLE.ATLASSIAN.NET"] { assert (is_example_host $h) $h }
            for h in ["examples.org" "example.org.corp.test" "site.atlassian.net"] { assert (not (is_example_host $h)) $h }
            assert equal (url_port "https://jira.corp.test:8443/jira") 8443
            assert equal (url_port "http://jira.corp.test") 80
            assert equal (url_port "https://jira.corp.test") 443
        }

        "the auth mode is the agent's: Basic with an email, Bearer without": {
            assert equal (auth_mode "") "Bearer"
            assert equal (auth_mode "jira-user@corp.test") "Basic"
        }

        "a token's length and whitespace are told, not the token": {
            assert equal (token_facts "abc def") { length: 7, whitespace: true, base64: false }
            assert equal (token_facts "abcdef\n").whitespace true
            assert equal (token_facts "") { length: 0, whitespace: false, base64: false }
        }

        "base64 of text is flagged, a raw token is not": {
            assert (looks_base64 ("jira-user@corp.test:made-up-token" | encode base64))
            # `echo token | base64` leaves a newline inside
            assert (looks_base64 ("made-up-token-123\n" | encode base64))
            # a raw Atlassian token has - and _, outside the alphabet
            assert (not (looks_base64 "ATATT3xFfGF0-made_up-token-T8sH4="))
            # too short to say
            assert (not (looks_base64 ("abcdefghi" | encode base64)))
            # not whole quanta
            assert (not (looks_base64 "YWJjZGVmZ2hpamtsbW5vcA"))
            # space or a tab inside the decoded text is not a token
            assert (not (looks_base64 ("made up token 12345" | encode base64)))
        }

        "for any printable text, its base64 is flagged": {
            for _ in 1..$CASES {
                let s = (printable (random int 12..60))
                assert (looks_base64 ($s | encode base64)) $"base64 of a printable string of ($s | str length)"
            }
        }

        "random bytes, base64-encoded, are not flagged": {
            for _ in 1..$CASES {
                let b = (random binary 33 | encode base64)
                assert (not (looks_base64 $b)) "33 random bytes"
            }
        }

        "an address is classed, never printed": {
            let cases = [
                ["100.64.0.1" "ZPA-style"] ["100.127.255.255" "ZPA-style"] ["100.63.255.255" "public"] ["100.128.0.1" "public"]
                ["10.0.0.1" "private"] ["172.16.0.1" "private"] ["172.31.255.255" "private"] ["172.32.0.1" "public"]
                ["192.168.1.1" "private"] ["8.8.8.8" "public"] ["127.0.0.1" "loopback"] ["169.254.1.1" "link-local"]
                ["::1" "loopback"] ["0:0:0:0:0:0:0:1" "loopback"] ["fd00::1" "private"] ["fe80::1" "link-local"]
                ["2600:1f18::1" "public"] ["::ffff:10.0.0.1" "private"] ["[2600::1]" "public"] ["300.1.1.1" "unknown"]
                ["not an address" "unknown"] ["" "no connection"]
            ]
            for c in $cases { assert equal (address_class $c.0) $c.1 $c.0 }
            assert equal (address_class null) "no connection"
            for _ in 1..$CASES {
                assert equal (address_class $"100.(random int 64..127).(random int 0..255).(random int 0..255)") "ZPA-style"
                assert equal (address_class $"10.(random int 0..255).(random int 0..255).(random int 0..255)") "private"
                assert equal (address_class $"172.(random int 16..31).(random int 0..255).(random int 0..255)") "private"
            }
        }

        "/proc/net/tcp gives the address a connection goes to, in either listing": {
            let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"
            let v4 = ([
                $header
                "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000 65532 0 1 1"
                "   1: 0500000A:A1B2 0302010A:01BB 02 00000001:00000000 01:00000064 00000002 65532 0 2 1"
                "   2: 0500000A:A1B3 0100007F:0050 01 00000000:00000000 00:00000000 00000000 65532 0 3 1"
            ] | str join "\n")
            assert equal (proc_net_remotes $v4 443) ["10.1.2.3"]
            assert equal (proc_net_remotes $v4 80) ["127.0.0.1"]
            assert equal (proc_net_remotes $v4 8080) []
            let v6 = ([
                $header
                "   0: 00000000000000000000000000000000:A1B2 0000000000000000FFFF00000302010A:01BB 02 0 0 0 0 0 0 0"
                "   1: 00000000000000000000000000000000:A1B3 000000FD000000000000000001000000:01BB 06 0 0 0 0 0 0 0"
            ] | str join "\n")
            assert equal (proc_net_remotes $v6 443) ["10.1.2.3" "fd00:0:0:0:0:0:0:1"]
            assert equal (hex_address "000080FE000000000000000001000000") "fe80:0:0:0:0:0:0:1"
            for _ in 1..$CASES {
                let o = [(random int 0..255) (random int 0..255) (random int 0..255) (random int 0..255)]
                assert equal (hex_address (proc_hex $o)) ($o | each { into string } | str join ".")
            }
        }

        "openssl's answer gives the address it connected to and its verdict on the certificate": {
            let ok = "Connecting to 203.0.113.9\nCONNECTED(00000003)\ndepth=2 C=US\n---\nVerify return code: 0 (ok)\n"
            assert equal (openssl_address $ok) "203.0.113.9"
            assert equal (openssl_verify $ok) "0 (ok)"
            let bad = "Connecting to 100.64.1.2\nCONNECTED(00000003)\nVerify return code: 20 (unable to get local issuer certificate)\n"
            assert equal (openssl_verify $bad) "20 (unable to get local issuer certificate)"
            let refused = "4057:error:8000006F:system library:BIO_connect:Connection refused:crypto/bio/bio_sock2.c:183:calling connect()\nconnect:errno=111\n"
            assert equal (openssl_address $refused) null
            assert equal (openssl_verify $refused) null
        }

        "a failed request is told by its kind and one word, as nu reports it": {
            # nu 0.115.1's errors, the host replaced
            let cases = [
                ["DNS Error" "Network(Dns(DnsError { kind: NoName, span: Span[1..2], query: Spanned { item: \"https://jira.corp.test/rest/api/2/serverInfo\" } }))" "DNS" "NoName"]
                ["I/O error" "Io(IoError { kind: Std(ConnectionRefused, Sealed), span: Span[1..2], path: None })" "connection refused" "ConnectionRefused"]
                ["I/O error" "Io(IoError { kind: Std(TimedOut, Sealed), span: Span[1..2], path: None })" "timeout" "TimedOut"]
                ["I/O error" "Io(IoError { kind: Std(InvalidData, Sealed), span: Span[1..2], path: None })" "TLS" "InvalidData"]
                ["I/O error" "Io(IoError { kind: Std(HostUnreachable, Sealed) })" "unreachable" "HostUnreachable"]
                ["something new" "Other" "other" null]
            ]
            for c in $cases {
                assert equal (error_kind $c.0 $c.1) $c.2 $c.0
                assert equal (error_detail $c.1) $c.3 $c.1
            }
        }

        "masking leaves no host, URL, email, address or token, whatever its case": {
            let s = { url: "https://jira.corp.test/", host: "jira.corp.test", email: "jira-user@corp.test", token: "made-up-token-0123", credential: ("jira-user@corp.test:made-up-token-0123" | encode base64) }
            assert equal (mask "GET https://jira.corp.test//rest/api/2/myself" $s) "GET JIRA-URL/rest/api/2/myself"
            assert equal (mask "cannot resolve JIRA.CORP.TEST:443" $s) "cannot resolve JIRA-HOST:443"
            assert equal (mask "as jira-user@corp.test with made-up-token-0123" $s) "as JIRA-EMAIL with JIRA-TOKEN"
            assert equal (mask $"Basic ($s.credential)" $s) "Basic JIRA-TOKEN"
            assert equal (mask "redirected to https://other.corp.test/x from 10.1.2.3 and 2600:1f18::9 by ops@corp.test" $s) "redirected to URL from IP and IP by EMAIL"
            # what the check prints is left alone
            for l in (report_lines (sound)) { assert equal (mask $l $s) $l $l }
        }

        "for any host, no masked line holds it": {
            for _ in 1..$CASES {
                let host = (random_host)
                let url = $"https://($host)(['' '/' '/jira' '/rest/api/2'] | get (random int 0..3))"
                let s = { url: $url, host: $host, email: $"someone@($host)", token: (printable 24), credential: "" }
                let lines = [
                    $url
                    $"GET ($url)/rest/api/2/myself"
                    $"($host):443 refused"
                    $"cannot resolve ($host | str uppercase)"
                    $"Connecting to ($host) \(failed\)"
                    $"https://($host | str uppercase)/x"
                    $"ask someone@($host)"
                    ...(report_lines (sound | update tls_verify $"0 \(($host)\)"))
                ]
                for l in $lines {
                    let m = (mask $l $s)
                    assert (not ($m | str lowercase | str contains $host)) $"($host) left in: ($m)"
                    assert (not ($m | str contains $s.token)) "the token left in a line"
                }
            }
        }

        # On u7c the placeholder token is a word the check says: masked, it showed where it was.
        "a token that is a word the check says is not masked into view": {
            let facts = (sound | update url (url_facts "https://example.atlassian.net") | update email_placeholder true)
            let s = { url: "https://example.atlassian.net", host: "example.atlassian.net", email: "jira-user@example.org", token: "placeholder", credential: ("jira-user@example.org:placeholder" | encode base64) }
            let lines = (printed $facts $s)
            assert (says $lines "example placeholder: yes") $"($lines)"
            assert (not (says $lines "JIRA-TOKEN"))
            assert equal $lines (report_lines $facts)
            # and the host, the URL and the email still are
            let leaky = (printed ($facts | update tls_verify "0 (https://example.atlassian.net jira-user@example.org)") $s)
            assert (says $leaky "0 (JIRA-URL JIRA-EMAIL)") $"($leaky)"
        }

        "the settings are the document's literal values": {
            let doc = {
                sections: [
                    { name: "builder", variables: [{ name: "POLAR_CAPABILITIES", value: "cassini" }] }
                    { name: "jira", variables: [
                        { name: "JIRA_URL", value: "https://site.atlassian.net" }
                        { name: "JIRA_TOKEN", value: { env: "JIRA_TOKEN" } }
                        { name: "JIRA_EMAIL", value: "jira-user@corp.test" }
                        { name: "JIRA_DEPLOYMENT", value: null }
                    ] }
                ]
            }
            assert equal (settings_of_document $doc) { url: "https://site.atlassian.net", email: "jira-user@corp.test", deployment: "" }
            assert equal (settings_of_document {}) { url: "", email: "", deployment: "" }
        }

        "an endpoint's answer: its status, whether the caller was anonymous, how many projects": {
            let header = {|name, value| { request: [], response: [{ name: $name, value: $value }] } }
            let me = (endpoint_facts "/rest/api/2/myself" { status: 200, headers: (do $header "X-AUSERNAME" "anonymous"), body: {} })
            assert equal [$me.status $me.anonymous] [200 true]
            let named = (endpoint_facts "/rest/api/2/myself" { status: 200, headers: (do $header "x-ausername" "a-made-up-user"), body: {} })
            assert equal $named.anonymous false
            let none = (endpoint_facts "/rest/api/2/myself" { status: 401, headers: (do $header "server" "x"), body: "" })
            assert equal [$none.status $none.anonymous] [401 null]
            let projects = (endpoint_facts "/rest/api/2/project" { status: 200, headers: (do $header "server" "x"), body: [{ key: "A" } { key: "B" } { key: "C" }] })
            assert equal $projects.projects 3
            let text = (endpoint_facts "/rest/api/2/project" { status: 200, headers: (do $header "server" "x"), body: "[{\"key\":\"A\"}]" })
            assert equal $text.projects 1
            let refused = (endpoint_facts "/rest/api/2/project" { status: 401, headers: (do $header "server" "x"), body: [] })
            assert equal $refused.projects null
            let info = (endpoint_facts "/rest/api/2/serverInfo" { status: 200, headers: (do $header "server" "x"), body: { deploymentType: "Cloud", baseUrl: "https://site.atlassian.net" } })
            assert equal $info.deployment_type "Cloud"
            assert (not (fact_lines (sound | update endpoints [$info]) | str join "\n" | str contains "site.atlassian.net"))
        }

        "the placeholder URL is called out": {
            let f = (sound | update url (url_facts "https://example.atlassian.net") | update email_placeholder true | with_statuses $in [404 404 404])
            let l = (interpret $f)
            assert (says $l "JIRA_URL is still the example placeholder") $"($l)"
            assert (says $l "JIRA_EMAIL is still the example placeholder")
            # the 404 is the placeholder's, not a wrong path
            assert (not (says $l "Jira answered 404"))
        }

        "a trailing slash and a /rest in the URL are called out": {
            assert (says (interpret (sound | update url (url_facts "https://site.atlassian.net/"))) "ends with '/'")
            assert (says (interpret (sound | update url (url_facts "https://site.atlassian.net/rest"))) "contains /rest")
        }

        "Basic on a site that is not atlassian.net, and Bearer on one that is, are called out": {
            let server = (sound | update url (url_facts "https://jira.corp.test"))
            assert (says (interpret $server) "unset JIRA_EMAIL (Bearer)")
            assert (not (says (interpret ($server | update auth "Bearer")) "unset JIRA_EMAIL"))
            assert (says (interpret (sound | update auth "Bearer")) "Set JIRA_EMAIL")
        }

        "a 401, or an anonymous caller, is a token Jira did not accept": {
            let refused = (interpret (sound | with_statuses $in [200 401 401]))
            assert (says $refused "did not accept the token") $"($refused)"
            assert (says $refused "base64")
            let anonymous = (sound | update endpoints.1.anonymous true)
            assert (says (interpret $anonymous) "did not accept the token")
            assert (not (says (interpret $anonymous) "All good"))
        }

        "a token with whitespace, or encoded, or empty, is called out": {
            assert (says (interpret (sound | update token (token_facts "made-up-token\n"))) "whitespace")
            assert (says (interpret (sound | update token (token_facts ("jira-user@corp.test:made-up" | encode base64)))) "decodes as base64")
            assert (says (interpret (sound | update token (token_facts ""))) "token is empty")
        }

        "DNS, connection and TLS failures are told apart": {
            let dns = (interpret (failing (sound) "DNS" | update address { class: "no connection", source: "openssl made none" }))
            assert (says $dns "cannot resolve JIRA-HOST") $"($dns)"
            assert (not (says $dns "could not connect"))
            let zpa = (interpret (failing (sound) "timeout" | update address { class: "ZPA-style", source: "the address a connection was attempted to, which did not complete" }))
            assert (says $zpa "could not connect (timeout)") $"($zpa)"
            assert (says $zpa "the node must reach ZPA's connector")
            assert (says (interpret (failing (sound) "connection refused")) "could not connect (connection refused)")
            assert (says (interpret (failing (sound) "TLS")) "TLS failed")
            for k in ["DNS" "timeout" "TLS"] { assert (not (says (interpret (failing (sound) $k)) "All good")) $k }
        }

        "all good says so, with the projects the account sees": {
            assert equal (interpret (sound)) ["All good: the address, the token and the auth mode are accepted, and the account sees 3 projects."]
            let none = (sound | update endpoints.2.projects 0)
            assert (says (interpret $none) "sees no projects")
            # an answer this check has no reading of is not taken for good
            assert equal (interpret (sound | with_statuses $in [200 500 200])) ["Nothing this check knows explains the answers above."]
        }

        "the facts are printed in full, nothing withheld or added": {
            let l = (fact_lines (sound))
            assert (says $l "length 26, whitespace: no, looks base64-encoded: no")
            assert (says $l "atlassian.net: yes, example placeholder: no")
            assert (says $l "address    public (the address openssl connected to)")
            assert (says $l "myself     HTTP 200, x-ausername: a user")
            assert (says $l "project    HTTP 200, 3 projects")
            assert (says $l "serverInfo HTTP 200, deploymentType Cloud")
            let dns = (fact_lines (failing (sound) "DNS" | update endpoints.0.detail "NoName"))
            assert (says $dns "serverInfo DNS (NoName)")
            assert (says $dns "myself     not tried")
        }

        "the pod is given the token from the Secret and nothing on a command line": {
            let settings = { url: "https://site.atlassian.net", email: "jira-user@corp.test", deployment: "" }
            let placement = { nodeSelector: { "polar.dev/pool": "observers" }, tolerations: [{ key: "polar.dev/pool", operator: "Equal", value: "observers", effect: "NoSchedule" }] }
            let pod = (check_pod "jira-check-abcdef" "localhost:30500/polar-nu-init:latest@sha256:00" $settings { name: "jira-secret", key: "token" } $placement)
            let c = ($pod.spec.containers | first)
            assert equal ($c.env | where name == "JIRA_TOKEN" | first | get valueFrom.secretKeyRef) { name: "jira-secret", key: "token" }
            assert equal ($c.env | where name == "JIRA_TOKEN" | first | get -o value) null
            assert equal ($c.env | where name == "JIRA_URL" | first | get value) $settings.url
            assert equal ($c.command? | default null) null
            assert equal ($c.args? | default null) null
            assert equal $pod.spec.nodeSelector $placement.nodeSelector
            assert equal $pod.spec.tolerations $placement.tolerations
            assert equal $pod.spec.restartPolicy "Never"
            assert ($pod.spec.activeDeadlineSeconds > 0)
            assert equal $pod.spec.automountServiceAccountToken false
            assert equal $pod.metadata.namespace "polar"
            assert equal ($pod.spec.volumes | where name == "scripts" | first | get configMap.name) "jira-check-abcdef"
            let map = (scripts_config_map "jira-check-abcdef" "the script")
            assert equal ($map.data | columns | sort) ["init.nu" "jira-check.nu"]
            assert ($map.data."init.nu" | str contains "probe")
        }

        "the image is the nu-init one the cluster runs, by the reference it runs it under": {
            let items = [
                { spec: { template: { spec: { containers: [{ image: "localhost:30500/jira-observer:latest@sha256:11" }] } } } }
                { spec: { template: { spec: { initContainers: [{ image: "localhost:30500/polar-init:latest@sha256:22" } { image: "localhost:30500/polar-nu-init:latest@sha256:33" }], containers: [{ image: "neo4j:5" }] } } } }
            ]
            assert equal (nu_image_of $items) "localhost:30500/polar-nu-init:latest@sha256:33"
            assert equal (nu_image_of [{ spec: { template: { spec: { containers: [{ image: "polar-nu-init:latest" }] } } } }]) "polar-nu-init:latest"
            assert equal (nu_image_of ($items | first 1)) null
            assert equal (nu_image_of [{ spec: { template: { spec: { containers: [{ image: "r/polar-nu-initx:1" }] } } } }]) null
            assert equal (nu_image_of [{ spec: { template: { spec: { containers: [{ image: "r/not-polar-nu-init:1" }] } } } }]) null
        }
    }

    let results = ($tests | transpose name body | each {|t|
        try {
            do $t.body
            { name: $t.name, error: null }
        } catch {|e|
            { name: $t.name, error: $e.msg }
        }
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
