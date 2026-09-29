docker run --interactive --tty --rm \
  --volume=./latest_var/neo4j_volumes/data:/var/lib/neo4j/data \
  --volume=./latest:/backups \
  neo4j:5.26.2-community \
  neo4j-admin database load --from-path=/backups --overwrite-destination=true neo4j
