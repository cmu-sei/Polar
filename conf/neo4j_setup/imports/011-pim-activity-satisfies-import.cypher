:auto LOAD CSV WITH HEADERS from "file:///111-pim-activity-satisfies.csv" AS line
CALL {
    WITH line
    UNWIND split(line.Satisfies, ',') AS requirements
    MATCH (r:Requirement {Name: requirements})
    WITH line,r
    MATCH (a:Activity { Name: line.Name })
    CREATE (a)<-[:Satisfies]-(r)
} IN TRANSACTIONS OF 500 ROWS;
:auto LOAD CSV WITH HEADERS from "file:///111-pim-activity-satisfies.csv" AS line
CALL {
    WITH line
    UNWIND split(line.Satisfies, ',') AS requirements
    MATCH (r:Requirement {Name: requirements})
    WITH line,r
    MATCH (b:ActivityCategory { Name: line.Name })
    CREATE (b)<-[:Satisfies]-(r)
} IN TRANSACTIONS OF 500 ROWS;
