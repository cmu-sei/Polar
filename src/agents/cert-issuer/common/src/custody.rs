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

//! The custodian: the custody daemon's own identity, distinct from the workload it keeps keys for
//! (docs/architecture/key-custody.md, "Part A's bootstrap: the custodian").
//!
//! The pod's service-account token buys one thing, the custodian's certificate (`CertType::Custody`), named
//! `custody.<X>` for the workload identity X the token proves. Its `p:custody:` names say what it may ask for on X's
//! behalf. The prefix is reserved: cert-issuer signs a name that starts with it only as a custodian's certificate, for
//! the identity its token proves, so a session named `custody.X` holds X's custodian's certificate.

use crate::permission::{PermissionExtractionError, san_strings};
use std::collections::BTreeSet;

/// What every custodian's identity starts with, and no other certificate's may.
pub const CUSTODIAN_PREFIX: &str = "custody.";

/// The custodian's identity for the workload identity `identity`.
pub fn custodian_identity(identity: &str) -> String {
    format!("{CUSTODIAN_PREFIX}{identity}")
}

/// The workload identity `identity` is the custodian of, if it is a custodian's.
pub fn custodian_of(identity: &str) -> Option<&str> {
    identity
        .strip_prefix(CUSTODIAN_PREFIX)
        .filter(|workload| !workload.is_empty() && !is_reserved(workload))
}

/// Whether `identity` is in the custodians' space: signed only as a custodian's certificate.
pub fn is_reserved(identity: &str) -> bool {
    identity.starts_with(CUSTODIAN_PREFIX)
}

/// What a custodian may ask the credentials agent for on its workload's behalf, carried in its certificate as
/// `p:custody:client`, `p:custody:server` or `p:custody:escrow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CustodyPermission {
    /// The workload's mTLS client certificate.
    Client,
    /// The workload's server certificate (its names are cert-issuer's to allow, on that request).
    Server,
    /// The workload's escrow key: retrieved, or issued and escrowed if there is none.
    Escrow,
}

/// Why a name is not a custody permission.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{0}` is not a custody permission (p:custody:client, p:custody:server or p:custody:escrow)"
)]
pub struct NotACustodyPermission(pub String);

const CUSTODY_PERMISSION_PREFIX: &str = "p:custody:";

impl CustodyPermission {
    pub const ALL: [CustodyPermission; 3] = [Self::Client, Self::Server, Self::Escrow];

    fn word(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
            Self::Escrow => "escrow",
        }
    }

    pub fn to_san_string(self) -> String {
        format!("{CUSTODY_PERMISSION_PREFIX}{}", self.word())
    }

    /// The inverse of [`to_san_string`](Self::to_san_string), exactly: nothing else is one.
    pub fn from_san_string(name: &str) -> Result<Self, NotACustodyPermission> {
        name.strip_prefix(CUSTODY_PERMISSION_PREFIX)
            .and_then(|word| Self::ALL.into_iter().find(|p| p.word() == word))
            .ok_or_else(|| NotACustodyPermission(name.to_string()))
    }
}

/// Whether `name` claims to be a custody permission, well-formed or not: such a name is a custodian's certificate's
/// alone.
pub fn names_custody(name: &str) -> bool {
    name.trim()
        .to_ascii_lowercase()
        .starts_with(CUSTODY_PERMISSION_PREFIX)
}

/// Every custody permission `certificate_pem` carries; its other names are skipped.
pub fn extract_custody_permissions(
    certificate_pem: &str,
) -> Result<BTreeSet<CustodyPermission>, PermissionExtractionError> {
    Ok(san_strings(certificate_pem)?
        .iter()
        .filter_map(|name| CustodyPermission::from_san_string(name).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const X: &str = "kube-consumer.polar.serviceaccount.cluster.local";

    #[test]
    fn a_custodian_is_named_for_its_workload_and_read_back() {
        assert_eq!(
            custodian_identity(X),
            "custody.kube-consumer.polar.serviceaccount.cluster.local"
        );
        assert_eq!(custodian_of(&custodian_identity(X)), Some(X));
        assert_eq!(custodian_of(X), None);
        assert_eq!(custodian_of("custody."), None);
        assert_eq!(
            custodian_of("custody.custody.x"),
            None,
            "a custodian of a custodian is nothing"
        );
        assert!(is_reserved("custody.anything") && !is_reserved(X));
    }

    #[test]
    fn a_custody_permission_is_one_of_three_names_exactly() {
        for permission in CustodyPermission::ALL {
            assert_eq!(
                CustodyPermission::from_san_string(&permission.to_san_string()),
                Ok(permission)
            );
            assert!(names_custody(&permission.to_san_string()));
        }
        for name in [
            "p:custody:",
            "p:custody:Client",
            "p:custody:client ",
            "p:custody:clientx",
            "p:custody:client:more",
            "p:custodyclient",
            "p:sub:polar-authority-updates",
            "custody:client",
        ] {
            assert!(CustodyPermission::from_san_string(name).is_err(), "{name}");
        }
        assert!(
            names_custody(" P:Custody:anything"),
            "a name claiming custody, however written"
        );
        assert!(!names_custody("p:sub:custody"));
    }

    #[test]
    fn a_certificate_s_custody_permissions_are_read_from_its_names() {
        let mut params = rcgen::CertificateParams::new(vec![custodian_identity(X)]).unwrap();
        for name in [
            "p:custody:client",
            "p:custody:escrow",
            "p:sub:polar-authority-updates",
            "p:custody:nope",
        ] {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI(name.try_into().unwrap()));
        }
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        assert_eq!(
            extract_custody_permissions(&pem).unwrap(),
            BTreeSet::from([CustodyPermission::Client, CustodyPermission::Escrow])
        );
    }

    proptest! {
        #[test]
        fn custodian_of_inverts_custodian_identity_for_every_identity_outside_the_reserved_space(
            identity in "[a-z0-9][a-z0-9.-]{0,60}"
        ) {
            prop_assume!(!is_reserved(&identity));
            let custodian = custodian_identity(&identity);
            prop_assert!(is_reserved(&custodian));
            prop_assert_eq!(custodian_of(&custodian), Some(identity.as_str()));
            // and no identity outside the space is anyone's custodian
            prop_assert_eq!(custodian_of(&identity), None);
        }

        #[test]
        fn only_the_three_names_parse_as_custody_permissions(name in "p:custody:[a-z:]{0,12}|.{0,24}") {
            match CustodyPermission::from_san_string(&name) {
                Ok(permission) => prop_assert_eq!(permission.to_san_string(), name),
                Err(_) => prop_assert!(CustodyPermission::ALL.iter().all(|p| p.to_san_string() != name)),
            }
        }
    }
}
