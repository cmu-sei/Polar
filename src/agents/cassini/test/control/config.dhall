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

{-
  TestPlan.dhall

  A load- and integration-testing oriented harness configuration.
  The harness executes phases sequentially.
  Each phase starts from a clean slate unless explicitly configured otherwise.
-}


let Pattern =
      < Drip : { idle_time_seconds : Natural }
      | Burst : { burst_size : Natural, idle_time_seconds : Natural }
      >

let Producer =
      { topic : Text
      , message_size : Natural
      , duration : Natural
      , pattern : Pattern
      }

let Expectation =
      < AtLeast : { messages : Natural, within_seconds : Natural }
      | NoStarvation : { max_idle_seconds : Natural }
      | OrderingPreserved : { key : Text }
      >

let Test =
      { name : Text
      , producer : Producer
      --, expectations : List Expectation
      }

let TestPlan = { name : Text, tests : List Test }

--TODO: Rather than provide a list of producer configuration, which really act as
--multiple actors. We should have just one producer configuration that should contain the same params
--This will simplify the producer agent, as now it will not have to supervise multiple "producer" actors
--that all used the same client.
let producer =
       { topic = "steady"
        , message_size = 128
        , duration = 10
        , pattern = Pattern.Drip { idle_time_seconds = 2000 }
        }


--let expectations = [ Expectation.AtLeast { messages = 1, within_seconds = 30 } ]

let tests = [ { name = "load test", producer } ]

let plan
    : TestPlan
    = { name = "Polar test plan", tests }

in  plan
