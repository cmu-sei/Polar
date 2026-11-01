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
# The Jira check's pure parts: what it says of a URL and an address, how it reads nu's and openssl's answers, that its
# masking leaves no host, URL, email or address in a line, what the facts are taken to mean, and the pod it makes (no
# credential, nothing on a command line: since 3.0.0 the credential is jira-observer's signed section, which only its
# CryptoActor opens). Examples, and loops over generated cases. Every
# value is made up; nothing reaches a cluster or a Jira.
#
#   nu infra/tests/jira-check.nu

use std/assert
use ../jira-check.nu *

# Generated cases per loop.
const CASES = 200

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
        anonymous: false
        address: { class: "public", source: "the address openssl connected to" }
        tls_verify: "0 (ok)"
        endpoints: [
            { path: "/rest/api/2/serverInfo", status: 200, error: null, detail: null, anonymous: null, projects: null, deployment_type: "Cloud" }
            { path: "/rest/api/2/myself", status: 401, error: null, detail: null, anonymous: true, projects: null, deployment_type: null }
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

        "masking leaves no host, URL, email or address, whatever its case": {
            let s = { url: "https://jira.corp.test/", host: "jira.corp.test" }
            assert equal (mask "GET https://jira.corp.test//rest/api/2/myself" $s) "GET JIRA-URL/rest/api/2/myself"
            assert equal (mask "cannot resolve JIRA.CORP.TEST:443" $s) "cannot resolve JIRA-HOST:443"
            assert equal (mask "as jira-user@corp.test" $s) "as EMAIL"
            assert equal (mask "redirected to https://other.corp.test/x from 10.1.2.3 and 2600:1f18::9 by ops@corp.test" $s) "redirected to URL from IP and IP by EMAIL"
            # what the check prints is left alone
            for l in (report_lines (sound)) { assert equal (mask $l $s) $l $l }
        }

        "for any host, no masked line holds it": {
            for _ in 1..$CASES {
                let host = (random_host)
                let url = $"https://($host)(['' '/' '/jira' '/rest/api/2'] | get (random int 0..3))"
                let s = { url: $url, host: $host }
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
                }
            }
        }

        "the settings are the document's literal values": {
            let doc = {
                sections: [
                    { name: "builder", variables: [{ name: "POLAR_CAPABILITIES", value: "cassini" }] }
                    { name: "jira", variables: [
                        { name: "JIRA_URL", value: "https://site.atlassian.net" }
                        { name: "JIRA_ANONYMOUS", value: "true" }
                        { name: "JIRA_DEPLOYMENT", value: null }
                    ] }
                ]
            }
            assert equal (settings_of_document $doc) { url: "https://site.atlassian.net", deployment: "", anonymous: true }
            assert equal (settings_of_document {}) { url: "", deployment: "", anonymous: false }
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
            let f = (sound | update url (url_facts "https://example.atlassian.net") | with_statuses $in [404 404 404])
            let l = (interpret $f)
            assert (says $l "JIRA_URL is still the example placeholder") $"($l)"
            # the 404 is the placeholder's, not a wrong path
            assert (not (says $l "Jira answered 404"))
        }

        "a trailing slash and a /rest in the URL are called out": {
            assert (says (interpret (sound | update url (url_facts "https://site.atlassian.net/"))) "ends with '/'")
            assert (says (interpret (sound | update url (url_facts "https://site.atlassian.net/rest"))) "contains /rest")
        }

        "an anonymous caller is expected; reading anonymously, a 401 is what the site hides": {
            let l = (interpret (sound))
            assert (says $l "as it should: this check has no credential") $"($l)"
            assert (says $l "jira-observer's health")
            let anonymous = (interpret (sound | update anonymous true))
            assert (says $anonymous "does not let anonymous users in") $"($anonymous)"
        }

        "a site read with a credential over http is called out; read anonymously it is not": {
            let http = (sound | update url (url_facts "http://jira.corp.test"))
            assert (says (interpret $http) "sends a credential only over https") $"(interpret $http)"
            assert (not (says (interpret ($http | update anonymous true)) "only over https"))
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
            for k in ["DNS" "timeout" "TLS"] { assert (not (says (interpret (failing (sound) $k)) "The site is reached")) $k }
        }

        "a site reached says so, with the projects it shows the public": {
            assert (says (interpret (sound)) "The site is reached and answers: it shows the public 3 projects.")
            let none = (sound | update endpoints.2.projects 0)
            assert (says (interpret $none) "shows the public 0 projects")
            # an answer this check has no reading of is not taken for good
            assert equal (interpret (sound | update endpoints.1.anonymous null | with_statuses $in [200 500 200])) ["Nothing this check knows explains the answers above."]
        }

        "the facts are printed in full, nothing withheld or added": {
            let l = (fact_lines (sound))
            assert (says $l "reads      with its credential (the signed credentials section); asked here anonymously")
            assert (says $l "atlassian.net: yes, example placeholder: no")
            assert (says $l "address    public (the address openssl connected to)")
            assert (says $l "myself     HTTP 401, x-ausername: anonymous")
            assert (says $l "project    HTTP 200, 3 projects")
            assert (says $l "serverInfo HTTP 200, deploymentType Cloud")
            let dns = (fact_lines (failing (sound) "DNS" | update endpoints.0.detail "NoName"))
            assert (says $dns "serverInfo DNS (NoName)")
            assert (says $dns "myself     not tried")
        }

        "the pod is given no credential and nothing on a command line": {
            let settings = { url: "https://site.atlassian.net", deployment: "", anonymous: false }
            let placement = { nodeSelector: { "polar.dev/pool": "observers" }, tolerations: [{ key: "polar.dev/pool", operator: "Equal", value: "observers", effect: "NoSchedule" }] }
            let pod = (check_pod "jira-check-abcdef" "localhost:30500/polar-nu-init:latest@sha256:00" $settings $placement)
            let c = ($pod.spec.containers | first)
            assert ($c.env | all {|e| $e.valueFrom? == null }) "nothing from a Secret"
            assert equal ($c.env | where name == "JIRA_TOKEN") []
            assert equal ($c.env | where name == "JIRA_ANONYMOUS" | first | get value) "false"
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
