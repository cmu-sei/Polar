{ apiVersion = "v2"
, name = "polar"
, description = "A Helm chart for deploying the Polar application"
, type = "application"
, version = "0.1.0"
, appVersion = "0.1.0"
, dependencies = [
      { name = "gitlab-agent" }
  ,   { name = "neo4j" }
  ,   { name = "cassini" }
]
}
