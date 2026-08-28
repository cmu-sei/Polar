/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

//! MOCK -- NOT REAL CRYPTOGRAPHIC MATERIAL.
//!
//! credentials-agent-architecture.md's secret-escrow mechanism (a real,
//! dedicated escrow keypair -- deliberately separate from this agent's
//! own mTLS client certificate -- that other agents encrypt secrets
//! to) is fully designed but not yet built: no real keypair generation,
//! no private-key custody, no rotation, no identity-distribution topic
//! pair. This exists purely so IssuanceReply::Issued has *something*
//! syntactically present in its escrow_public_key_pem field while the
//! rest of today's issuance flow gets built and proven end to end.
//!
//! TRACKED FOR REMOVAL: replace with real escrow keypair generation
//! and custody at the next available opportunity after today's build
//! is otherwise complete -- not deferred indefinitely.
pub fn mock_escrow_public_key_pem() -> String {
    "-----BEGIN PUBLIC KEY-----\nMOCK-NOT-REAL-KEY-MATERIAL-see-escrow.rs-doc-comment\n-----END PUBLIC KEY-----\n".to_string()
}
