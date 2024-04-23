#!/bin/bash

set -e

work_dir=$(pwd)
scripts_dir='backup_scripts'
project_base='some_dir'

args=("$@")
echo "Backup file: " ${args[0]}

backup_file=${args[0]}

# Get the container ID.
neo4j_container=$(docker ps -a | grep -i neo4j | cut -d ' ' -f 1)

# Backup up the configuration directory
if [[ "$work_dir" == *"$scripts_dir" ]]; then
    echo "We are in the backup scripts directory."
    cd ..
elif [[ "$work_dir" == *"$project_base" ]]; then
    echo "We are in the project base directory."
else
    echo "This script should be run from the project base directory or the scripts directory. Exiting."
    exit;
fi

# Perform DB restore.

echo "Stopping the container..."
docker stop $neo4j_container

echo "Restoring Neo4J from backup..."
docker run --rm --volumes-from $neo4j_container -v $(pwd):/backup neo4j:latest /var/lib/neo4j/bin/neo4j-admin database load --from-path=/backup/$backup_file --verbose --overwrite-destination=true neo4j

docker run --rm --volumes-from $neo4j_container -v $(pwd):/backup neo4j:latest /var/lib/neo4j/bin/neo4j-admin database load --from-path=/backup/$backup_file --verbose --overwrite-destination=true system

echo "Restarting the container..."
docker start $neo4j_container
