## Development Compose Setup

There are some first time components before `docker compose up` can be ran. When doing local development, the rust binaries should be compiled and ran locally outside a container. More will be explained further down. 

**Setup:**

1. SSL Files will need to be generated for the RabbitMQ server. The server is expecting them at specific locations, which should be an `ssl` directory in the same directory as this README file. 
2. Using the tls-gen created by RabbitMQ (repo here: https://github.com/rabbitmq/tls-gen), generate the certificates. There are instructions in the `basic` directory there, but here is the basics:
   1. Clone the repo and change into the `basic` directory
   2. Run `make CN=rabbitmq` to generate the basic certificates.
   3. Copy the contents of the created `results` directory to the `ssl` one created in the same directory as this README. 
4. Due to a bug in a Rust Library, the client p12 file created will need to be converted to a legacy file using openssl. Change the `rabbitmq` portion of the command to whatever **CN** was used in the make command above. 
   1. Change into the `ssl` directory
   2. Run the following command (Overwriting the existing `client_<CN>.p12` file): 
```bash
openssl pkcs12 -legacy -export -inkey client_rabbitmq_key.pem -in client_rabbitmq_certificate.pem -out client_rabbitmq.p12 -passout pass:""
```
5. Make sure the volume mounts for the rabbitmq service in the `docker-compose.yml` file are correct host paths. 
6. If running on Linux, the server certificate files will need to have their permissions changed. Refer to the following link:
    * Section: **Permission of SSL/TLS certificate and key files**
    * https://github.com/bitnami/containers/tree/main/bitnami/rabbitmq#permission-of-ssltls-certificate-and-key-files
7. Since the RABBITMQ certificate determines the connection string, a local DNS entry will need to be added to point the hostname `rabbitmq` to `127.0.0.1`. 
8. Configure a `.env` file. Every shell created to run the rust binaries will need these variables, best to add an export in front of the variables. Explainer of the variables below:
```bash
export GITLAB_TOKEN="" # The gitlab personal access token to use for the observer
export GITLAB_ENDPOINT=https://gitlab.com/api/v4 # The API endpoint for the Gitlab instance to observer (Make sure to include /api/v4 to the end)
export NEO4J_ENDPOINT=neo4j://localhost # The NEO4J, should probably be this value.
export RABBITMQ_ENDPOINT=amqps://rabbitmq:5671/%2f?auth_mechanism=external # The rabbitmq endpoint, it's probably this value.
export NEO4J_USER=neo4j
export NEO4J_PASSWORD="" # This will need to be set on the first creation of the neo4j instance, done through the web UI at localhost:7474. The default password is neo4j. 
export NEO4J_DB=neo4j
export TLS_CLIENT_KEY="" # The absolute file path to the client .p12 file. This is used by the Rust binaries to auth with RabbitMQ via TLS. 
export TLS_KEY_PASSWORD="" # If a password was set for the .p12 file, put it here.
export TLS_CA_CERT="" # The absolute file path to the ca_certificates.pem file created by TLS_GEN. Used by the Rust binaries to auth with RabbitMQ via TLS.
export GITLAB_OBSERVER_CONFIG="" # The absolute file path to the observer_config.yaml. One is available in the following dir: ./gitlab_agent/src/observer/src/observer_config.yaml
```
9. Run `docker compose up` in the development-compose directory and spawn the NEO4J and RabbitMQ servers. Make sure to auth with your private registry if you're using one.
10. Access the NEO4J web UI via http://localhost:7474 to configure a password. The default user/pass combo is `neo4j:neo4j`. This will need to be changed before running the rust binaries for the first time. 


### Running the Rust Binaries (Using Gitlab Agent)
1. Change into the `gitlab_agent` directory. Make sure cargo (Rust package manager) is installed.  
   * Refer here for instructions: https://doc.rust-lang.org/cargo/getting-started/installation.html
2. Run `cargo build` to build all the workspace binaries. 
3. To run the consumer and observer do the following:
   1. Change into the `target/debug` directory. 
   2. Remember to source the `.env` file created early FOR EACH shell. 
   3. Observer: `./observer_entrypoint`
   4. Consumer: `./consumer_entrypoint`
4. After some time, nodes should start to show up in the Neo4J instance. 