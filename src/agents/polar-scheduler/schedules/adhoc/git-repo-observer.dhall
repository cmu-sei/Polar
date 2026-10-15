-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

let TimeSpec = < Periodic : { interval : Natural, unit : < Minutes | Hours | Days > }
               | Exact : { timestamp : Text }
               | Daily : { hour : Natural, minute : Natural, timezone : Optional Text }
               | Weekly : { day : < Mon | Tue | Wed | Thu | Fri | Sat | Sun >,
                            hour : Natural, minute : Natural, timezone : Optional Text }
               | Monthly : { day : Natural, hour : Natural, minute : Natural,
                             timezone : Optional Text }
               >

let ScheduleKind = < Permanent | Adhoc | Ephemeral >

let ScheduleMetadata = { description : Optional Text, owner : Optional Text }

in  { id = "adhoc/git-repo-observer"
    , kind = ScheduleKind.Adhoc
    , agent_id = None Text
    , agent_type = Some "git-repo-observer"
    , schedule = TimeSpec.Periodic { interval = 5, unit = < Minutes | Hours | Days >.Minutes }
    , config = { repoUrl = "https://example.com/repo.git", branch = "main" }
    , metadata = { description = Some "Observes a git repo every 5 minutes", owner = Some "test" }
    , version = 1
    }
