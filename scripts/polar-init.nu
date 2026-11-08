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

# polar-init.nu — cert bootstrap init script for Polar agents.
#
# Reads the projected SA token, calls cert-issuer-init to obtain
# a short-lived mTLS cert, and exits. The workload container starts
# only after this exits zero.
#
# Required env vars:
#   POLAR_CERT_ISSUER_URL  — in-cluster address of the cert issuer
#   POLAR_CERT_DIR         — emptyDir mountPath for cert output
#   POLAR_SA_TOKEN_PATH    — path to the projected SA token (default provided)
#
# Exit codes mirror cert-issuer-init:
#   0 — success
#   1 — misconfiguration (bad token, wrong audience, identity mismatch)
#   2 — transient failure (cert issuer unreachable, CA unavailable)
#   3 — internal error


let token_path = ($env | get -o POLAR_SA_TOKEN_PATH | default "/home/polar/token")
let cert_dir   = ($env | get -o POLAR_CERT_DIR       | default "/home/polar/certs")
let issuer_url = ($env | get -o POLAR_CERT_ISSUER_URL | default "")
let cert_type  = ($env | get -o POLAR_CERT_TYPE       | default "client")

if $issuer_url == "" {
    print "POLAR_CERT_ISSUER_URL must be set"
    exit 1
}

if not ($token_path | path exists) {
    print $"SA token not found at ($token_path). Check that the projected SA token volume is mounted correctly"
    exit 1
}

print $"obtaining ($cert_type) cert from ($issuer_url)"

# Request certificates, hardcoded to use ecdsa-p256 algorithms until it becomes necessary/desirable to use another.
let extra_sans = ($env | get -o POLAR_EXTRA_SANS | default "" | split row "," | each { str trim } | where { |s| $s != "" })

let extra_san_flags = ($extra_sans | each { |san| ["--extra-san", $san] } | flatten)

cert-client --cert-issuer-url $issuer_url --token-path $token_path --cert-dir $cert_dir --cert-type $cert_type --key-algorithm ecdsa-p256 ...$extra_san_flags

let exit_code = $env.LAST_EXIT_CODE

if $exit_code != 0 {
    log -e $"cert-issuer-init failed with exit code ($exit_code)"
    exit $exit_code
}

print $"cert bundle written to ($cert_dir)"
