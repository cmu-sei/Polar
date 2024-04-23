:auto LOAD CSV WITH HEADERS from "file:///112-pim-dso-domains.csv" AS line
CALL {
    with line
    CREATE (n:Domain {
        Name: line.DSO_Domains
    })
} IN TRANSACTIONS OF 500 ROWS;
