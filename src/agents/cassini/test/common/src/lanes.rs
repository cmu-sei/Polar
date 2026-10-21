/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

//! The topics a run uses. Topics are exact names (policy-enforcement.md, "Decisions"), so the harness declares a fixed
//! set of lanes, and a run says how many of them it uses: producer `i` publishes on lane `i % lanes`, and sink `j`
//! subscribes to lane `j % lanes`. One lane is one busy topic; eight are eight topics in parallel.

/// Every lane, in order. The producer declares that it publishes on each, the sink that it subscribes to each.
pub const LANES: [&str; 8] = [
    "cassini.harness.lane.0",
    "cassini.harness.lane.1",
    "cassini.harness.lane.2",
    "cassini.harness.lane.3",
    "cassini.harness.lane.4",
    "cassini.harness.lane.5",
    "cassini.harness.lane.6",
    "cassini.harness.lane.7",
];

/// The lane producer or sink `index` uses, when a run uses `lanes` of them (at least one).
pub fn lane_of(index: u64, lanes: u64) -> usize {
    (index % lanes.max(1)) as usize
}

/// The producers that publish on `lane`, of the `producers` a run has: what a sink on that lane must hear from.
pub fn producers_on(lane: usize, producers: u64, lanes: u64) -> Vec<u32> {
    (0..producers)
        .filter(|p| lane_of(*p, lanes) == lane)
        .map(|p| p as u32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn producers_are_dealt_round_the_lanes() {
        assert_eq!(producers_on(0, 4, 1), vec![0, 1, 2, 3]);
        assert_eq!(producers_on(1, 4, 2), vec![1, 3]);
        assert_eq!(producers_on(3, 2, 4), Vec::<u32>::new());
        assert_eq!(LANES[lane_of(9, 8)], "cassini.harness.lane.1");
    }

    proptest! {
        /// Every producer is on exactly one lane, the one `lane_of` names, and a sink on a lane hears from all of
        /// them: the lanes split the producers, with nothing left over and nothing twice.
        #[test]
        fn the_lanes_split_the_producers(producers in 0u64..40, lanes in 1u64..=8) {
            let mut all: Vec<u32> = (0..lanes as usize)
                .flat_map(|lane| producers_on(lane, producers, lanes))
                .collect();
            all.sort();
            prop_assert_eq!(all, (0..producers as u32).collect::<Vec<_>>());
            for p in 0..producers {
                prop_assert!(producers_on(lane_of(p, lanes), producers, lanes).contains(&(p as u32)));
                prop_assert!(lane_of(p, lanes) < LANES.len());
            }
        }
    }
}
