# Cameo Exports from the SEI-CMU DevSecOps PIM


## Requirements Export

Uses a custom Apache VTL template (Attached to the TWC Model) which exports to an
HTML file containing the requirements with capabilities. The file includes a button
to convert the table into a CSV file, which is what we use for import into
Neo4J. 

To run the export in Cameo, use the Report Wizard or find the report in the
Containment Tree. In the steps, just include all the `System Requirements` in the 
elements to export. All other defaults work. 

## Exporting CSVs

Cameo supports storing elements in a tabular format with columns to represent certain relationships between them. These tables can be exported directly into 
an excel readable format (.xlss or .csv) by clicking the 'export' button in the navbar at the top. It should be noted that there can often be data loss when
columns are exported to .xlss formats, so it's reccomended that csv be the de facto format for exports.
