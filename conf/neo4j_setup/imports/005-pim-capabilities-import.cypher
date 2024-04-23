:auto LOAD CSV WITH HEADERS from "file:///105-pim-capabilities.csv" AS line
CALL {
    with line
    CREATE (n:Capability {
        Name: line.Name,
        Definition: line.Definition,
        Process: line.Process
    })
    SET n = line
} IN TRANSACTIONS OF 500 ROWS;
