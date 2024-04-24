#!/usr/bin/fish

# Neo4J
mkdir -p ../conf/neo4j_volumes/conf
mkdir -p ../conf/neo4j_volumes/data
mkdir -p ../conf/neo4j_volumes/import
mkdir -p ../conf/neo4j_volumes/logs
mkdir -p ../conf/neo4j_volumes/plugins

cp -r ../conf/neo4j_setup/plugins/* ../conf/neo4j_volumes/plugins
cp ../conf/neo4j_setup/conf/neo4j_setup/neo4j.conf ..conf/neo4j_volumes/conf/neo4j.conf
cp ../conf/neo4j_setup/imports/neo4j_setup/imports* ../conf/neo4j_volumes/import

chown -R 7474:7474 ../conf/neo4j_volumes
