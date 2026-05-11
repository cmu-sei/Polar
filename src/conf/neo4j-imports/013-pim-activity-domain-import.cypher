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
