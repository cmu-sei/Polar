# polar-schedules

The scheduler's schedules, one Dhall file per schedule, in the directory for its kind:
`permanent/` (one agent, by id), `adhoc/` (an agent type that announces itself) and `ephemeral/`
(an agent type started by an event). The scheduler observer evaluates every `.dhall` file here and
publishes the result; the processor records it and notifies the agents it concerns.

This directory is the first commit of the repository, made once by the Polar git server when it
finds the repository empty. Later commits are made by whoever the server's access list lets push.
Redeploying never changes the history.
