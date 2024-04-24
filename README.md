# Polar
This is a prototyped suite of tools modeled after the Lending Club's mercator
project, which was a set of software tools developed to provide visibility into
an organization's currently deployed infrastructure. 

The top-level folder is a Cargo workspace. Its main purpose is to contain other
cargo projects and build them as a unit, placing build artifacts in the
top-level workspace build context. I.e., conducting a `cargo build` in the
top-level workspace folder will build all sub-projects.

"Polar" is a play on the original "Mercator", as it was released by Lending
Club, a set of agents designed to collect infrastructure data and stuff it all
into a graph, using Neo4J. The original Lending Club Mercator agents were all
built in Java, for an obsolete version of Neo4J, and is no longer
publicly-available on Github on Github. This project takes inspiration from
that original project, bringing with it modern ideas and modern software.

Polar agents will be implemented mostly in Rust, and where possible, will avoid
a direct dependence on Neo4J as the underlying graph technology. I.e.,
establishing a connection to and interacting with the graph should require as
little graph-specific nomenclature as possible, creating agents using a
generalized graph interface, and choosing the specific implementation from a
configuration file, and an associated implementation of the graph for the
chosen configuration type. Initially, this will be Neo4J, but the graph
landscape is rich and we should be able to support multiple graph types with
some small effort.

## Tools

### Gitlab Agent

The gitlab agent is a suite of services configured to read information about a
given gitlab instance and push to a hosted rabbitmq message broker. 

The suite is composed of three parts
* The Resource Observer
    * Requires credentials for Gitlab in the form of a private token configured
      with read and write access for the API as well as credentials for
      authenticating with the given rabbitmq instance.
* The Message Consumer
    * The message consumer requires credentials for the rabbitmq instance as
      well and credentials for signing into a given neo4j graph database to
      publish information to.
* The Types Library

All credentials, endpoints, and the like should be read in as environment variables. 


## Getting started and running example
This tool requires the following values to be present in your environment as
variables - some of which may contain sensitive data and should be stored
securely. See your team about how to retrieve this information.

* GITLAB_ENDPOINT - the service endpoint of the gitlab api you wish to pull information from NOTE: the endpoint must end in /api/v4
* GITLAB_TOKEN - NOTE: Data received may vary depending on access level of the
  token, see Gitlab's documentation for more information. The private token
  used to authenticate with the given gitlab instance, should be provided to
  the service when making calls. 
* NEO4J_ENDPOINT - the service endpoint of the given neo4j instance
* NEO4J_USER
* NEO4J_PASSWORD
* NEO4J_DB
* RABBITMQ_ENDPOINT - the service endpoint of the rabbitmq instance. Should be prefixed in amqp://
