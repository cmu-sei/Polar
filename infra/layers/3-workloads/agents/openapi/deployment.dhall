-- infra/layers/3-workloads/agents/openapi/deployment.dhall
--
-- OpenAPI (web) agent Deployment.
-- STATUS: Placeholder — not yet implemented.
-- Pattern will mirror gitlab/jira when wired up.

let kubernetes = ../../../../schema/kubernetes.dhall
let Constants  = ../../../../schema/constants.dhall
let functions  = ../../../../schema/functions.dhall

-- TODO: implement when openapi agent is ready for deployment.
-- Follow the gitlab agent pattern: observer polls API, consumer writes graph.

in  {=}
