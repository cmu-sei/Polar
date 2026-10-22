
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
# src/agents/credentials-agent/model/tests/fixtures/regenerate.nu
#
# Writes sandbox-model.json: the authority model the render gives the credentials agent for `sandbox-local`, made from this
# workspace's own agents (each built here and asked for its scaffold), not from the images on a cluster, so it carries
# what the code declares now. The model crate's tests judge policies over it. Run it after a change to what an agent
# declares, and commit the result:
#
#   nu src/agents/credentials-agent/model/tests/fixtures/regenerate.nu

use ../../../../../../infra/topics.nu [served]

const ROOT = path self ../../../../../..
const OUT = path self sandbox-model.json
# The issuance floor the render gives the credentials agent today, from the same catalog: slice 8 compares it with what
# the model gives each workload, then removes it.
const FLOOR = path self sandbox-floor.json

def main [] {
    let catalog = (glob ($ROOT | path join "infra/config/workloads/*.nuon") | sort | each {|f| open $f } | flatten)
    let built = ($catalog | where {|w| ($w.config? | default "env") == "document" })
    let served_list = (served $catalog)
    let agents = ($ROOT | path join "src/agents")
    let dir = (mktemp --directory)
    for w in $built {
        let package = ($w.scaffold_package? | default $w.image)
        # The broker's binary is named for what it is, not for its crate.
        let binary = if $package == "cassini-broker" { "cassini-server" } else { $package }
        let features = ($w.scaffold_features? | default "")
        let build = (do {
            cd $agents
            if ($features | is-empty) { ^cargo build -q -p $package --bin $binary } else { ^cargo build -q -p $package --bin $binary --features $features }
        } | complete)
        if $build.exit_code != 0 { error make { msg: $"($w.name): cargo build failed: ($build.stderr | lines | last 3 | str join '; ')" } }
        let target = ($env.CARGO_TARGET_DIR? | default ($agents | path join "target"))
        with-env { POLAR_SERVED_WORKLOADS: $served_list } { ^($target | path join "debug" $binary) --scaffold }
        | save --force ($dir | path join $"($w.name).json")
    }
    let maps = (with-env { POLAR_SCAFFOLD_DIR: $dir } {
        ^nu ($ROOT | path join "infra/resolve.nu") resources credentials-agent --target sandbox-local
    } | from json | get config_maps)
    rm -rf $dir
    let variables = ($maps | first | get content | from json | get sections | each {|s| $s.variables } | flatten)
    let model = ($variables | where name == "CREDENTIALS_AGENT_AUTHORITY_MODEL" | get 0.value | from json)
    $model | to json --indent 2 | save --force $OUT
    print $"wrote ($OUT): ($model.workloads | length) workloads"
    let floor = ($variables | where name == "CREDENTIALS_AGENT_ISSUANCE_TOPICS" | get 0.value | from json)
    $floor | to json --indent 2 | save --force $FLOOR
    print $"wrote ($FLOOR): ($floor | length) workloads"
}
