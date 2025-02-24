Polar (OSS)

Copyright 2024 Carnegie Mellon University.

NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING
INSTITUTE MATERIAL IS FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON
UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER EXPRESSED OR IMPLIED, AS
TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR PURPOSE
OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE
MATERIAL. CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND
WITH RESPECT TO FREEDOM FROM PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

Licensed under a MIT-style license, please see license.txt or contact
permission@sei.cmu.edu for full terms.

[DISTRIBUTION STATEMENT A] This material has been approved for public release
and unlimited distribution.  Please see Copyright notice for non-US
Government use and distribution.

This Software includes and/or makes use of Third-Party Software each subject
to its own license.

DM24-0470

# Running a Local Stack
## Running a Pub/Sub Broker and a Graph Data Store

The docker compose file has been included to ease the setup stage for the development workflow.

**NOTE:** If you haven't already, ensure you''ve either downloaded or built the container images for the gitlab agent using the nix flake in the Cargo workspace, see [the README.md for details on how you can build and package the agents.](../../src/agents/gitlab/README.md)

Once you have the associated images. You should ensure that the enviornment variables' values match up with your desired environment.
(i.e. that TLS certificate paths are accruate, bind addresses and ports are correct, etc.)

You can simply run `docker compose up` to start the cassini mesasge broker and a neo4j instance. Keep in mind that you will still need to set a new password in the neo4j server to be used by the agent. 

**NOTE:** The default user/password combo is `neo4j:neo4j`. This will need to be changed **before** running the rust agent for the first time.

