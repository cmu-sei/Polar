# Gitlab Agent

Prep Steps:
1. Create a copy of the `example.env` named `.env` in the *dev_compose* directory. 
   1. The .env file is read by the service in the docker-compose file. 
   2. Additionally it is in the `.gitignore` file.
2. Populate the values contained in the .env file, see the variable descriptions below.
3. Any changes to the environment variables will require a container restart of the services.

**Required Variables:**
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
```

## Fun queries
match (r:GitlabRunner) where r.runner_id = '304' with r  match p=(r)-[:hasJob]->(j:GitlabRunnerJob) where j.status = 'failed' return p as failedjob
