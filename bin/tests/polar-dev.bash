#!/usr/bin/env bash
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

# bin/polar-dev's pure parts: reading ssh's and dropbear's refusals into a cause and a hint, the key types a local ssh
# policy accepts, whether `up` is about to replace the pod's image, and the git server a clone was refused by. Nothing
# here talks to a cluster or a network. The fixtures are the messages a Mac under a FIPS ssh policy got from the pod
# (2026-10-29), and the same refusals captured from a local dropbear 2026.94 and OpenSSH 10.5.
#
#   bash bin/tests/polar-dev.bash          (POLAR_DEV_SCRIPT=<file> tests another copy of the script)
#
# Written, as bin/polar-dev is, for the bash a Mac has (3.2) as well as a newer one.

HERE_TESTS=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=bin/polar-dev
source "${POLAR_DEV_SCRIPT:-$HERE_TESTS/../polar-dev}"
set +e

failures=0
passed=0
pass() { passed=$((passed + 1)); }
fail() { failures=$((failures + 1)); echo "FAIL: $1"; [ -n "${2:-}" ] && printf '  %s\n' "$2"; }

# The value is what was expected.
is() { if [ "$2" = "$3" ]; then pass; else fail "$1" "expected [$3], got [$2]"; fi; }
# The text has the part in it.
has() { case "$2" in *"$3"*) pass ;; *) fail "$1" "[$3] is not in: $2" ;; esac; }
# The text does not have the part in it.
lacks() { case "$2" in *"$3"*) fail "$1" "[$3] should not be in: $2" ;; *) pass ;; esac; }

# ── Fixtures ────────────────────────────────────────────────────────────────

# What the Mac's ssh said (2026-10-29), in the shape ssh -v prints it.
FIPS_FILES='debug1: Reading configuration data /Users/pat/.ssh/config
debug1: Reading configuration data /etc/ssh/ssh_config
debug1: /etc/ssh/ssh_config line 4: Including file /etc/ssh/ssh_config.d/fips_ssh_config depth 0
debug1: Reading configuration data /etc/ssh/ssh_config.d/fips_ssh_config
debug1: Reading configuration data /etc/ssh/ssh_config.d/fips_ssh_config'
HOSTKEY="$FIPS_FILES
debug1: Connecting to 127.0.0.1 [127.0.0.1] port 2222.
debug1: Connection established.
debug1: Remote protocol version 2.0, remote software version dropbear_2025.88
debug1: kex: host key algorithm: (no match)
Unable to negotiate with 127.0.0.1 port 2222: no matching host key type found. Their offer: ssh-ed25519,rsa-sha2-256"
CIPHER="$FIPS_FILES
debug1: kex: host key algorithm: ecdsa-sha2-nistp256
Unable to negotiate with 127.0.0.1 port 2222: no matching cipher found. Their offer: chacha20-poly1305@openssh.com,aes256-ctr,aes128-ctr"
# The same refusals from OpenSSH 10.5 against dropbear 2026.94.
KEX='debug1: kex: algorithm: (no match)
Unable to negotiate with 127.0.0.1 port 22999: no matching key exchange method found. Their offer: sntrup761x25519-sha512,curve25519-sha256,ecdh-sha2-nistp256,kexguess2@matt.ucc.asn.au'
MAC='Unable to negotiate with 127.0.0.1 port 2222: no matching MAC found. Their offer: hmac-sha2-256,hmac-sha1'
PUBKEY='debug1: Authentications that can continue: publickey
debug1: Will attempt key: /Users/pat/.ssh/id_ed25519 ED25519 SHA256:AAAAexampleED25519fingerprintAAAAAAAAAAAAAA agent
debug1: Skipping ssh-ed25519 key /Users/pat/.ssh/id_ed25519 - corresponding algorithm not in PubkeyAcceptedAlgorithms
debug1: No more authentication methods to try.
pat@127.0.0.1: Permission denied (publickey).'
REFUSED='debug1: connect to address 127.0.0.1 port 2222: Connection refused
ssh: connect to host 127.0.0.1 port 2222: Connection refused'
CLOSED='debug1: Connection established.
kex_exchange_identification: Connection closed by remote host
Connection closed by 127.0.0.1 port 2222'
OK='debug1: Authenticated to 127.0.0.1 ([127.0.0.1]:2222) using "publickey".
debug1: Exit status 0'
# The pod's log: start.nu's, then dropbear's (-E), as kubectl logs shows it.
POD_LOG='  • Headless ......... vigild is PID 1; log in over SSH
[41] Oct 29 14:02:10 Child connection from 127.0.0.1:56180
[41] Oct 29 14:02:10 Exit before auth from <127.0.0.1:56180>: No matching algo enc c->s
[52] Oct 29 14:02:11 Child connection from 127.0.0.1:56188
[52] Oct 29 14:02:11 Exit before auth from <127.0.0.1:56188>: No matching algo hostkey'
# The clone's refusal (2026-10-29).
CLONE="Cloning into '/workspace'...
git@52.235.181.217: Permission denied (publickey).
fatal: Could not read from remote repository.

Please make sure you have the correct access rights
and the repository exists."
# ssh -G under a FIPS policy: no ed25519, no chacha20, no ctr.
FIPS_G='user pat
port 2222
ciphers aes256-gcm@openssh.com,aes128-gcm@openssh.com
macs hmac-sha2-256-etm@openssh.com,hmac-sha2-512-etm@openssh.com,hmac-sha2-256,hmac-sha2-512
kexalgorithms ecdh-sha2-nistp256,ecdh-sha2-nistp384,ecdh-sha2-nistp521
hostkeyalgorithms ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,rsa-sha2-256,rsa-sha2-512
pubkeyacceptedalgorithms ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,rsa-sha2-256,rsa-sha2-512'
AGENT_LIST='256 SHA256:AAAAexampleED25519fingerprintAAAAAAAAAAAAAA pat@mac (ED25519)
3072 SHA256:BBBBexampleRSAfingerprintBBBBBBBBBBBBBBBBBBB pat@old (RSA)'

# ── ssh's and dropbear's refusals, read into a cause ────────────────────────

is "ssh's host key refusal is a host key cause" "$(printf '%s\n' "$HOSTKEY" | ssh_cause)" hostkey
is "ssh's cipher refusal is a cipher cause" "$(printf '%s\n' "$CIPHER" | ssh_cause)" cipher
is "ssh's key exchange refusal is a kex cause" "$(printf '%s\n' "$KEX" | ssh_cause)" kex
is "ssh's MAC refusal is a mac cause" "$(printf '%s\n' "$MAC" | ssh_cause)" mac
is "a denied key is a publickey cause" "$(printf '%s\n' "$PUBKEY" | ssh_cause)" publickey
is "git's denied key is a publickey cause" "$(printf '%s\n' "$CLONE" | ssh_cause)" publickey
is "a refused connection" "$(printf '%s\n' "$REFUSED" | ssh_cause)" refused
is "a connection the far end closed" "$(printf '%s\n' "$CLOSED" | ssh_cause)" closed
is "a login that worked" "$(printf '%s\n' "$OK" | ssh_cause)" ok
is "nothing known is unknown" "$(printf 'debug1: something else\n' | ssh_cause)" unknown
is "nothing at all is unknown" "$(printf '' | ssh_cause)" unknown
is "dropbear's host key refusal is a host key cause" \
    "$(printf '%s\n' '[52] Oct 29 14:02:11 Exit before auth from <127.0.0.1:56188>: No matching algo hostkey' | ssh_cause)" hostkey
is "dropbear's cipher refusal is a cipher cause" \
    "$(printf '%s\n' 'Exit before auth from <127.0.0.1:42910>: No matching algo enc c->s' | ssh_cause)" cipher
is "dropbear's kex refusal is a kex cause" "$(printf '%s\n' 'Exit before auth from <x>: No matching algo kex' | ssh_cause)" kex
is "dropbear's mac refusal is a mac cause" "$(printf '%s\n' 'Exit before auth from <x>: No matching algo mac s->c' | ssh_cause)" mac
is "the last refusal is the cause" "$(printf '%s\n' "$POD_LOG" | ssh_cause)" hostkey

is "the refusal line, as ssh said it" "$(printf '%s\n' "$HOSTKEY" | ssh_reason)" \
    "Unable to negotiate with 127.0.0.1 port 2222: no matching host key type found. Their offer: ssh-ed25519,rsa-sha2-256"
is "the denial line" "$(printf '%s\n' "$PUBKEY" | ssh_reason)" "pat@127.0.0.1: Permission denied (publickey)."
is "the refused line, without debug1" "$(printf '%s\n' "$REFUSED" | ssh_reason)" "ssh: connect to host 127.0.0.1 port 2222: Connection refused"
is "no refusal line" "$(printf 'debug1: hello\n' | ssh_reason)" ""
is "a refusal ssh said only in debug1, without the prefix" \
    "$(printf 'debug1: connect to address 127.0.0.1 port 2222: Connection refused\n' | ssh_reason)" \
    "connect to address 127.0.0.1 port 2222: Connection refused"

is "the host keys the pod offers" "$(printf '%s\n' "$HOSTKEY" | ssh_offer)" "ssh-ed25519,rsa-sha2-256"
is "the ciphers the pod offers" "$(printf '%s\n' "$CIPHER" | ssh_offer)" "chacha20-poly1305@openssh.com,aes256-ctr,aes128-ctr"
is "no offer in a denial" "$(printf '%s\n' "$PUBKEY" | ssh_offer)" ""

is "the policy files ssh read, each once, in order" "$(printf '%s\n' "$HOSTKEY" | ssh_config_files | paste -s -d' ' -)" \
    "/Users/pat/.ssh/config /etc/ssh/ssh_config /etc/ssh/ssh_config.d/fips_ssh_config"
is "a field of ssh -G" "$(printf '%s\n' "$FIPS_G" | ssh_g_field ciphers)" "aes256-gcm@openssh.com,aes128-gcm@openssh.com"
is "an older ssh's name for the accepted key types" \
    "$(printf 'pubkeyacceptedkeytypes ecdsa-sha2-nistp256\n' | ssh_g_field pubkeyacceptedalgorithms)" "ecdsa-sha2-nistp256"
is "a field ssh -G does not print" "$(printf '%s\n' "$FIPS_G" | ssh_g_field nosuchfield)" ""
is "a field given twice: the first" "$(printf 'ciphers a\nciphers b\n' | ssh_g_field ciphers)" a
is "the ssh -G field of a host key refusal" "$(ssh_g_name hostkey)" hostkeyalgorithms
is "the ssh -G field of a cipher refusal" "$(ssh_g_name cipher)" ciphers
is "the ssh -G field of a kex refusal" "$(ssh_g_name kex)" kexalgorithms
is "the ssh -G field of a MAC refusal" "$(ssh_g_name mac)" macs
is "the ssh -G field ssh_g_name and ssh_g_field agree on" "$(printf '%s\n' "$FIPS_G" | ssh_g_field "$(ssh_g_name hostkey)")" \
    "ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,rsa-sha2-256,rsa-sha2-512"
is "the key types ssh skipped for its policy" "$(printf '%s\n' "$PUBKEY" | skipped_key_types)" "ssh-ed25519"

is "dropbear's refusals, the last few, without the rest of the log" "$(printf '%s\n' "$POD_LOG" | dropbear_refusals | paste -s -d'|' -)" \
    "[41] Oct 29 14:02:10 Exit before auth from <127.0.0.1:56180>: No matching algo enc c->s|[52] Oct 29 14:02:11 Exit before auth from <127.0.0.1:56188>: No matching algo hostkey"
is "at most five of them" "$(for i in 1 2 3 4 5 6 7; do echo "[$i] Exit before auth from <x>: Exited normally"; done | dropbear_refusals | wc -l | tr -d ' ')" 5
is "the five newest" "$(for i in 1 2 3 4 5 6 7; do echo "[$i] Exit before auth from <x>: Exited normally"; done | dropbear_refusals | head -n 1)" \
    "[3] Exit before auth from <x>: Exited normally"
is "a login for a user the pod does not have" \
    "$(printf '%s\n' '[9] Oct 30 10:29:56 Login attempt for nonexistent user from 127.0.0.1:42924' | dropbear_refusals)" \
    "[9] Oct 30 10:29:56 Login attempt for nonexistent user from 127.0.0.1:42924"
is "a pod log with no refusal" "$(printf '%s\n' '[1] Child connection from 127.0.0.1:1' 'Pubkey auth succeeded for x' | dropbear_refusals)" ""

# ── The key types a local policy accepts ────────────────────────────────────

fips_accepted=$(printf '%s\n' "$FIPS_G" | ssh_g_field pubkeyacceptedalgorithms)
is "an ed25519 key under a FIPS policy: none usable" "$(usable_key_types ssh-ed25519 "$fips_accepted")" ""
is "an ECDSA key under a FIPS policy" "$(usable_key_types ecdsa-sha2-nistp256 "$fips_accepted")" ecdsa-sha2-nistp256
is "an RSA key is usable through rsa-sha2-256" "$(usable_key_types ssh-rsa "$fips_accepted")" ssh-rsa
is "an RSA key is not usable when no RSA algorithm is" "$(usable_key_types ssh-rsa ecdsa-sha2-nistp256,ssh-ed25519)" ""
is "an RSA certificate through rsa-sha2-512-cert" \
    "$(usable_key_types ssh-rsa-cert-v01@openssh.com rsa-sha2-512-cert-v01@openssh.com)" ssh-rsa-cert-v01@openssh.com
is "an RSA certificate is not usable through a plain RSA algorithm" "$(usable_key_types ssh-rsa-cert-v01@openssh.com rsa-sha2-512)" ""
is "of several, the usable ones" "$(usable_key_types "$(printf 'ssh-ed25519\necdsa-sha2-nistp384\nssh-rsa')" "$fips_accepted" | paste -s -d, -)" \
    "ecdsa-sha2-nistp384,ssh-rsa"
is "a type is matched whole, not as a prefix" "$(usable_key_types ecdsa-sha2-nistp256 ecdsa-sha2-nistp256-cert-v01@openssh.com)" ""
is "two keys of a type: the type once" "$(usable_key_types "$(printf 'ecdsa-sha2-nistp256\necdsa-sha2-nistp256')" "$fips_accepted")" ecdsa-sha2-nistp256
is "the agent's key types" "$(printf '%s\n' 'ssh-ed25519 AAAAC3Nz pat@mac' 'ssh-rsa AAAAB3Nz pat@old' | agent_key_types | paste -s -d, -)" "ssh-ed25519,ssh-rsa"
is "an empty agent has no key types" "$(printf 'The agent has no identities.\n' | agent_key_types)" ""

# ── The hints ───────────────────────────────────────────────────────────────

files="/etc/ssh/ssh_config /etc/ssh/ssh_config.d/fips_ssh_config"
hint=$(negotiation_hint hostkey "ssh-ed25519,rsa-sha2-256" "ecdsa-sha2-nistp256,rsa-sha2-512" "$files")
has "a host key hint names what was refused" "$hint" "host key type"
has "a host key hint gives the pod's offer" "$hint" "ssh-ed25519,rsa-sha2-256"
has "a host key hint gives what ssh accepts" "$hint" "ecdsa-sha2-nistp256,rsa-sha2-512"
has "a host key hint names the policy files" "$hint" "/etc/ssh/ssh_config.d/fips_ssh_config"
has "a host key hint says the image may predate the FIPS fixes" "$hint" "FIPS"
has "a host key hint says how to get a newer image" "$hint" "bin/polar-dev build-image"
is "every line of a hint is in the script's voice" "$(printf '%s\n' "$hint" | awk '!/^polar-dev: /')" ""
has "a cipher hint names what was refused" "$(negotiation_hint cipher a b c)" "cipher"
has "a kex hint names what was refused" "$(negotiation_hint kex a b c)" "key exchange method"
has "a mac hint names what was refused" "$(negotiation_hint mac a b c)" "MAC"
lacks "a cipher hint does not say host key type" "$(negotiation_hint cipher a b c)" "accepts no host key type"

pod_key="256 SHA256:AAAAexampleED25519fingerprintAAAAAAAAAAAAAA pat@mac (ED25519)"
hint=$(publickey_hint "$pod_key" "your agent's first key" "$AGENT_LIST" "")
has "a denial names the key the pod has" "$hint" "SHA256:AAAAexampleED25519fingerprintAAAAAAAAAAAAAA"
has "a denial says where that key came from" "$hint" "your agent's first key"
has "a denial lists the agent's keys" "$hint" "SHA256:BBBBexampleRSAfingerprintBBBBBBBBBBBBBBBBBBB"
has "a denial says the agent holds the pod's key" "$hint" "your agent holds it"
hint=$(publickey_hint "256 SHA256:CCCCnotInTheAgentCCCCCCCCCCCCCCCCCCCCCCCCCCC other (ECDSA)" "POLAR_DEV_SSH_KEY" "$AGENT_LIST" "")
has "a denial for a key the agent lacks says so" "$hint" "not in your agent"
lacks "a denial for a key the agent lacks does not say it holds it" "$hint" "your agent holds it"
hint=$(publickey_hint "$pod_key" "your agent's first key" "$AGENT_LIST" "ssh-ed25519")
has "a denial for a type the policy skipped says so" "$hint" "did not offer your ssh-ed25519 keys"
has "a denial for a type the policy skipped blames the policy" "$hint" "PubkeyAcceptedAlgorithms"
lacks "the hint gives fingerprints, not keys" "$(publickey_hint "$pod_key" x "$AGENT_LIST" "")" "AAAAC3Nz"

has "a forward that has stopped says so" "$(forward_hint dead "error: lost connection to pod" refused)" "has stopped"
has "a forward that has stopped gives its log's last line" "$(forward_hint dead "error: lost connection to pod" refused)" "error: lost connection to pod"
has "a forward that runs but reaches nothing says the pod's end does not answer" "$(forward_hint alive "" closed)" "dropbear"
has "a refused port under a live forward points at the port" "$(forward_hint alive "" refused)" "POLAR_DEV_PORT"

# ── The image `up` applies ──────────────────────────────────────────────────

out=$(image_warning "localhost:30500/polar-dev:abc1234" "docker.io/daveman1010220/polar-dev:latest-arm64" ""); rc=$?
is "an image that changes, not asked for, asks" "$rc" 0
has "it names the running image" "$out" "the pod runs localhost:30500/polar-dev:abc1234"
has "it names the new image" "$out" "this applies docker.io/daveman1010220/polar-dev:latest-arm64"
has "it says how to keep the running one" "$out" "set POLAR_DEV_IMAGE=localhost:30500/polar-dev:abc1234 to keep it"
has "it says the Nix store is seeded again" "$out" "re-seeds the Nix store"
out=$(image_warning "a:1" "b:2" "b:2"); rc=$?
is "an image that changes because POLAR_DEV_IMAGE says so does not ask" "$rc" 1
has "but still says the store is seeded again" "$out" "re-seeds the Nix store"
lacks "and does not offer to keep the old one" "$out" "to keep it"
out=$(image_warning "a:1" "a:1" ""); rc=$?
is "the same image does not ask" "$rc" 1
is "the same image says nothing" "$out" ""
out=$(image_warning "" "a:1" ""); rc=$?
is "no pod yet does not ask" "$rc" 1
is "no pod yet says nothing" "$out" ""

# ── The clone ───────────────────────────────────────────────────────────────

is "the host of an scp-style remote" "$(repo_host git@52.235.181.217:SEI/ACE.git)" 52.235.181.217
is "the host of an ssh URL with a port" "$(repo_host ssh://git@gitlab.example.org:2222/SEI/ACE.git)" gitlab.example.org
is "the host of an https URL" "$(repo_host https://github.com/cmu-sei/Polar.git)" github.com
is "the host of a remote with no user" "$(repo_host example.org:SEI/ACE.git)" example.org
hint=$(clone_hint 52.235.181.217 "$AGENT_LIST")
has "a refused clone names the server" "$hint" "52.235.181.217"
has "a refused clone says the server knows none of the keys" "$hint" "knows none of the keys"
has "a refused clone lists the agent's fingerprints" "$hint" "SHA256:AAAAexampleED25519fingerprintAAAAAAAAAAAAAA pat@mac (ED25519)"
has "a refused clone says to add a key" "$hint" "add one of them to your account on 52.235.181.217"
has "a refused clone offers POLAR_DEV_REPO" "$hint" "POLAR_DEV_REPO"
has "a refused clone says how to clone later" "$hint" "bin/polar-dev up"

# ── build-image ─────────────────────────────────────────────────────────────

is "x86_64 is amd64" "$(oci_arch x86_64)" amd64
is "aarch64 is arm64" "$(oci_arch aarch64)" arm64
is "arm64 is arm64" "$(oci_arch arm64)" arm64
oci_arch riscv64 >/dev/null 2>&1; is "another machine is refused" "$?" 1
image_tag_ok abc1234; is "a short hash is a tag" "$?" 0
image_tag_ok v1.2_rc-3; is "dots, dashes and underscores" "$?" 0
image_tag_ok "a b"; is "a space is not" "$?" 1
image_tag_ok "-abc"; is "a leading dash is not" "$?" 1
image_tag_ok "a/b"; is "a slash is not" "$?" 1
image_tag_ok ""; is "nothing is not" "$?" 1
image_tag_ok "$(printf 'a%.0s' $(seq 1 129))"; is "129 characters are too many" "$?" 1
image_tag_ok "$(printf 'a%.0s' $(seq 1 128))"; is "128 are not" "$?" 0

# ── The personal sops files ─────────────────────────────────────────────────

TREE=$(mktemp -d)
is "no target has any: nothing" "$(user_sops_files "$TREE")" ""
mkdir -p "$TREE/infra/targets/one" "$TREE/infra/targets/two" "$TREE/infra/targets/three"
for f in one/user.sops.yaml one/.user.sops.yaml two/user.sops.yaml one/secrets.sops.yaml one/user.sops.yaml.example three/.sops.yaml; do
    echo encrypted > "$TREE/infra/targets/$f"
done
is "each target's personal files, and only those" "$(user_sops_files "$TREE")" "infra/targets/one/user.sops.yaml
infra/targets/two/user.sops.yaml
infra/targets/one/.user.sops.yaml"

# copy_user_sops against a stand-in for the pod: a directory, with the pod's tar (GNU's: BSD's has no --skip-old-files).
if tar --version 2>/dev/null | head -1 | command grep -q GNU; then
    POD=$(mktemp -d)
    k() {
        shift 5 # exec -i deploy/... -c dev (or exec deploy/... -c dev --, which is one fewer)
        [ "$1" = "--" ] && shift
        case "$1" in
            tar) local a out=(); for a in "$@"; do out+=("${a//\/workspace/$POD}"); done; "${out[@]}" ;;
            chown) echo "$*" > "$POD/chowned" ;;
            *) echo "the pod was asked to run $1, which is not in its image for certain" >&2; return 1 ;;
        esac
    }
    DEPLOY=polar-dev-test
    HERE=$TREE
    mkdir -p "$POD/infra/targets/two"
    echo "the pod's own" > "$POD/infra/targets/two/user.sops.yaml"
    copied=$(copy_user_sops 2>&1)
    is "what the pod has none of goes in" "$(cat "$POD/infra/targets/one/user.sops.yaml")" encrypted
    is "the file that says who it is for goes too" "$(cat "$POD/infra/targets/one/.user.sops.yaml")" encrypted
    is "what the pod has of its own stays" "$(cat "$POD/infra/targets/two/user.sops.yaml")" "the pod's own"
    [ ! -e "$POD/infra/targets/one/secrets.sops.yaml" ]; is "what git carries does not go" "$?" 0
    has "it says what it copied" "$copied" "infra/targets/one/user.sops.yaml"
    has "the files become the user's in the pod" "$(cat "$POD/chowned" 2>/dev/null)" "/workspace/infra/targets/one/user.sops.yaml"
    rm -rf "$POD"
    EMPTY=$(mktemp -d)
    HERE=$EMPTY
    has "with none here it says so, and that it is not a failure" "$(copy_user_sops 2>&1)" "no user.sops.yaml"
    rm -rf "$EMPTY"
fi
rm -rf "$TREE"

echo "polar-dev tests: $passed passed, $failures failed"
[ "$failures" -eq 0 ]
