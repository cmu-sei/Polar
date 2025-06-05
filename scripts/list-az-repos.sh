#!/bin/bash
# In the absence of an agent to monitor azure,
# this is how we can do a quick and dirty scrape of an azure registry to get some repos and tags
ACR_NAME="sandboxaksacr"
CYPHER_OUT="import.cypher"

echo "// Generated Cypher to represent Azure Container Registry images and tags" > "$CYPHER_OUT"
echo "BEGIN;" >> "$CYPHER_OUT"

cat <<EOF >> $CYPHER_OUT
MATCH (c:PodContainer) WHERE c.image IS NOT NULL
WITH c, split(c.image, "/")[0] AS registryHost
MERGE (acr:ContainerRegistry {hostName: registryHost})
MERGE (c)-[:PULLED_FROM]->(acr)
EOF

for repo in $(az acr repository list --name $ACR_NAME --output tsv); do
    tags=$(az acr repository show-tags --name $ACR_NAME --repository "$repo" --output tsv)
    for tag in $tags; do
        cat <<EOF >> "$CYPHER_OUT"
MERGE (tag:ContainerImageTag {name: "$tag"})
MERGE (repo)-[:CONTAINS_TAG]->(tag);
EOF
    done
done

echo "COMMIT;" >> "$CYPHER_OUT"
