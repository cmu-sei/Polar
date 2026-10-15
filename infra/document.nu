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

# infra/document.nu
#
# How the resolver fills an agent's configuration document. The agent prints its own scaffold
# (`<agent> --scaffold`): every variable of every section that applies to it, with type,
# default, secrecy and documentation, each value empty. The catalog knows the deployment's
# facts, and what it gives the workload is a list of environment entries (`resolve.nu` builds
# it from the capabilities, the kind and the workload). This module writes those entries into
# the variables they name, and says what does not fit:
#
#   - an entry for a variable the agent does not declare (nothing reads it, so it is an error,
#     not a silent extra),
#   - a required variable with no value and no default,
#   - a secret given as a literal (the document sits in a ConfigMap; secrets arrive through
#     the environment from a Secret, and the document holds a reference).
#
# Pure: records in, records out. Tested in infra/tests/document.nu.

# The scaffold with the entries written into it.
#
# `entries` is the list of { name, value, secret, key, from } the resolver built. Returns
# { document, problems, secret_env }: `secret_env` is the entries that were secrets, which the
# pod must still receive as environment variables (the document refers to them).
export def fill_document [scaffold: record, entries: list, who: string] {
    let declared = ($scaffold.sections | each {|s| $s.variables | get name } | flatten)
    let agent = ($scaffold.agent.name? | default $who)

    let unknown = ($entries | where {|e| not ($e.name in $declared) } | each {|e|
        $"($e.name) is set by ($e.from), but ($agent) does not declare it"
    })

    let by_name = ($entries | where {|e| $e.name in $declared } | reduce --fold {} {|e, acc| $acc | upsert $e.name $e })

    mut problems: list<string> = $unknown
    mut secret_env: list<any> = []
    mut sections: list<any> = []
    for section in $scaffold.sections {
        mut variables: list<any> = []
        for v in $section.variables {
            let given = ($by_name | get -o $v.name)
            mut value: any = null
            if $given != null {
                if $given.secret != null {
                    $value = { env: $v.name }
                    $secret_env = ($secret_env | append $given)
                } else if $v.secret {
                    $problems = ($problems | append $"($v.name) is a secret and ($given.from) gives it as a literal; give it from a Secret")
                } else {
                    $value = $given.value
                }
            } else if $v.required {
                $problems = ($problems | append $"($v.name) is required and nothing gives it a value: ($v.doc)")
            }
            $variables = ($variables | append ($v | upsert value $value))
        }
        $sections = ($sections | append ($section | upsert variables $variables))
    }

    { document: ($scaffold | upsert sections $sections), problems: $problems, secret_env: $secret_env }
}

# What the pod's container is given: where the document is, and the secrets it refers to.
export def pod_env [filled: record, path: string] {
    [{ name: "POLAR_CONFIG", value: $path, secret: null, key: null, from: "document" }] ++ $filled.secret_env
}

# The capabilities the agent's own code requires, as it says them.
export def requires_of [scaffold: record] {
    $scaffold.requires
}

# The least the deployment may give the agent to drain, in seconds.
export def budget_minimum_of [scaffold: record] {
    $scaffold.shutdown_budget_minimum_secs
}

# The document as the text that goes into the ConfigMap.
export def document_json [filled: record] {
    $filled.document | to json
}

# The command an image runs, as the image's own configuration says it: its entrypoint, then its cmd.
# The scaffold is asked of that command, so the agent that answers is the one the image would run.
export def image_command [config: record] {
    let command = (($config.Entrypoint? | default []) ++ ($config.Cmd? | default []))
    if ($command | is-empty) {
        error make { msg: "the image has neither an entrypoint nor a cmd, so there is nothing to ask for its scaffold" }
    }
    $command
}
