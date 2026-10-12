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
