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


There are some first time components before `docker compose up` can be ran. When doing local development, the rust binaries should be compiled and ran locally outside a container. More will be explained further down. 

**Prep Steps:**
1. Create a copy of the `example.env` named `.env` in the *conf/gitlab_compose* directory. 
   1. The .env file is read by the service in the docker-compose file. 
   2. Additionally it is in the `.gitignore` file.
2. Populate the values contained in the .env file, see the variable descriptions below.
3. Any changes to the environment variables will require a container restart of the services.

## Required Variable
This tool requires the following values to be present in your environment as
variables - some of which may contain sensitive data and should be stored
securely. See your team about how to retrieve this information.

```bash
# Gitlab Instance URL (i.e. https://gitlab.star.mil)
# The service endpoint of the gitlab api you wish to pull information from NOTE: 
# The endpoint must end in /api/v4
GITLAB_ENDPOINT=""

# A Personal Access Token for the instance (Note: The information returned from 
# GItlab will depend on the permissions granted to the token.
# See Gitlab's REST API docs for more information)
GITLAB_TOKEN=""

# The service endpoint of the given neo4j instance.
# For the development container, this should be "neo4j://neo4j:7687"
NEO4J_ENDPOINT=""

# The service endpoint of the rabbitmq instance. Should be prefixed in amqp://
# For the development container, this should be "rabbitmq"
RABBITMQ_ENDPOINT=""

# For the development container, this should be "neo4j"
NEO4J_USER=""

# For the development container, this should be "neo4j"
NEO4J_PASSWORD=""

# For the development container, this should be "neo4j"
NEO4J_DB=""

# The absolute file path to the client .p12 file. This is used by the Rust binaries to auth with RabbitMQ via TLS.
TLS_CLIENT_KEY="" 

# If a password was set for the .p12 file, put it here.
TLS_KEY_PASSWORD=""

# The absolute file path to the client .p12 file. This is used by the Rust binaries to auth with RabbitMQ via TLS. 
TLS_CLIENT_KEY=""

# The absolute file path to the ca_certificates.pem file created by TLS_GEN. Used by the Rust binaries to auth with RabbitMQ via TLS.
TLS_CA_CERT=""

# The absolute file path to the observer_config.yaml. One is available in the following dir: ./gitlab_agent/src/observer/src/observer_config.yaml
GITLAB_OBSERVER_CONFIG=""
```

## Setup
**Note to Mac Users: You must install openssl before completing the steps below**
1. SSL Files will need to be generated for the RabbitMQ server. The server is expecting them at specific locations, which should be an `ssl` directory in the same directory as this README file. 
2. Using the tls-gen created by RabbitMQ ( repo here: https://github.com/rabbitmq/tls-gen ), generate the certificates. There are instructions in the `basic` directory there, but here is the basics:
   1. Clone the repo and change into the `basic` directory
   2. Run `make CN=rabbitmq` to generate the basic certificates.
   3. Copy the contents of the created `results` directory to the `ssl` one created in the same directory as this README. 
   3.1. `mkdir $PROJECT_ROOT/conf/gitlab_compose/ssl`
   3.2. `cp results/* $PROJECT_ROOT/conf/gitlab_compose/ssl`
3. Due to a bug in a Rust Library, the client p12 file created will need to be converted to a legacy file using openssl. Change the `rabbitmq` portion of the command to whatever **CN** was used in the make command above. 
   1. Change into the `ssl` directory
   1.1. `cd $PROJECT_ROOT/conf/gitlab_compose/ssl`
   1.2. `cp client_rabbitmq.p12 client_rabbitmq.p12.original`
   2. Run the following command (Overwriting the existing `client_<CN>.p12` file): 
```bash
openssl pkcs12 -legacy -export -inkey client_rabbitmq_key.pem -in client_rabbitmq_certificate.pem -out client_rabbitmq.p12 -passout pass:""
```
4. If running on Linux, the server certificate files will need to have their permissions changed. Refer to the following link:
    * Section: **Permission of SSL/TLS certificate and key files**
    * https://github.com/bitnami/containers/tree/main/bitnami/rabbitmq#permission-of-ssltls-certificate-and-key-files
    `sudo chown 1001:root *`
    `sudo chmod 400 *`
    `cd ../`
    `sudo chown 1001:root rabbitmq.conf`
5. Make sure the volume mounts for the rabbitmq service in the `docker-compose.yml` file are correct host paths. 
5.1. `cd $PROJECT_ROOT/scripts`
5.2. `chmod 755 01_provision_stack.sh`
5.3. `sudo ./01_provision_stack.sh`
5.4. `cd ../conf/gitlab_compose`
6. Since the RABBITMQ certificate determines the connection string, a local DNS entry will need to be added to point the hostname `rabbitmq` to `127.0.0.1`. 
6.1. `echo '127.0.0.1 rabbitmq' | sudo tee -a /etc/hosts`
6.1. `echo '127.0.0.1 neo4j' | sudo tee -a /etc/hosts`
7. Run the following command:`sudo scripts/01_provision_stack.sh`
7. Run `docker compose up` in the development-compose directory and spawn the NEO4J and RabbitMQ servers. Make sure to auth with your private registry if you're using one.
8. Access the NEO4J web UI via http://localhost:7474 to configure a password. The default user/pass combo is `neo4j:neo4j`. This will need to be changed before running the rust binaries for the first time. 


### Running the Rust Binaries (Using Gitlab Agent)
0. Install a Rust toolchain, possibly using rustup...
1. Change into the project source directory. Make sure cargo (Rust package manager) is installed.  
1.1. `cd $PROJECT_ROOT/src`
   * Refer here for instructions: https://doc.rust-lang.org/cargo/getting-started/installation.html
2. Run `cargo build` to build all the workspace binaries. 
3. To run the consumer and observer do the following:
   1. Change into the `target/debug` directory. 
   2. Remember to source the `.env` file created early FOR EACH shell. 
   3. Observer: `./observer_entrypoint`
   4. Consumer: `./consumer_entrypoint`
4. After some time, nodes should start to show up in the Neo4J instance. 



## Fun queries
match (r:GitlabRunner) where r.runner_id = '304' with r  match p=(r)-[:hasJob]->(j:GitlabRunnerJob) where j.status = 'failed' return p as failedjob
