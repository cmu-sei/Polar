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

# infra/admin.nu
#
# The decisions about ~/.config/polar-admin that up.nu makes, kept apart from the filesystem so they can be tested
# (infra/tests/admin.nu). That directory holds an administrator's certificates and the CA they were issued under,
# so it belongs to one cluster; a reset gives a new CA and the old certificates are of no use to the new cluster.

# Whether what is stored in the admin directory belongs to another cluster: the CA stored with it
# (`stored_ca`, null when there is none) is not this cluster's (`cluster_ca`). Trailing whitespace is not a difference.
# Nothing stored says nothing, so there is nothing to move.
export def is_another_clusters [stored_ca: any, cluster_ca: string] {
    if $stored_ca == null { return false }
    ($stored_ca | str trim) != ($cluster_ca | str trim)
}

# Where to move a directory aside to: beside it, with the time in the name, and a number after it when that name is
# taken, so an earlier one is never overwritten. `taken` is the paths already in use.
export def aside_path [dir: string, stamp: string, taken: list<string>] {
    let base = $"($dir).old-($stamp)"
    if not ($base in $taken) { return $base }
    mut n = 2
    while ($"($base)-($n)" in $taken) { $n = $n + 1 }
    $"($base)-($n)"
}

# Moves everything in `dir` into a new directory `aside`, leaving `dir` there and empty. It moves what is in it rather
# than renaming it, because `dir` may be a mount point, which cannot be renamed (EBUSY): in the dev container in the
# cluster, ~/.config/polar-admin is a subPath of the pod's state volume.
export def move_aside [dir: string, aside: string] {
    mkdir $aside
    ls --all $dir | get name | each {|entry| mv $entry $aside }
    null
}
