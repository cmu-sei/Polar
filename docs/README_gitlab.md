Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact
permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release
and unlimited distribution.  Please see Copyright notice for non-US
Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject
to its own license.

DM24-0470


# Gitlab Agent Development Compose SetUp

There are some first time components before the Gitlab agent can be ran. When doing local development, the rust binaries should be compiled and ran locally outside a container. More will be explained further down. 

# Requirements

## Operating System
MacOS Monterrey or newer or modern Linux

Instructions written and tested on an Intel Mac running MacOS Sonoma (14.5) and Ubuntu 22.04 (LTS) on an Amazon EC2 instance (t3.2xlarge). 

## Hardware
- Multi-core CPU
- At least 8GB of RAM
- 25GB Free Storage

## Software
- [Rust](https://doc.rust-lang.org/cargo/getting-started/installation.html)
- GNU Make
- Git
- OpenSSL
- Sudo
- (Recommended, but not required, instructions written assuming Docker is installed) [Docker Engine](https://docs.docker.com/engine/install/) with [Docker Compose Plugin](https://docs.docker.com/compose/install/)

## Install Commands

### Fedora/CentOS/RHEL:
```sh
sudo dnf install make git openssl sudo
```

### Debian:
```sh
sudo apt-get install make git openssl sudo
```

### Arch:
```sh
sudo pacman -S make git openssl sudo
```

### Alpine:
```sh
sudo apk add make git openssl sudo
```

### MacOS
Brew required. Install if you are missing it: https://brew.sh/
```sh
brew install make git openssl
```

# Automated Setup
Running the below set up script will create an environments file, creates the necessary certificates files and updates permissions to allow you to run the tool.
```sh
cd $PROJECT_ROOT/scripts
chmod +x dev_stack.sh
./dev_stack.sh
```

# Manual Setup
[skip if you have already run the automated setup above]

This tool requires the following values to be present in your environment as
variables - some of which may contain sensitive data and should be stored
securely. See your team about how to retrieve this information.
1. Create an environments file named `conf/env_setup.sh` with the following template.
```sh
# Generated Environment Configuration. If you edit this, do not re-run dev_stack.sh
# script without backing up this file, first.
# The service endpoint of the given neo4j instance.
# For local development, this could be "neo4j://neo4j:7687"
export GRAPH_ENDPOINT=""
# The service endpoint of the broker instance. (for rabbitmq, prefix with amqp://)
# For the development container, the name could be "rabbitmq". The auth mechanism,
# if specified, is according to your configuration and follows the docs for
# your chosen broker.
# For our default, reference: "amqps://rabbitmq:5671/%2f?auth_mechanism=external"
export BROKER_ENDPOINT=""
# For the development container, this should be "neo4j"
export GRAPH_USER=""
# For the development container, this will be whatever you set it to be when
# you set up your graph.
export GRAPH_PASSWORD=""
# For the development container, this might be "neo4j"
export GRAPH_DB=""
# The absolute file path to the client .p12 file. This is used by the Rust
# binaries to auth with the broker via TLS.
export TLS_CLIENT_KEY=""
# If a password was set for the .p12 file, put it here.
export TLS_KEY_PASSWORD=""
# The absolute file path to the ca_certificates.pem file created by TLS_GEN.
# Used by the Rust binaries to auth with RabbitMQ via TLS.
export TLS_CA_CERT=""
# The absolute file path to the observer_config.yaml. One is available in the
# following dir: ./gitlab_agent/src/observer/src/observer_config.yaml
export GITLAB_OBSERVER_CONFIG=""
# The endpoint must end in /api/v4
export GITLAB_ENDPOINT=""
# A Personal Access Token for the instance (Note: The information returned from
# GitLab will depend on the permissions granted to the token
# See Gitlabs REST API docs for more information
# For reference, GitLab tokens use the form, "glpat-xxxxxxxxxxxxxxxxxxxx"
export GITLAB_TOKEN=""
```
2. SSL Files will need to be generated for the RabbitMQ server. The server is expecting them at specific locations, which should be an `ssl` directory in the same directory as this README file. 
3. Using the tls-gen created by RabbitMQ ( repo here: https://github.com/rabbitmq/tls-gen ), generate the certificates. There are instructions in the `basic` directory there, but here is the basics:
   1. Clone the repo and change into the `basic` directory
   2. Run `make CN=rabbitmq` to generate the basic certificates.
   3. Copy the contents of the created `results` directory to the `ssl` one created in the same directory as this README. 
   `
      1. `mkdir $PROJECT_ROOT/conf/gitlab_compose/ssl`
      2. `cp results/* $PROJECT_ROOT/conf/gitlab_compose/ssl`
4. Due to a bug in a Rust Library, the client p12 file created will need to be converted to a legacy file using openssl. Change the `rabbitmq` portion of the command to whatever **CN** was used in the make command above. 
   1. Change into the `ssl` directory
      1. `cd $PROJECT_ROOT/conf/gitlab_compose/ssl`
      2. `cp client_rabbitmq.p12 client_rabbitmq.p12.original`
   2. Run the following command (Overwriting the existing `client_<CN>.p12` file): 
```bash
openssl pkcs12 -legacy -export -inkey client_rabbitmq_key.pem -in client_rabbitmq_certificate.pem -out client_rabbitmq.p12 -passout pass:""
```
5. If running on Linux, the server certificate files will need to have their permissions changed. Refer to the following link:
    * Section: **Permission of SSL/TLS certificate and key files**
    * https://github.com/bitnami/containers/tree/main/bitnami/rabbitmq#permission-of-ssltls-certificate-and-key-files
    `sudo chown $(whoami):root *`
    `sudo chmod 400 *`
    `cd ../`
    `sudo chown $(whoami):root rabbitmq.conf`
6. Make sure the volume mounts for the rabbitmq service in the `docker-compose.yml` file are correct host paths. 
   1. `cd $PROJECT_ROOT/scripts`
   2. `chmod 755 01_provision_stack.sh`
   3. `sudo ./01_provision_stack.sh`
   4. `cd ../conf/gitlab_compose`
7. Since the RABBITMQ certificate determines the connection string, a local DNS entry will need to be added to point the hostname `rabbitmq` to `127.0.0.1`. 
   1. `echo '127.0.0.1 rabbitmq' | sudo tee -a /etc/hosts`
   2. `echo '127.0.0.1 neo4j' | sudo tee -a /etc/hosts`

# Running a Local Stack
## Running a Pub/Sub Broker and a Graph Data Store
[We currently support Neo4j and RabbitMQ, but intend to expand support for other common infrastructure]

1. Run `docker compose up` in the gitlab_compose directory [$PROJECT_ROOT/conf/gitlab_compose/](/conf/gitlab_compose/) and spawn the NEO4J and RabbitMQ servers. Make sure to auth with your private registry if you're using one.
2. Access the NEO4J web UI via http://localhost:7474 to configure a password. The default user/pass combo is `neo4j:neo4j`. *This will need to be changed before running the rust binaries for the first time.*

## Running the Rust Binaries (Using Gitlab Agent)
0. Install a Rust toolchain, possibly using rustup...
   * Refer here for instructions: https://doc.rust-lang.org/cargo/getting-started/installation.html
1. Change into the project source directory.
   * `cd $PROJECT_ROOT/src`
2. Run `cargo build` to build all the workspace binaries. 
3. To run the consumer and observer do the following:
   1. Change into the `target/debug` directory. 
   2. Remember to source the environment file created early FOR EACH shell. 
      * `source $PROJECT_ROOT/conf/env_setup.sh`
   3. Observer: `./observer_entrypoint`
   4. Consumer: `./consumer_entrypoint`
4. After some time, nodes should start to show up in the Neo4J instance. 

## Fun queries
match (r:GitlabRunner) where r.runner_id = '304' with r  match p=(r)-[:hasJob]->(j:GitlabRunnerJob) where j.status = 'failed' return p as failedjob
