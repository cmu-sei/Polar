# Gitlab Agent

## Description

TODO:

## Local Development

A local development container has been setup to support rust development for the gitlab agent. The visual studio code extension
*Dev Containers* (`ms-vscode-remote.remote-containers`) needs to be installed on your local machine. It will use docker to spin up 
a compose stack that includes the required dependencies (rabbitmq & neo4j). ***NOTE***: This is strictly for development purposes only, 
no security constrains are implemented.

Prep Steps:
1. Create a copy of the `example.env` named `.env` in the *.devcontainer* directory. 
   1. The .env file is read by the `devcontainer` service in the docker-compose file. 
   2. Additionally it is in the `.gitignore` file.
2. Populate the values contained in the .env file, see the variable descriptions below.
3. Using the VSCode Command Palette (CMD + SHIFT + P on MACOS), type: `> dev containers: Reopen in container` 
4. Any changes to the environment variables will require a dev container rebuild (Until we figure out how to properly set variables in the debugger)

**Required Variables:**

```bash
GITLAB_ENDPOINT=""      # Gitlab Instance URL (i.e. https://gitlab.star.mil)
GITLAB_TOKEN=""         # A Personal Access Token for the instance (Note: The information returned from GItlab will depend on the permissions granted to the token. See Gitlab's REST API docs for more information)
NEO4J_ENDPOINT=""       # For the development container, this should be "neo4j://neo4j:7687"
RABBITMQ_ENDPOINT=""    # For the development container, this should be "rabbitmq"
NEO4J_USER=""           # For the development container, this should be "neo4j"
NEO4J_PASSWORD=""       # For the development container, this should be "neo4j"
NEO4J_DB=""             # For the development container, this should be "neo4j"
```


## Fun queries
match (r:GitlabRunner) where r.runner_id = '304' with r  match p=(r)-[:hasJob]->(j:GitlabRunnerJob) where j.status = 'failed' return p as failedjob
