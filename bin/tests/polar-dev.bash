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

# bin/polar-dev's pure parts: whether `up` is about to replace the pod's image, and what build-image takes. Nothing here
# talks to a cluster or a network.
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

echo "polar-dev tests: $passed passed, $failures failed"
[ "$failures" -eq 0 ]
