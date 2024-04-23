We found limitations to import via Neo4J cypher alone. Overcoming automated
import, at least in our case, meant creating a custom binary for this purpose.
This small rust project will create an executable binary that loads certain
types of PIM data.
