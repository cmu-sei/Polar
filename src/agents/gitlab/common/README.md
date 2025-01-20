# Gitlab Observer Common Utilities 
This crate provides some shared untilities for the gitlab agent and its components.


## Features
 - **Graphql Schema** :Generated using the `cynic` CLI tool.
    Over time, the gitlab API schema may be updated, when this happens, we can regenerate it with the following command

    `cynic introspect -H "PRIVATE-TOKEN: $GITLAB_TOKEN" "${GITLAB_GRAPHQL_ENDPOINT}" -o schema/gitlab.graphql`

    Refer to (the cynic docs)[https://cynic-rs.dev/schemas/introspection] for more details on how this is done.


