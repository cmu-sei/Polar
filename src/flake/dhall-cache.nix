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

# The Dhall cache the render needs, so that it fetches nothing.
#
# Every remote import in the repository's Dhall is pinned (`https://... sha256:<hash>`), and Dhall
# keeps a pinned import in its cache as `1220<hash>`: the import's normal form, encoded, whose own
# sha256 is that hash. So each entry is a fixed-output derivation whose hash is the pin already in
# the source, and nothing here has to be kept in step with it: the imports are read from the
# repository's .dhall files when this is evaluated. With these entries in its cache, Dhall reads an
# import from the cache and never looks at the files beneath it (dhall-kubernetes is a thousand of
# them, fetched one at a time on a cold cache).
#
#   $out/lib/polar/dhall-cache/1220<hash>   one per pinned remote import
#
# render.nu copies the entries its cache lacks from POLAR_DHALL_CACHE, or from /lib/polar/dhall-cache
# in the dev container, which carries this.
{ pkgs, lib, roots }:
let
  dhallFiles = lib.concatMap
    (root: lib.filter (f: lib.hasSuffix ".dhall" (toString f)) (lib.filesystem.listFilesRecursive root))
    roots;

  importsIn = file:
    map (m: { url = builtins.elemAt m 0; hash = builtins.elemAt m 1; })
      (lib.filter builtins.isList
        (builtins.split "(https://[^[:space:]]+)[[:space:]]+sha256:([0-9a-f]{64})" (builtins.readFile file)));

  # One per hash: the same import pinned at two URLs (a library at two commits) is one entry.
  imports = lib.attrValues (lib.listToAttrs
    (map (i: { name = i.hash; value = i; }) (lib.concatMap importsIn dhallFiles)));

  entry = i: pkgs.runCommand "dhall-cache-1220${i.hash}" {
    outputHashAlgo = "sha256";
    outputHashMode = "flat";
    outputHash = i.hash;
    nativeBuildInputs = [ pkgs.dhall ];
    SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
  } ''
    export XDG_CACHE_HOME="$TMPDIR/cache"
    echo '${i.url} sha256:${i.hash}' > import.dhall
    dhall resolve --file import.dhall > /dev/null
    cp "$XDG_CACHE_HOME/dhall/1220${i.hash}" "$out"
  '';
in
pkgs.runCommand "polar-dhall-cache" { } ''
  mkdir -p "$out/lib/polar/dhall-cache"
  ${lib.concatMapStrings (i: ''
    cp ${entry i} "$out/lib/polar/dhall-cache/1220${i.hash}"
  '') imports}
''
