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

//! Repairs of what a write path got wrong before it was fixed, run by the writer's graph controller at start
//! (docs/architecture/graph-write-path.md, "Cleaning up what was written"). Each is a fixed statement reviewed here,
//! never a caller's Cypher: the fields are private, so an agent can only pick one of the constants below. Each is
//! idempotent (a second start finds nothing), counted and logged, and removes only what its rule names.

use neo4rs::{Graph, Query};
use tracing::{debug, info};

/// A repair: its name, what it does, and its statement, which handles at most one batch and returns how many nodes it
/// removed as `removed`; it is run again until a batch removes fewer than [`BATCH`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repair {
    name: &'static str,
    what: &'static str,
    statement: &'static str,
}

/// The most nodes one statement of a repair removes, so a large graph is repaired in transactions of bounded size.
pub const BATCH: i64 = 5000;

/// The empty nodes kube-consumer's state transitions made before `KubeNodeKey::State` bound its variable (35,185 on
/// u7c, 2026-11-01): a node with no label and no property, whose every relationship comes into it and is an OF_TYPE
/// (from a state instance) or a HAS_STATE (from a resource). Each instance's OF_TYPE and each resource's HAS_STATE is
/// pointed at the one `:State` node instead, as the fixed transition writes them (MERGE, so a resource that already
/// points there keeps one), and the empty node is removed. Nothing with a label, a property, an outgoing relationship
/// or any other relationship is touched.
pub const EMPTY_STATE_TYPE_NODES: Repair = Repair {
    name: "empty-state-type-nodes",
    what: "empty nodes left as state types by unbound (:State) transitions",
    statement: "
        MATCH (n)
        WHERE size(labels(n)) = 0 AND size(keys(n)) = 0
          AND NOT EXISTS { MATCH (n)-->() }
          AND NOT EXISTS { MATCH (n)<-[r]-() WHERE NOT type(r) IN ['OF_TYPE', 'HAS_STATE'] }
        WITH n LIMIT 5000
        OPTIONAL MATCH (instance)-[:OF_TYPE]->(n)
        WITH n, collect(DISTINCT instance) AS instances
        OPTIONAL MATCH (resource)-[:HAS_STATE]->(n)
        WITH n, instances, collect(DISTINCT resource) AS resources
        MERGE (state:State)
        FOREACH (i IN instances | MERGE (i)-[:OF_TYPE]->(state))
        FOREACH (r IN resources | MERGE (r)-[:HAS_STATE]->(state))
        DETACH DELETE n
        RETURN count(*) AS removed
    ",
};

impl Repair {
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Runs the repair to the end, a batch a transaction, and logs how many nodes it removed (info when any, debug
    /// when none, as at every start after the first). Returns that number; `Err` when the graph refused or could not
    /// be reached, with what was removed before it kept.
    pub async fn run(&self, graph: &Graph) -> Result<i64, String> {
        let mut total = 0;
        loop {
            let mut rows = graph
                .execute(Query::new(self.statement.to_string()))
                .await
                .map_err(|e| format!("repair {}: {e}", self.name))?;
            let removed = match rows
                .next()
                .await
                .map_err(|e| format!("repair {}: {e}", self.name))?
            {
                Some(row) => row.get::<i64>("removed").map_err(|e| e.to_string())?,
                None => 0,
            };
            total += removed;
            if removed < BATCH {
                break;
            }
        }
        if total > 0 {
            info!("repair {}: removed {total} {}", self.name, self.what);
        } else {
            debug!("repair {}: nothing to remove", self.name);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The batch named in the statement is the one the loop stops on.
    #[test]
    fn the_statement_s_batch_is_the_loop_s() {
        assert!(
            EMPTY_STATE_TYPE_NODES
                .statement
                .contains(&format!("LIMIT {BATCH}"))
        );
    }

    /// It deletes only nodes with no label and no property, and nothing that has an outgoing relationship or an
    /// incoming one other than the two the transition wrote.
    #[test]
    fn the_rule_names_only_empty_state_targets() {
        let s = EMPTY_STATE_TYPE_NODES.statement;
        for clause in [
            "size(labels(n)) = 0",
            "size(keys(n)) = 0",
            "NOT EXISTS { MATCH (n)-->() }",
            "NOT type(r) IN ['OF_TYPE', 'HAS_STATE']",
        ] {
            assert!(s.contains(clause), "{clause}");
        }
        assert_eq!(s.matches("DELETE").count(), 1);
    }
}
