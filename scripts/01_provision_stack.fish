#!/usr/bin/fish

# Neo4J
mkdir -p neo4j_volumes/conf
mkdir -p neo4j_volumes/data
mkdir -p neo4j_volumes/import
mkdir -p neo4j_volumes/logs
mkdir -p neo4j_volumes/plugins

cp -r ../conf/neo4j_setup/plugins/* neo4j_volumes/plugins
cp ../conf/neo4j_setup/conf/neo4j.conf neo4j_volumes/conf/neo4j.conf
cp ../conf/neo4j_setup/imports/import* neo4j_volumes/import
