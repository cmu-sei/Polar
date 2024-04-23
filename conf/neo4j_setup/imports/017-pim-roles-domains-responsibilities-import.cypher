:auto LOAD CSV WITH HEADERS from "file:///117-pim-roles-domains-responsibilities.csv" AS line
CALL {
    with line
    CREATE (n:Role {Name: line.Role, Responsibilities: line.Responsibilities})
    with line,n
    match (d:Domain {Name: line.Domain})
    with line, n, d
    CREATE (n)-[r:InDomain]->(d)
} IN TRANSACTIONS OF 500 ROWS;
