# Polar (OSS)

# Copyright 2024 Carnegie Mellon University.

# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
# INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
# UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
# TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
# OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
# MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
# WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

# Licensed under a MIT-style license, please see license.txt or contact
# permission@sei.cmu.edu for full terms.

# [DISTRIBUTION STATEMENT A] This material has been approved for public release
# and unlimited distribution.  Please see Copyright notice for non-US
# Government use and distribution.

# This Software includes and/or makes use of Third-Party Software each subject
# to its own license.

# DM24-0470

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
