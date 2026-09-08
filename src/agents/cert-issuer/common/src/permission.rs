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

//! What a certificate's holder is permitted to communicate with --
//! RBAC-style, embedded into the certificate's own SAN set at
//! issuance time, the same way Kubernetes stores RBAC-relevant
//! information in its own client certificates. Lives here, not in
//! credentials-agent-common, because embedding permissions into a
//! certificate's SAN set is fundamentally cert-issuer's own concern;
//! credentials-agent-common depends on this crate already (the
//! reverse would be a backwards dependency).
//!
//! A closed, hard-selected scope first, before any specific target is
//! even considered -- the same "don't fall back to free text when the
//! set is genuinely enumerable" discipline already applied to
//! RevocationReason/GrantReason.

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub enum PermissionScope {
    ServiceToService,
    ServiceToUser,
    UserToService,
    UserToUser,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize)]
pub struct Permission {
    pub scope: PermissionScope,
    /// A specific FQDN this permission is scoped to (e.g.
    /// "cassini"), narrowing it from "any target within this scope"
    /// to one named target. Always None today -- FQDN-scoping
    /// (letting a certificate say "service-to-service, specifically
    /// with cassini") is real, deliberately deferred future work, not
    /// a rejected idea.
    pub target: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_round_trips_through_serde_json() {
        let original = Permission {
            scope: PermissionScope::ServiceToService,
            target: Some("cassini".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: Permission = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn permission_with_no_target_round_trips_through_serde_json() {
        let original = Permission {
            scope: PermissionScope::UserToService,
            target: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: Permission = serde_json::from_str(&json).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn permission_round_trips_through_rkyv() {
        let original = Permission {
            scope: PermissionScope::UserToUser,
            target: None,
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original)
            .unwrap()
            .to_vec();
        let restored: Permission =
            rkyv::from_bytes::<Permission, rkyv::rancor::Error>(&bytes).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn every_scope_round_trips_through_serde_json() {
        for scope in [
            PermissionScope::ServiceToService,
            PermissionScope::ServiceToUser,
            PermissionScope::UserToService,
            PermissionScope::UserToUser,
        ] {
            let original = Permission {
                scope,
                target: None,
            };
            let json = serde_json::to_string(&original).unwrap();
            let restored: Permission = serde_json::from_str(&json).unwrap();
            assert_eq!(original, restored);
        }
    }
}
