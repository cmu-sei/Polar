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

//! Whether a verified signer may make this change.
//!
//! Version one is a capability in the signer's certificate. The rule sits behind a trait that is given
//! the payload as well as the signer, so a policy engine can later decide per agent and per variable (the
//! capability set is a closed list that cannot name either) without touching verification.

use crate::verify::SignerInfo;
use cert_issuer_common::capability::Capability;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AuthorityRefusal(pub String);

pub trait Authority: Send + Sync {
    fn authorize(&self, signer: &SignerInfo, payload: &Value) -> Result<(), AuthorityRefusal>;
}

/// The signer's certificate must carry one capability, whatever the payload.
pub struct RequireCapability(pub Capability);

impl RequireCapability {
    /// The capability meant for this: submitting a policy update.
    pub fn update_policy() -> Self {
        Self(Capability::UpdatePolicy)
    }
}

impl RequireCapability {
    /// The capability meant for changing configuration. Not `update-policy`: that is for Cedar policy.
    pub fn update_configuration() -> Self {
        Self(Capability::UpdateConfiguration)
    }

    /// The capability meant for changing what an agent works on (a runtime document). Neither
    /// `update-configuration`, which changes how it runs, nor `update-policy`.
    pub fn update_runtime() -> Self {
        Self(Capability::UpdateRuntime)
    }

    /// The capability meant for running commands.
    pub fn run_command() -> Self {
        Self(Capability::RunCommand)
    }
}

impl Authority for RequireCapability {
    fn authorize(&self, signer: &SignerInfo, _payload: &Value) -> Result<(), AuthorityRefusal> {
        if signer.capabilities.contains(&self.0) {
            Ok(())
        } else {
            Err(AuthorityRefusal(format!(
                "the signer ({}) does not hold {}",
                signer
                    .username
                    .as_deref()
                    .or(signer.identity.as_deref())
                    .unwrap_or("unknown"),
                self.0.to_san_string()
            )))
        }
    }
}
