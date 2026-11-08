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

# infra/validate.nu
#
# Environment validation helpers.
# Called by render.nu before any rendering begins.
# Exits with a clear error message if required vars are missing.

# Validate that all required environment variables are set and non-empty.
# Takes a list of variable name strings.
# Returns nothing on success, exits on failure.
export def validate_env [required: list<string>] {
    let missing = $required | where { |var|
        let val = ($env | get -o $var)
        ($val == null) or ($val | is-empty)
    }

    if ($missing | is-empty) {
        print "  env: all required variables present"
    } else {
        print $"ERROR: Missing required environment variables:"
        for var in $missing {
            print $"  - ($var)"
        }
        print ""
        print "Source your environment file and try again."
        print "Example: source my.env && nu infra/render.nu local"
        exit 1
    }
}

# Validate that a file exists and is non-empty.
export def validate_file [path: string, description: string] {
    if not ($path | path exists) {
        print $"ERROR: Required file missing: ($path)"
        print $"  ($description)"
        exit 1
    }
    if (open $path | str trim | is-empty) {
        print $"ERROR: Required file is empty: ($path)"
        print $"  ($description)"
        exit 1
    }
}

# Validate that a directory exists.
export def validate_dir [path: string, description: string] {
    if not ($path | path exists) {
        print $"ERROR: Required directory missing: ($path)"
        print $"  ($description)"
        exit 1
    }
}
