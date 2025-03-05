Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE
MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO
WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER
INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE OR
MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT
TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact
permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release
and unlimited distribution.  Please see Copyright notice for non-US Government
use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to
its own license.

DM24-0470

# Polar

<p align="center">
  <img width="300" height="300" src="docs/gitlab/Polar-Logo.png">
</p>

## Background

### General Background
"Polar" is a play on the original "Mercator", as it was released by Lending Club, a set of agents designed to collect infrastructure data and load it into a graph, using Neo4J. The original [Lending Club Mercator](https://github.com/LendingClub/) agents were all built in Java, for an obsolete version of Neo4J, and is no longer publicly-available on Github. 

Polar is a knowledge graph framework that takes inspiration from that original project, bringing with it modern ideas for software. Polar alters the original architecture to include pub/sub, mutual TLS, and external observation of services and infrastructure.

### Project Structure
- **conf**: Configuration files for Polar, including settings, certificates, and docker compose files to customize the framework according to specific requirements.
- **docs**: Documentation resources for Polar, such as guides, diagrams, and explanatory documents, to aid in understanding and utilizing the framework effectively.
- **scripts**: Utility scripts for Polar, including development stack setup, backup, and loader scripts, designed to automate common tasks and streamline workflow processes.
- **src**: The source code of Polar, where the core functionality and agents of the framework are implemented, allowing users to delve into the codebase for customization or extension.
- **var**: Variable data for Polar, including SSL certificates, Neo4J volumes, and TLS generation data, essential for the framework's operation and customization. The var folder will be created by the user or automatically after cloning the project.

### Modular Workspace
The core workspace, located in the 'src' folder, serves as the foundation of Polar's modular architecture. This structure enables seamless integration of additional cargo projects, allowing for cohesive building of components. A simple `cargo build` command executed within the top-level workspace folder orchestrates the compilation of all sub-projects.

### Language Choice and Graph Independence
Polar agents are predominantly envisioned to be implemented in Rust, prioritizing versatility and independence from specific graph technologies. This approach aims to minimize direct dependencies on Neo4J, facilitating effortless interaction with the graph through a generalized interface. Users can flexibly select the desired graph implementation via a configuration file, paving the way for potential support of various graph types beyond Neo4J in the future.

## Agents

### Gitlab Agent

The gitlab agent is a suite of services configured to observe a given gitlab instance and push to a hosted rabbitmq message broker. 

Three important parts of the framework implementation include:
* GitLab Resource Observer
    * Requires credentials for Gitlab in the form of a private token configured with read and write access for the API as well as credentials for authenticating with the given rabbitmq instance. The GitLab Observer is instantiated as a set of cooperating binaries, each handling a specific type of GitLab data.
* GitLab Message Consumer (now known as the Information Processor)
    * The message consumer requires credentials for the rabbitmq instance as well and credentials for signing into a given neo4j graph database to publish information to. The information processor transforms specific GitLab data into domain-specified nodes and edges that can be used to tie CI/CD concepts to other domain knowledge.
* The Types Library
    * Contains implementation of the various GitLab types, as well as implementations  for serialization / deserialization.

All credentials, endpoints, and the like should be read in as environment variables,possibly from an environment file. There is an example an environment file in the gitlab agent [README](./docs/README_gitlab.md) in the manual setup.

## Getting Started
Please consult the [README](./docs/README_gitlab.md) in the docs folder 

## Additional Resources
* [Polar: Improving DevSecOps Observability](https://insights.sei.cmu.edu/blog/polar-improving-devsecops-observability/): Blog that provides comprehensive insights into Polar's architecture, components, and capabilities.
