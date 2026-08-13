#!/bin/bash

# Polar (OSS)

# Copyright 2024 Carnegie Mellon University.

# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
# INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
# UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
# TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
# OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
# MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
# WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

# Licensed under a MIT-style license, please see license.txt or contact
# permission@sei.cmu.edu for full terms.

# [DISTRIBUTION STATEMENT A] This material has been approved for public release
# and unlimited distribution.  Please see Copyright notice for non-US
# Government use and distribution.

# This Software includes and/or makes use of Third-Party Software each subject
# to its own license.

# DM24-0470

set -e

work_dir=$(pwd)
scripts_dir='backup_scripts'
project_base='some_dir'

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

# Perform DB backup.
echo "Backing up Neo4j..."

# Stop the container.
docker stop $neo4j_container

the_date=$(date --iso-8601=seconds)
mkdir -p $(pwd)/$the_date
sudo chown -R 7474:7474 $(pwd)/$the_date

# Take backup.
docker run --rm --volumes-from $neo4j_container -v $(pwd):/backup neo4j:latest /var/lib/neo4j/bin/neo4j-admin database dump --to-path=/backup/$the_date --verbose neo4j

docker run --rm --volumes-from $neo4j_container -v $(pwd):/backup neo4j:latest /var/lib/neo4j/bin/neo4j-admin database dump --to-path=/backup/$the_date --verbose system

# Start the container.
docker start $neo4j_container
