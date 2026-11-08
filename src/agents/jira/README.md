ACE Tool

Copyright 2026 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
distribution.  Please see Copyright notice for non-US Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject to its own license.

DM26-0670

# Jira agents

- `observe/` (jira-observer): polls a Jira site (Cloud or Data Center/Server) for projects, groups, users and issues and
  publishes them to Cassini.
- `process/` (jira-processor): writes what the observer publishes into the graph.
- `common/`: the message and graph types the two share. `query/`: queries over the Jira nodes.

How to connect a site (the six variables, where a target sets them, the token's Secret, Cloud vs Data Center auth), what
the observer does when Jira fails, and how to check a connection (`nu infra/jira-check.nu <target>`) are in
[docs/architecture/jira.md](../../../docs/architecture/jira.md). `config/` holds leftover files from an earlier layout;
nothing reads them.
