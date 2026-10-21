<!--
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
-->

# Cassini load harness

Two builder agents that measure the broker from inside the cluster (`docs/architecture/policy-enforcement.md`, stage
5a): `harness-producer` sends a run's messages, and `harness-sink` receives and measures them. Each is one binary per
workload, with a catalog entry that is added to a target only when it is being measured. A run is a set of Kubernetes
Jobs made from the catalog's rendered pod spec, given the run's parameters as environment variables;
`infra/load-harness.nu` starts the sinks, waits until each says it is ready, starts the producers, and collects one
result line from each.

## The pieces

- `common` (`harness-common`): what both share, all of it pure and tested there.
  - `lanes`: the eight topics a run can use, `cassini.harness.lane.0` to `.7`. Producer `i` publishes on lane
    `i mod HARNESS_LANES`, sink `j` subscribes to lane `j mod HARNESS_LANES`.
  - `envelope`: a 20-byte header (producer, sequence number, send time in nanoseconds) in front of a payload made once
    per run, so the sink reads what it measures without decoding the payload.
  - `schedule`: message `i` is due `i / rate` seconds in; every due message is sent on each wake, and never more than
    the window is unacknowledged.
  - `stats`: nearest-rank percentiles over every latency, and a tally per producer of what arrived, lost, repeated and
    out of order.
  - `settings`: what both are told about the run: `HARNESS_RUN_ID`, `HARNESS_INDEX`, `HARNESS_PRODUCERS`,
    `HARNESS_LANES`, `HARNESS_COUNT`.
  - `report`: the `HARNESS LOAD RESULT` line (its mark, then one line of JSON with a `role`), and the sink's
    `HARNESS LOAD READY` line.
- `producer` (`harness-producer`): once registered, sends `HARNESS_COUNT` messages of `HARNESS_MESSAGE_SIZE` bytes at
  `HARNESS_RATE` a second (0 is as fast as the window allows), with at most `HARNESS_WINDOW` messages (and `HARNESS_WINDOW_BYTES`) unacknowledged; gives
  up on the rest when nothing is acknowledged for `HARNESS_STALL_SECS`. It reports what it sent, what was acknowledged,
  refused or failed, and the rate it reached.
- `sink` (`harness-sink`): subscribes to its lane, says it is ready once the broker acknowledges the subscription, and
  measures every message from its lane's producers, the first `HARNESS_WARMUP` of each left out of the latencies. It
  reports when it has them all, or when none has come for `HARNESS_IDLE_SECS` (`HARNESS_FIRST_WAIT_SECS` for the first).

Each prints every variable it reads with `--scaffold`, and what it does with `--help`. Both exit 0 when their part of a
run is done, and 1 for anything else.

Latency is the sink's clock less the producer's, so it is right only when both run on one node.
