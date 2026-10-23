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

use proptest::prelude::*;
use std::sync::Arc;

proptest! {
    #[test]
    fn payload_survives_arc_wrap(bytes in any::<Vec<u8>>()) {
        let payload = Arc::new(bytes.clone());
        prop_assert_eq!(payload.as_ref(), &bytes);
        prop_assert_eq!(payload.len(), bytes.len());
    }

    #[test]
    fn empty_payload_arc(topic in "[a-z]{1,20}") {
        let payload: Arc<Vec<u8>> = Arc::new(vec![]);
        prop_assert_eq!(payload.len(), 0);
        prop_assert!(!topic.is_empty() && topic.len() <= 20);
    }
}
