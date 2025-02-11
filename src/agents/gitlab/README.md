# Gitlab Agent

Three important parts of the framework implementation include:
* GitLab Resource Observer
    * Requires credentials for Gitlab in the form of a private token configured with read and write access for the API as well as credentials for authenticating with the given rabbitmq instance. The GitLab Observer is instantiated as a set of cooperating binaries, each handling a specific type of GitLab data.
* GitLab Message Consumer
    * The message consumer requires credentials for the rabbitmq instance as well and credentials for signing into a given neo4j graph database to publish information to. The consumer transforms specific GitLab data into domain-specified nodes and edges that can be used to tie CI/CD concepts to other domain knowledge.
* The Types Library
    * Contains implementation of the various GitLab types, as well as implementations  for serialization / deserialization.

All credentials, endpoints, and the like should be read in as environment variables,possibly from an environment file. There is an example an environment file in the gitlab agent [README](../../docs/README_gitlab.md) in the manual setup.

