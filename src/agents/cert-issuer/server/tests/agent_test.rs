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

//! cert-issuer as a Polar agent on the builder, run whole (docs/architecture/cert-issuer-isolation.md, "1b in detail"):
//! it signs its own client certificate on its own token, through its floor, and serves; and with no broker to reach,
//! long past the Cassini deadline, it is still serving and healthy, since it speaks over Cassini without depending on
//! it.

mod agent_rig;
mod helpers;
use agent_rig::*;
use std::time::Duration;

#[test]
fn cert_issuer_signs_its_own_certificate_and_serves_with_no_broker_long_past_the_cassini_deadline()
{
    let rig = Rig::start(&[]);
    let answer = rig.first("/ca");
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(
        answer.contains("BEGIN CERTIFICATE"),
        "it serves its CA: {answer}"
    );

    let own = std::fs::read_to_string(rig.dir.path().join("tls/client.crt")).unwrap();
    assert_eq!(
        names_of(&own),
        vec![IDENTITY.to_string(), "g:cassini-client".to_string()],
        "its own client certificate, with what its floor entry gives it"
    );
    assert_eq!(
        std::fs::read_to_string(rig.dir.path().join("tls/ca.pem")).unwrap(),
        std::fs::read_to_string(rig.ca()).unwrap(),
        "and the CA it signs under, where its Cassini client reads the broker's CA"
    );
    // Both its own certificates are in its record: its TLS server certificate, and its client certificate, signed on
    // its token through its floor.
    let record =
        cert_issuer::record::Record::open(&rig.dir.path().join("record/issued.jsonl")).unwrap();
    let paths: Vec<(String, cert_issuer_common::record::IssuePath)> = record
        .entries()
        .iter()
        .map(|e| (e.identity.clone(), e.path))
        .collect();
    assert_eq!(
        paths,
        vec![
            (
                SERVER_IDENTITY.to_string(),
                cert_issuer_common::record::IssuePath::Own
            ),
            (
                IDENTITY.to_string(),
                cert_issuer_common::record::IssuePath::TokenOnly
            ),
        ]
    );

    // Four times the Cassini deadline, ticking every second, with no broker.
    std::thread::sleep(Duration::from_secs(4));
    assert!(rig.ended.try_recv().is_err(), "it has not ended");
    let answer = rig.get("/ca").unwrap();
    assert!(
        answer.starts_with("HTTP/1.1 200"),
        "still serving: {answer}"
    );
    let health: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(rig.dir.path().join("health.json")).unwrap())
            .unwrap();
    assert_eq!(health["status"], "healthy", "{health}");
    assert_eq!(health["cassini_connected"], false, "{health}");
}
