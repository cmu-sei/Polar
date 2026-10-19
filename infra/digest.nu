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

# infra/digest.nu
#
# The digest an image has in a registry, asked with a HEAD request for its manifest (about ten milliseconds; `skopeo inspect`
# takes a few hundred). Used to key what is computed from an image, such as its scaffold, so a rebuilt image under the same tag is a
# different key and nothing stale is served.

# "localhost:5000/cassini:latest" -> { host, name, tag }, or null when the reference does not have that shape (a digest reference,
# a reference with no tag).
export def parse_image_ref [ref: string] {
    let found = ($ref | parse --regex '^(?<host>[^/@]+)/(?<name>[^@:]+(?:/[^@:]+)*):(?<tag>[^:/@]+)$')
    if ($found | is-empty) { null } else { $found | first }
}

# The Docker-Content-Digest header of a response to a manifest HEAD request, or null.
export def digest_from_headers [headers: string] {
    let line = ($headers | lines | where {|l| ($l | str lowercase | str starts-with "docker-content-digest:") })
    if ($line | is-empty) { return null }
    let digest = ($line | first | split row ":" | skip 1 | str join ":" | str trim)
    if ($digest | str starts-with "sha256:") { $digest } else { null }
}

const ACCEPT = "application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json"

# The digest the registry serves for `ref`, or null when it cannot say (the registry does not answer, or the image is not there).
# A plain-HTTP registry is how the node registry is reached.
export def registry_digest [ref: string] {
    let parts = (parse_image_ref $ref)
    if $parts == null { return null }
    let url = $"http://($parts.host)/v2/($parts.name)/manifests/($parts.tag)"
    let r = (do { ^curl -sI -m 5 -H $"Accept: ($ACCEPT)" $url } | complete)
    if $r.exit_code != 0 { return null }
    digest_from_headers $r.stdout
}
