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


# Feature request: the git observer reports a ref when it moves

Requested 2026-10-31 by the Blitz deployment (a consumer of the fork). Part of it is built on this branch
(`fix/git-observer-report-moved-refs-only`); the rest is the item already in `remaining-work.md`, "Git observer state,
decided 2026-10-12 (not built)", and is left for its owner.

## What was seen

`GitRepoWorker::observe_repository` called `on_ref_update(ref_name, prev, tip)` for every tracked ref on every pass,
after it had worked out that nothing had moved. The git observer therefore published a `RefUpdated` for each ref about
every fifteen minutes (its pass interval) whether or not anyone had pushed, and the event carried `old == Some(new)`.

Its consumers take a `RefUpdated` to mean that a ref moved. Blitz's build-orchestrator built the commit each time: 88
builds of one commit were in the graph, and three more arrived every pass, each costing up to 6 CPUs and 8Gi of the node.
Any consumer that does something per update (a build, a notification, a graph write that is not idempotent) has the same
problem, and each would have to learn to ignore an update whose `old` is its `new`. Blitz's orchestrator now does, as
a safeguard, and also remembers the commits it has taken.

## What is built here

1. **A ref that is where the last pass left it is not reported.** `observe_repository` calls `on_ref_update` only when
   `prev != Some(tip)`; `last_seen` is updated either way. `CommitDiscovered` was already incremental
   (`walk_commits_incremental` stops at `prev`), so it did not repeat. Test:
   `a_ref_that_has_not_moved_is_not_reported_again` (first pass from nothing, a second with nothing moved, a third after the
   ref advances).

## What is not, and is already listed

2. **After a restart the observer still reports every ref from nothing** (`old: None`, with up to `max_depth` commits
   walked again), because `last_seen` starts empty. The observer restarts about hourly as its certificate is renewed, so
   a consumer that builds on `RefUpdated` still builds each ref once an hour unless it remembers what it has built.
   `remaining-work.md` decides the fix: restore the checkpoints from the graph at worker start, by asking the
   processor over Cassini. This request does not change that decision; it adds the cost seen of leaving it, which is one
   build per ref per restart for a consumer that does not remember (Blitz's now does, in memory).

## Open questions for the maintainer

- Should a first report (`old: None`) say so, so that a consumer can tell "new to the observer" from "a ref was created"?
  Today the two look the same.
- The processor's graph writes for `RefUpdated` were not checked for idempotence. With this change they run less often,
  but the restart case still repeats them.
