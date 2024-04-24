# Polar

"Polar" is a play on the original "Mercator", as it was released by Lending
Club, a set of agents designed to collect infrastructure data and load it
into a graph, using Neo4J. The original Lending Club Mercator agents were all
built in Java, for an obsolete version of Neo4J, and is no longer
publicly-available on Github. Polar is a knowledge graph framework that takes 
inspiration from that original project, bringing with it modern ideas for software.
Polar alters the original architecture to include pub/sub, mutual TLS, and external 
observation of services and infrastructure.

The main Cargo workspace is located in the 'src' folder. Its purpose is to 
contain other cargo projects and build them as a unit, placing build artifacts
in the top-level workspace build context. I.e., conducting a `cargo build` in the
top-level workspace folder will build all sub-projects.

Polar agents are anticipated to be implemented mostly in Rust, and where possible,
will avoid a direct dependence on Neo4J as the underlying graph technology. I.e.,
establishing a connection to and interacting with the graph should require as
little graph-specific context as possible, creating agents using a
generalized graph interface, and choosing the specific implementation from a
configuration file, and an associated implementation of the graph for the
chosen configuration type. Initially, this will be Neo4J, but the graph
landscape is rich and we plan to support multiple graph types with some effort.

## Tools

### Gitlab Agent

The gitlab agent is a suite of services configured to observe a given gitlab 
instance and push to a hosted rabbitmq message broker. 

Three important parts of the framework implementation include:
* GitLab Resource Observer
    * Requires credentials for Gitlab in the form of a private token configured
      with read and write access for the API as well as credentials for
      authenticating with the given rabbitmq instance. The GitLab Observer is
      instantiated as a set of cooperating binaries, each handling a specific
      type of GitLab data.
* GitLab Message Consumer (now known as the Information Processor)
    * The message consumer requires credentials for the rabbitmq instance as
      well and credentials for signing into a given neo4j graph database to
      publish information to. The information processor transforms specific GitLab
      data into domain-specified nodes and edges that can be used to tie CI/CD concepts
      to other domain knowledge.
* The Types Library
    * Contains implementation of the various GitLab types, as well as implementations 
      for serialization / deserialization.

All credentials, endpoints, and the like should be read in as environment variables, 
possibly from an environment file. There is an example in the tree.

## Getting Started
Please consult the Readme in the specific Agents folder for GitLab for additional details.
