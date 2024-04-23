:auto LOAD CSV WITH HEADERS from "file:///113-pim-activity-domain.csv" AS line
CALL {
    WITH line
    UNWIND split(line.Domain, ',') AS the_domain
    MATCH (d:Domain { Name: the_domain })
    MATCH (a:Activity { Name: line.Activity })
    CREATE (a)-[:PerformedIn]->(d)
} IN TRANSACTIONS OF 500 ROWS;

:auto LOAD CSV WITH HEADERS from "file:///113-pim-activity-domain.csv" AS line
CALL {
    WITH line
    UNWIND split(line.Domain, ',') AS the_domain
    MATCH (d:Domain { Name: the_domain })
    MATCH (a:ActivityCategory { Name: line.Activity })
    CREATE (a)-[:PerformedIn]->(d)
} IN TRANSACTIONS OF 500 ROWS;
