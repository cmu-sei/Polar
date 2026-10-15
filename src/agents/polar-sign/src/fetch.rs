//! Getting the agent's escrow certificate from the credentials agent's gateway (`POST /agent-escrow-key`), and
//! turning it into the key to encrypt to. The gateway speaks plain HTTP and takes no authentication; what makes
//! the answer believable is the check in [`escrow_key`]: the certificate chains to the CA in the author's
//! `ca.pem`, is valid now, and names the agent (`polar_signing::verify_escrow_certificate`). A key swapped on the
//! way back fails it.

use serde_json::Value;

/// The identity an agent's escrow certificate carries: its workload name in the `polar` namespace, as
/// cert-issuer normalises it.
pub fn escrow_identity(agent: &str) -> String {
    format!("{agent}.polar.serviceaccount.cluster.local")
}

/// The certificate in the gateway's answer, or why there is none.
pub fn certificate_from_reply(status: u16, body: &str, identity: &str) -> Result<String, String> {
    match status {
        200 => {
            let value: Value = serde_json::from_str(body)
                .map_err(|_| "the gateway's answer is not JSON".to_string())?;
            value
                .pointer("/Found/certificate_pem")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
                .map(str::to_string)
                .ok_or_else(|| "the gateway's answer holds no certificate".to_string())
        }
        // The consumer's own "not found" has a body; a gateway without the route answers 404 with none.
        404 if body.contains("NotFound") => Err(format!(
            "the credentials agent has no escrow certificate for `{identity}`: the agent has not started \
             on this cluster yet, or is not called that"
        )),
        404 => Err(
            "the gateway has no /agent-escrow-key route: its image is older than this tool"
                .to_string(),
        ),
        400 => Err(format!(
            "the gateway refused the name `{identity}`: {}",
            body.trim().chars().take(200).collect::<String>()
        )),
        other => Err(format!("the gateway answered {other}")),
    }
}

/// Asks the gateway at `observer_url` for the escrow certificate of `identity`.
pub fn fetch_certificate(observer_url: &str, identity: &str) -> Result<String, String> {
    let url = format!("{}/agent-escrow-key", observer_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(|e| format!("cannot make an HTTP client: {e}"))?;
    let response = client
        .post(&url)
        .json(&serde_json::json!({ "identity": identity }))
        .send()
        .map_err(|e| {
            format!(
                "cannot reach the credentials agent's gateway at {observer_url} (is the port-forward up?): {}",
                // the error names the URL, which holds nothing secret, and the cause
                e.without_url()
            )
        })?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .map_err(|e| format!("cannot read the gateway's answer: {e}"))?;
    certificate_from_reply(status, &body, identity)
}

/// The public key to encrypt to, from the certificate the gateway gave, if it can be believed.
pub fn escrow_key(
    certificate_pem: &str,
    trust: &polar_signing::Trust,
    now: i64,
    identity: &str,
) -> Result<String, String> {
    polar_signing::verify_escrow_certificate(certificate_pem, trust, now, identity)
        .map_err(|e| format!("the escrow certificate for `{identity}` cannot be believed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_signing::testing::{EscrowSpec, escrow_certificate, now, root};

    #[test]
    fn an_agents_escrow_identity_is_its_name_in_the_polar_namespace() {
        assert_eq!(
            escrow_identity("git-repo-observer"),
            "git-repo-observer.polar.serviceaccount.cluster.local"
        );
    }

    #[test]
    fn the_certificate_is_taken_from_a_found_answer() {
        let body = r#"{"Found":{"public_key_pem":"KEY","certificate_pem":"CERT"}}"#;
        assert_eq!(certificate_from_reply(200, body, "x").unwrap(), "CERT");
    }

    #[test]
    fn every_other_answer_says_what_it_means() {
        assert!(
            certificate_from_reply(404, "\"NotFound\"", "a.b")
                .unwrap_err()
                .contains("has not started")
        );
        // a gateway that does not have the route says nothing in its 404
        let old = certificate_from_reply(404, "", "a.b").unwrap_err();
        assert!(old.contains("has no /agent-escrow-key route"), "{old}");
        assert!(
            certificate_from_reply(400, "bad name", "a b")
                .unwrap_err()
                .contains("bad name")
        );
        assert!(
            certificate_from_reply(502, "", "x")
                .unwrap_err()
                .contains("502")
        );
        for body in [
            "not json",
            "{}",
            r#"{"Found":{}}"#,
            r#"{"Found":{"certificate_pem":""}}"#,
            "\"NotFound\"",
        ] {
            assert!(certificate_from_reply(200, body, "x").is_err(), "{body}");
        }
    }

    #[test]
    fn a_gateway_that_cannot_be_reached_says_where_it_looked() {
        // nothing listens on port 1 of the loopback address
        let err = fetch_certificate(
            "http://127.0.0.1:1",
            "git-repo-observer.polar.serviceaccount.cluster.local",
        )
        .unwrap_err();
        assert!(
            err.contains("cannot reach the credentials agent's gateway at http://127.0.0.1:1"),
            "{err}"
        );
        assert!(err.contains("port-forward"));
    }

    #[test]
    fn a_certificate_from_the_trusted_ca_gives_a_key_and_anything_else_is_refused() {
        let ca = root("Polar Internal CA");
        let trust = polar_signing::Trust::from_pem(std::slice::from_ref(&ca.pem)).unwrap();
        let id = escrow_identity("git-repo-observer");
        let cert = escrow_certificate(&ca, EscrowSpec::default());
        assert!(
            escrow_key(&cert, &trust, now(), &id)
                .unwrap()
                .contains("BEGIN PUBLIC KEY")
        );

        // signed by someone else: the swapped key
        let attacker = root("Polar Internal CA");
        let forged = escrow_certificate(&attacker, EscrowSpec::default());
        let err = escrow_key(&forged, &trust, now(), &id).unwrap_err();
        assert!(
            err.contains("cannot be believed") && err.contains("trusts"),
            "{err}"
        );

        // another agent's genuine certificate
        let other = escrow_certificate(
            &ca,
            EscrowSpec {
                identity: "git-repo-controller.polar.serviceaccount.cluster.local",
                ..EscrowSpec::default()
            },
        );
        assert!(
            escrow_key(&other, &trust, now(), &id)
                .unwrap_err()
                .contains(&id)
        );
    }
}
