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

let Types = ../types.dhall

in  { agentId = "jenkins-observer-1"
    , schedule = Types.TimeSpec.Periodic { interval = 5, unit = < minutes | hours | days >.minutes }
    , rejuvenation = Some { interval = 24, unit = < minutes | hours | days >.hours }
    , config = { jenkinsUrl = "https://jenkins.example.com"
               , jobPattern = Some ".*"
               , outputTopic = "observations.jenkins"
               , credentials = Types.Credentials.reference "jenkins-cred"
               }
    , metadata = Some { description = "Observes all Jenkins jobs every 5 minutes"
                      , owner = "team-a"
                      , version = Some 1
                      }
    }
