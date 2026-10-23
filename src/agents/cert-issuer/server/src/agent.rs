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

//! What cert-issuer is, as the infrastructure asks to be told: see `polar_agent_builder::PolarAgent`
//! (docs/architecture/cert-issuer-isolation.md, "1b in detail").

use crate::supervisor::{CertIssuerSupervisor, SERVICE_NAME, SHUTDOWN_BUDGET};
use polar_agent_builder::{Capability, Configured, PolarAgent, StartError};
use polar_env::Section;
use ractor::Actor;
use std::time::Duration;

pub struct CertIssuer;

impl PolarAgent for CertIssuer {
    const NAME: &'static str = SERVICE_NAME;
    const PURPOSE: &'static str = "Polar's certificate authority front: signs the certificates the floor allows on a \
        token alone, and those the credentials agent vouches for";
    const NEEDS: &'static [Capability] = &[Capability::Cassini];
    // Cassini's certificate comes from cert-issuer, so cert-issuer cannot depend on Cassini; it speaks over it once it
    // is up (docs/architecture/cert-issuer-isolation.md, "The model", revised 2026-10-23).
    const DEPENDS_ON_CASSINI: bool = false;
    // A probe over Cassini would make its health wait on the broker.
    const SELF_TEST: bool = false;
    const SHUTDOWN_BUDGET: Duration = SHUTDOWN_BUDGET;
    const SECTIONS: &'static [&'static Section] = &[&crate::settings::SECTION];
    const HELP: Option<&'static str> = Some(
        "cert-issuer signs Polar's certificates. A caller with only a service account token is signed what the floor \
         (CERT_ISSUER_BOOTSTRAP) writes down for its workload; the credentials agent, by its verified mTLS \
         certificate, vouches for every other workload and for accounts (/internal/issue). It signs its own client \
         certificate the same way, on its own token, through its own floor. It serves over TLS whether or not \
         Cassini is up, and its health does not count the broker.",
    );

    async fn start(configured: Configured) -> Result<(), StartError> {
        configured.init_logging(SERVICE_NAME);
        // Required by rustls when more than one provider is in the dependency tree.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (_supervisor, handle) = Actor::spawn(
            Some(format!("{SERVICE_NAME}.supervisor")),
            CertIssuerSupervisor,
            configured,
        )
        .await
        .map_err(|e| format!("the cert-issuer supervisor did not start: {e}"))?;
        handle
            .await
            .map_err(|e| format!("the cert-issuer supervisor task failed: {e}"))?;
        Err("the cert-issuer supervisor stopped".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    polar_agent_builder::polar_agent_tests!(CertIssuer);
}
