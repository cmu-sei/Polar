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

//! An agent is the workload it runs as (docs/architecture/policy-enforcement.md, "Decisions"): its
//! service name is a constant in its code, one binary per workload, and its topics, its documents and
//! its policy are all named by it. So an agent starts only if that name is the workload its own
//! certificate names (`<workload>.<namespace>.serviceaccount.<domain>`): an image deployed as another
//! workload, or given another workload's certificate, would answer to a name its identity does not have.

use cert_issuer_common::identity::workload_of;
use cert_issuer_common::permission::extract_own_identity;

/// Whether an agent named `service` may run with the certificate identity `identity`, and if not, why.
pub(crate) fn judge(service: &str, identity: &str) -> Result<(), String> {
    match workload_of(identity) {
        Some(workload) if workload == service => Ok(()),
        Some(workload) => Err(format!(
            "this agent is `{service}`, but its certificate is the workload `{workload}`'s \
             (`{identity}`): an agent runs only as the workload it is named for"
        )),
        None => Err(format!(
            "this agent is `{service}`, but its certificate's identity `{identity}` is not a \
             workload's (`<workload>.<namespace>.serviceaccount.<domain>`)"
        )),
    }
}

/// The agent's own certificate, once judged: the identity it names, and its text (its `g:` groups count in the
/// agent's own decisions).
#[derive(Debug)]
pub(crate) struct OwnCertificate {
    pub(crate) identity: String,
    pub(crate) pem: String,
}

/// [`judge`] on the certificate at `path` (the agent's `TLS_CLIENT_CERT`): the certificate, when the agent may run
/// with it.
pub(crate) fn check_certificate(service: &str, path: &str) -> Result<OwnCertificate, String> {
    let pem = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read this agent's certificate {path}: {e}"))?;
    let identity = extract_own_identity(&pem)
        .map_err(|e| format!("this agent's certificate {path} names no identity: {e}"))?;
    judge(service, &identity)?;
    Ok(OwnCertificate { identity, pem })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A certificate's text naming `names`: the first is its identity (a DNS name), and `g:` and `p:` names are URI
    /// names, as cert-issuer writes them.
    pub(crate) fn certificate_with(names: &[&str]) -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![names[0].to_string()]).unwrap();
        for name in &names[1..] {
            params
                .subject_alt_names
                .push(rcgen::SanType::URI(name.to_string().try_into().unwrap()));
        }
        params.self_signed(&key).unwrap().pem()
    }

    /// A certificate whose first DNS SAN is `identity`, written to a file the test keeps.
    pub(crate) fn certificate_for(identity: &str) -> tempfile::NamedTempFile {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec![identity.to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), cert.pem()).unwrap();
        file
    }

    #[test]
    fn an_agent_runs_as_the_workload_it_is_named_for() {
        assert!(
            judge(
                "kube-observer",
                "kube-observer.polar.serviceaccount.cluster.local"
            )
            .is_ok()
        );
    }

    #[test]
    fn an_agent_with_another_workloads_certificate_is_refused_naming_both() {
        let said = judge(
            "kube-observer",
            "kube-consumer.polar.serviceaccount.cluster.local",
        )
        .unwrap_err();
        assert!(
            said.contains("`kube-observer`") && said.contains("`kube-consumer`"),
            "{said}"
        );
    }

    #[test]
    fn an_agent_with_a_users_certificate_is_refused() {
        let said = judge("kube-observer", "alice.example.org").unwrap_err();
        assert!(said.contains("not a workload's"), "{said}");
    }

    #[test]
    fn the_name_is_read_from_the_certificate_file() {
        let own = certificate_for("jira-observer.polar.serviceaccount.cluster.local");
        assert!(check_certificate("jira-observer", own.path().to_str().unwrap()).is_ok());
        let other = certificate_for("jira-processor.polar.serviceaccount.cluster.local");
        assert!(check_certificate("jira-observer", other.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn a_certificate_that_cannot_be_read_is_refused() {
        let said = check_certificate("jira-observer", "/nonexistent/cert.pem").unwrap_err();
        assert!(said.contains("/nonexistent/cert.pem"), "{said}");
        let garbage = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(garbage.path(), "not a certificate").unwrap();
        assert!(check_certificate("jira-observer", garbage.path().to_str().unwrap()).is_err());
    }

    fn label() -> impl Strategy<Value = String> {
        "[a-z0-9]([a-z0-9-]{0,20}[a-z0-9])?"
    }

    proptest! {
        /// An agent starts with a workload's certificate exactly when the workload is its name,
        /// whatever the namespace and the cluster domain.
        #[test]
        fn an_agent_starts_exactly_when_its_certificate_is_its_own_workloads(
            service in label(),
            workload in label(),
            namespace in label(),
            domain in prop::collection::vec(label(), 1..4).prop_map(|l| l.join(".")),
        ) {
            let identity = format!("{workload}.{namespace}.serviceaccount.{domain}");
            prop_assert_eq!(judge(&service, &identity).is_ok(), service == workload);
            prop_assert!(judge(&workload, &identity).is_ok());
        }
    }
}
