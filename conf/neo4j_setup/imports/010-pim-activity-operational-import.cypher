:auto LOAD CSV WITH HEADERS from "file:///110-pim-activity-operational.csv" AS line
CALL {
    with line
    MERGE (:Activity { Name: line.Name })
} IN TRANSACTIONS OF 500 ROWS;
