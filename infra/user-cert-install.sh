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

# infra/user-cert-install.sh <username>
#
# Imports <username>'s Polar certificate (~/.config/polar-admin/<username>.crt and .key) and the cluster's CA into the
# Librewolf profile on this machine, replacing any Polar certificate it held before. `just user-cert-install` runs it,
# and so does `bin/polar-dev cert` after copying the certificate out of an in-cluster dev container.
set -euo pipefail
POLAR_USER="${1:-$USER}"
DIR="$HOME/.config/polar-admin"
for f in "$POLAR_USER.crt" "$POLAR_USER.key" "ca.pem"; do
    [ -f "$DIR/$f" ] || { echo "ERROR: missing $DIR/$f" >&2; exit 1; }
done
# Librewolf keeps profiles in ~/.librewolf, or under ~/.config/librewolf/librewolf on
# newer versions and some packagings, and on macOS under ~/Library/Application Support/librewolf.
# The first profile found is used; with several, or another Firefox-family browser, set
# POLAR_NSS_DB. (`-exec dirname`, not GNU find's `-printf`, so macOS's find runs it too.)
LIBREWOLF_DIRS=("$HOME/.librewolf" "$HOME/.config/librewolf" "$HOME/Library/Application Support/librewolf")
NSS_DB="${POLAR_NSS_DB:-$({ find "${LIBREWOLF_DIRS[@]}" -maxdepth 3 -name cert9.db -exec dirname {} \; 2>/dev/null || true; } | head -1)}"
[ -n "$NSS_DB" ] || { echo "ERROR: no Librewolf profile with cert9.db found under ${LIBREWOLF_DIRS[*]} (set POLAR_NSS_DB to the profile directory)" >&2; exit 1; }
# Firefox-family browsers keep a `lock` symlink in the profile while running,
# named "<ip>:+<pid>". A crash leaves it behind, so only a live pid counts.
if [ -L "$NSS_DB/lock" ]; then
    lock_pid=$(readlink "$NSS_DB/lock"); lock_pid=${lock_pid##*+}
    if [[ "$lock_pid" =~ ^[0-9]+$ ]] && kill -0 "$lock_pid" 2>/dev/null; then
        echo "ERROR: Librewolf is running (process $lock_pid holds the profile lock). Close it and re-run." >&2
        exit 1
    fi
    echo "NOTE: ignoring a stale profile lock (process $lock_pid is gone)."
fi
# Not named TMP: nix-shell's stdenv overwrites TMP and TMPDIR inside the shell.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
openssl pkcs12 -export \
    -inkey "$DIR/$POLAR_USER.key" \
    -in "$DIR/$POLAR_USER.crt" \
    -certfile "$DIR/ca.pem" \
    -name "Polar $POLAR_USER" \
    -out "$WORK/user.p12" \
    -passout pass:changeit
# Runs inside nix-shell (needs certutil/pk12util). A file, not an inline
# string, so its quoting stays readable.
cat > "$WORK/install.sh" <<'EOS'
set -euo pipefail
db="sql:$NSS_DB"

# NSS gives every certificate with the same subject the same nickname, so a
# new certificate for a user who already has one in the store silently
# inherits the old nickname (typically "Imported Certificate") and the two
# stack up. So old certificates are recognised by what they are, not by
# name: compare subject and issuer hashes of the raw certificate.
user_hash=$(openssl x509 -in "$DIR/$POLAR_USER.crt" -noout -subject_hash)
ca_hash=$(openssl x509 -in "$DIR/ca.pem" -noout -subject_hash)

removed=0
# True if a private key is stored under this nickname. `certutil -K` lines
# look like "< 0> ec  <key id>  <nickname>": drop the first four fields.
has_key() {
    certutil -K -d "$db" 2>/dev/null \
        | awk -v n="$1" '/^</ { $1 = $2 = $3 = $4 = ""; sub(/^ +/, ""); if ($0 == n) found = 1 } END { exit !found }'
}
# Deletes every certificate stored under a nickname, and the private keys
# that go with them. One at a time, because they can differ: -F removes a
# certificate together with its key, but quietly does nothing for one that
# has no key (a CA), and -D removes a certificate but would leave a key behind.
remove_all() {
    local nick="$1" guard=0
    while certutil -L -n "$nick" -d "$db" >/dev/null 2>&1 && [ "$guard" -lt 20 ]; do
        if has_key "$nick"; then
            certutil -F -n "$nick" -d "$db" 2>/dev/null || true
        else
            certutil -D -n "$nick" -d "$db" 2>/dev/null || true
        fi
        guard=$((guard + 1))
        removed=$((removed + 1))
    done
}

echo "Removing old Polar certificates from $NSS_DB ..."
# One line per nickname: the listing minus its trailing trust flags
# ("u,u,u", "CT,,"), which are padded with spaces.
certutil -L -d "$db" \
    | awk 'NF && !/^Certificate Nickname/ && !/^[[:space:]]*SSL,S\/MIME/ { sub(/[[:space:]]+$/, ""); sub(/[[:space:]]+[A-Za-z,]*$/, ""); print }' \
    | sort -u > "$WORK/nicknames"
while IFS= read -r nick; do
    [ -n "$nick" ] || continue
    # Raw DER of one certificate under this nickname; all of them share a subject.
    certutil -L -n "$nick" -d "$db" -r > "$WORK/probe.der" 2>/dev/null || continue
    subject_hash=$(openssl x509 -inform DER -in "$WORK/probe.der" -noout -subject_hash 2>/dev/null || true)
    issuer_hash=$(openssl x509 -inform DER -in "$WORK/probe.der" -noout -issuer_hash 2>/dev/null || true)
    if [[ "$nick" == "Polar "* ]] \
        || [ "$subject_hash" = "$user_hash" ] \
        || [ "$subject_hash" = "$ca_hash" ] \
        || [ "$issuer_hash" = "$ca_hash" ]; then
        echo "  removing: $nick"
        remove_all "$nick"
    fi
done < "$WORK/nicknames"
echo "  removed $removed certificate(s)"

echo "Importing into $NSS_DB ..."
pk12util -i "$WORK/user.p12" -d "$db" -W changeit
# pk12util imports the CA from the bundle without trust; replace it with a trusted one.
certutil -D -n 'Polar Internal CA' -d "$db" 2>/dev/null || true
certutil -A -n 'Polar Internal CA' -t 'CT,,' -a -i "$DIR/ca.pem" -d "$db"
EOS
export NSS_DB DIR WORK POLAR_USER
# NSS's tools (certutil, pk12util) from the PATH when they are there (a Mac with nss from its package manager), else from Nix.
if command -v certutil >/dev/null && command -v pk12util >/dev/null; then
    bash "$WORK/install.sh"
else
    # Without the dev shell's startup hook, which a nested nix-shell would run again (its banner and its notes).
    env -u shellHook nix-shell -p nssTools openssl --run "bash $WORK/install.sh"
fi
# What starts the forward the browser needs: the caller says (bin/polar-dev, for a cluster the dev pod is in), else the Justfile's.
echo "Done. ${POLAR_CERT_NEXT:-just port-forwards-up}, then browse to https://localhost:7473"
