#!/usr/bin/env nu
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
