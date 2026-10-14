#!/usr/bin/env nu
# infra/browser.nu
#
# Step 10 of up.nu: the browser holds this cluster's CA and the user's certificate, and the Neo4j password is
# the one the agents use. Both are looked at first, read-only, so the step stops for a person only for what is
# missing and says which. The decisions are pure and tested (infra/tests/browser.nu); the probes below only
# read.

# The name Neo4j's server certificate carries for the service (services.nuon), which the forward answers to.
use retry.nu [with_retries]

const NEO4J_HOST = "polar-db-svc.polar-graph.svc.cluster.local"

# Whether two fingerprints are the same one, whatever the case or the colons. Null is never the same as anything.
export def same_fingerprint [a: any, b: any] {
    if $a == null or $b == null { return false }
    let norm = {|x| $x | str replace --all ":" "" | str uppercase }
    (do $norm $a) == (do $norm $b)
}

# What is wrong with the browser's Polar certificates, as sentences; empty when there is nothing to do.
# `found` is `{ ca, serial }` read from the profile (null for what is not there), `expected` the same from the
# files this cluster's admin flow left (`ca.pem` and the user's certificate).
export def browser_problems [found: record, expected: record, user: string] {
    mut problems: list<string> = []
    let way = $"close Librewolf and run: just user-cert-install ($user)"
    if $found.ca == null {
        $problems = ($problems | append $"the browser has no Polar Internal CA; ($way)")
    } else if not (same_fingerprint $found.ca $expected.ca) {
        $problems = ($problems | append $"the browser trusts a 'Polar Internal CA' from another cluster \(SHA-256 ($found.ca), this cluster's is ($expected.ca)\); it reports this cluster's certificates as SEC_ERROR_BAD_SIGNATURE. ($way) \(it removes the old one first\)")
    }
    if $found.serial == null {
        $problems = ($problems | append $"the browser has no certificate for ($user); ($way)")
    } else if $found.serial != $expected.serial {
        $problems = ($problems | append $"the browser holds an older certificate for ($user) \(serial ($found.serial), the current one is ($expected.serial)\); ($way)")
    }
    $problems
}

# What one answer from Neo4j's query API means, by what it really says (observed on a Neo4j started with and without
# a password, not recalled from its documentation):
#   `ok`          200, and the query ran
#   `expired`     200 with Neo.ClientError.Security.CredentialsExpired in the body: the credentials are valid but
#                 must be changed before use, which is how a Neo4j still on its default password answers
#   `refused`     401: the credentials are wrong
#   `unreachable` no answer at all (0)
#   `unknown`     anything else
export def neo4j_answer [code: int, body: string] {
    if $code == 0 { return "unreachable" }
    if $code == 200 and ($body | str contains "CredentialsExpired") { return "expired" }
    if $code == 200 { return "ok" }
    if $code == 401 { return "refused" }
    "unknown"
}

# Whether Neo4j's password is the one the agents use, from two answers: `with_secret`, to the password in the
# polar-graph-pw Secret, and `with_default`, to the factory default neo4j/neo4j (null when it was not asked, which is
# only done when the first was refused). `set` (the Secret's password works), `default` (still on its factory
# password, so a first login has to change it), `different` (a password of its own: neither works, so nobody is told to
# set it again), `unreachable` or `unknown`.
export def password_verdict [with_secret: string, with_default: any] {
    match $with_secret {
        "ok" => "set"
        "expired" => "default"
        "unreachable" => "unreachable"
        "refused" => {
            match $with_default {
                "expired" => "default"
                "unreachable" => "unreachable"
                _ => "different"
            }
        }
        _ => "unknown"
    }
}

# ── Probes: they read, and change nothing ────────────────────────────────────

# The profile directory the install recipe would use: POLAR_NSS_DB, else the first with a cert9.db under
# ~/.librewolf or ~/.config/librewolf. Null when there is none.
export def nss_db [] {
    let explicit = ($env.POLAR_NSS_DB? | default "")
    if ($explicit | is-not-empty) { return $explicit }
    let hits = (["~/.librewolf" "~/.config/librewolf"]
        | each {|d| $d | path expand }
        | where {|d| $d | path exists }
        | each {|d| glob $"($d)/**/cert9.db" --depth 3 }
        | flatten)
    if ($hits | is-empty) { null } else { $hits | first | path dirname }
}

# What the browser profile holds: the SHA-256 fingerprint of its 'Polar Internal CA' and the serial of 'Polar <user>',
# each null when absent. Read with certutil (from nix), which works on a profile that is open.
export def browser_found [db: string, user: string] {
    let script = $"set -e; T=$\(mktemp -d); trap 'rm -rf $T' EXIT
certutil -L -n 'Polar Internal CA' -d sql:($db) -r > $T/ca.der 2>/dev/null && echo \"ca=$\(openssl x509 -inform DER -in $T/ca.der -noout -fingerprint -sha256 | cut -d= -f2\)\" || true
certutil -L -n 'Polar ($user)' -d sql:($db) -r > $T/u.der 2>/dev/null && echo \"serial=$\(openssl x509 -inform DER -in $T/u.der -noout -serial | cut -d= -f2\)\" || true"
    let r = (do { ^nix-shell -p nssTools openssl --run $script } | complete)
    let lines = ($r.stdout | lines)
    let pick = {|key| $lines | where {|l| $l | str starts-with $"($key)=" } | each {|l| $l | str replace $"($key)=" "" | str trim } | get -o 0 }
    { ca: (do $pick "ca"), serial: (do $pick "serial") }
}

# The same two facts from the files this cluster's admin flow left in `admin_dir`.
export def browser_expected [admin_dir: string, user: string] {
    let fp = (^openssl x509 -in ($admin_dir | path join "ca.pem") -noout -fingerprint -sha256 | str trim | split row "=" | last)
    let serial = (^openssl x509 -in ($admin_dir | path join $"($user).crt") -noout -serial | str trim | split row "=" | last)
    { ca: $fp, serial: $serial }
}

# Neo4j's answer, over its forwarded HTTPS port with the user's certificate, to a trivial query as `neo4j` with
# `password`. Addressed by the name its certificate carries (resolved to the forward), since the certificate names
# hostnames and no IP address. Returns `{ code, body }`; code 0 when nothing answers. The password goes to curl on its standard
# input, never on a command line.
export def neo4j_probe [admin_dir: string, user: string, password: string] {
    let out = (mktemp)
    let cfg = $"user = \"neo4j:($password)\"\n"
    let r = ($cfg | ^curl -s -m 8 -K - -o $out -w "%{http_code}" --cacert ($admin_dir | path join "ca.pem") --cert ($admin_dir | path join $"($user).crt") --key ($admin_dir | path join $"($user).key") --resolve $"($NEO4J_HOST):7473:127.0.0.1" -H "Content-Type: application/json" -d '{"statements":[{"statement":"RETURN 1"}]}' $"https://($NEO4J_HOST):7473/db/neo4j/tx/commit" | complete)
    let body = (try { open --raw $out } catch { "" })
    rm -f $out
    { code: (try { $r.stdout | str trim | into int } catch { 0 }), body: $body }
}

# Runs `probe` (which returns `{ code, body }`, as `neo4j_probe` does) until Neo4j answers at all, up to `attempts`
# times with `pause` between. Neo4j that was just started, or whose port-forward is reconnecting, answers nothing
# for a while; any answer, a refusal included, ends the wait, since asking again cannot change it. Returns the last
# `{ code, body }`, whose code is 0 if nothing ever answered.
export def probe_until_answered [attempts: int, pause: duration, probe: closure] {
    let r = (with_retries $attempts $pause {|n|
        let a = (do $probe)
        { ok: ((neo4j_answer $a.code $a.body) != "unreachable"), final: false, detail: ($a | to json --raw) }
    })
    $r.detail | from json
}
