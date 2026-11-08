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

//! Whether a recipient's escrow key can be believed (docs/architecture/key-custody.md, "Also in this work"): the
//! credentials agent hands it out with the certificate it is in, so that the key is believed for the certificate and
//! not for the channel it came over. A key is encrypted to only if its certificate chains to the CA this agent pins,
//! names the recipient first, is an escrow certificate (`CertType::Service`: emailProtection, and no TLS purpose), is
//! valid now, and holds that very key.

use x509_parser::prelude::*;

/// The recipient's key, as its certificate holds it, if the certificate can be believed for `recipient`; else why not.
pub fn believed_key(
    certificate_pem: &str,
    public_key_pem: &str,
    recipient: &str,
    ca_pem: &str,
    now: i64,
) -> Result<String, String> {
    let der_of = |pem: &str, what: &str| {
        x509_parser::pem::parse_x509_pem(pem.as_bytes())
            .map(|(_, block)| block.contents)
            .map_err(|e| format!("{what} is not a PEM certificate: {e}"))
    };
    let leaf_der = der_of(certificate_pem, "the recipient's certificate")?;
    let ca_der = der_of(ca_pem, "the pinned CA")?;
    let (_, leaf) = X509Certificate::from_der(&leaf_der)
        .map_err(|e| format!("the recipient's certificate does not parse: {e}"))?;
    let (_, ca) = X509Certificate::from_der(&ca_der)
        .map_err(|e| format!("the pinned CA does not parse: {e}"))?;
    if leaf.issuer() != ca.subject() || leaf.verify_signature(Some(ca.public_key())).is_err() {
        return Err("the recipient's certificate was not signed by the pinned CA".to_string());
    }
    let at = ASN1Time::from_timestamp(now).map_err(|e| e.to_string())?;
    if !leaf.validity().is_valid_at(at) {
        return Err("the recipient's certificate is not valid now".to_string());
    }
    let first_name = leaf
        .subject_alternative_name()
        .ok()
        .flatten()
        .and_then(|san| {
            san.value.general_names.iter().find_map(|name| match name {
                GeneralName::DNSName(dns) => Some(dns.to_string()),
                _ => None,
            })
        });
    if first_name.as_deref() != Some(recipient) {
        return Err(format!("the certificate does not name `{recipient}` first"));
    }
    match leaf.extended_key_usage() {
        Ok(Some(eku))
            if eku.value.email_protection
                && !eku.value.any
                && !eku.value.client_auth
                && !eku.value.server_auth => {}
        _ => return Err("it is not an escrow certificate (emailProtection alone)".to_string()),
    }
    let held = cert_issuer_common::public_key::extract_escrow_public_key_pem(certificate_pem)
        .map_err(|e| format!("its key cannot be read: {e}"))?;
    if held.trim() != public_key_pem.trim() {
        return Err("the key handed out is not the key its certificate holds".to_string());
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPIENT: &str = "kube-consumer.polar.serviceaccount.cluster.local";

    struct Ca {
        issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
        pem: String,
    }

    fn ca() -> Ca {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Polar Internal CA");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().unwrap();
        let pem = params.self_signed(&key).unwrap().pem();
        Ca {
            issuer: rcgen::Issuer::new(params, key),
            pem,
        }
    }

    fn now() -> i64 {
        ::time::OffsetDateTime::now_utc().unix_timestamp()
    }

    /// A certificate `ca` signs for `name`, with `eku`, valid from `from` to `to` seconds from now; and its key's PEM.
    fn certificate(
        ca: &Ca,
        name: &str,
        eku: Vec<rcgen::ExtendedKeyUsagePurpose>,
        from: i64,
        to: i64,
    ) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
        params.extended_key_usages = eku;
        let at = ::time::OffsetDateTime::now_utc();
        params.not_before = at + ::time::Duration::seconds(from);
        params.not_after = at + ::time::Duration::seconds(to);
        let cert = params.signed_by(&key, &ca.issuer).unwrap().pem();
        (cert, key.public_key_pem())
    }

    fn escrow(ca: &Ca) -> (String, String) {
        certificate(
            ca,
            RECIPIENT,
            vec![rcgen::ExtendedKeyUsagePurpose::EmailProtection],
            -60,
            3600,
        )
    }

    #[test]
    fn an_escrow_certificate_the_pinned_ca_signed_for_the_recipient_holding_the_key_is_believed() {
        let ours = ca();
        let (cert, key) = escrow(&ours);
        assert_eq!(
            believed_key(&cert, &key, RECIPIENT, &ours.pem, now()),
            Ok(key)
        );
    }

    #[test]
    fn anything_else_is_not_believed_and_says_why() {
        let (ours, theirs) = (ca(), ca());
        let (cert, key) = escrow(&ours);
        let refused = |result: Result<String, String>, why: &str| {
            let said = result.unwrap_err();
            assert!(said.contains(why), "{said}");
        };
        // Another CA's.
        let (forged, forged_key) = escrow(&theirs);
        refused(
            believed_key(&forged, &forged_key, RECIPIENT, &ours.pem, now()),
            "pinned CA",
        );
        // Another recipient's.
        refused(
            believed_key(&cert, &key, "someone-else", &ours.pem, now()),
            "does not name",
        );
        // A key that is not the certificate's.
        let (_, other_key) = escrow(&ours);
        refused(
            believed_key(&cert, &other_key, RECIPIENT, &ours.pem, now()),
            "not the key",
        );
        // Not an escrow certificate: a client's, or one with no purpose at all.
        let (client, client_key) = certificate(
            &ours,
            RECIPIENT,
            vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth],
            -60,
            3600,
        );
        refused(
            believed_key(&client, &client_key, RECIPIENT, &ours.pem, now()),
            "escrow certificate",
        );
        let (any, any_key) = certificate(&ours, RECIPIENT, vec![], -60, 3600);
        refused(
            believed_key(&any, &any_key, RECIPIENT, &ours.pem, now()),
            "escrow certificate",
        );
        // Expired, or not yet valid.
        let (old, old_key) = certificate(
            &ours,
            RECIPIENT,
            vec![rcgen::ExtendedKeyUsagePurpose::EmailProtection],
            -7200,
            -3600,
        );
        refused(
            believed_key(&old, &old_key, RECIPIENT, &ours.pem, now()),
            "not valid now",
        );
        // Not a certificate at all.
        refused(
            believed_key("CERT", &key, RECIPIENT, &ours.pem, now()),
            "not a PEM certificate",
        );
    }
}
