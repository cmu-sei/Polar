:auto LOAD CSV WITH HEADERS from "file:///109-pim-activity-categories.csv" AS line
CALL {
    with line
    MERGE (:ActivityCategory { Name: line.Name })
} IN TRANSACTIONS OF 500 ROWS;
