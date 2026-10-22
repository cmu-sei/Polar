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

//! Submits one signed admin action to the credentials-agent observer over
//! HTTP and returns the consumer's verdict.
//!
//! The action is self-authenticating (the consumer verifies the signature
//! against the author's certificate in the ledger), so the transport needs no
//! identity of its own. The reply is the only confirmation there is: the
//! consumer is the gate, so a delivered request means nothing until it says
//! Recorded or Refused.

use credentials_agent_common::admin_action::{AdminActionReply, SignedAdminAction};
use std::time::Duration;

pub async fn request_admin_action(
    observer_url: &str,
    signed: &SignedAdminAction,
    timeout: Duration,
) -> Result<AdminActionReply, String> {
    let url = format!(
        "{}{}",
        observer_url.trim_end_matches('/'),
        credentials_agent_common::routes::ADMIN_ACTION
    );

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("building HTTP client: {e}"))?;

    let response = client
        .post(&url)
        .json(signed)
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("reading the observer's reply: {e}"))?;

    parse_reply(status.as_u16(), &body)
}

/// A refusal arrives with a non-2xx status but still carries the consumer's
/// reply as JSON; anything that is not that (a 504 timeout, a 503 from the
/// broker, a 404 from the wrong service) is a transport problem, reported
/// with the status and body so it is not mistaken for a verdict.
fn parse_reply(status: u16, body: &str) -> Result<AdminActionReply, String> {
    serde_json::from_str::<AdminActionReply>(body)
        .map_err(|_| format!("observer returned {status}: {}", body.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_reply_is_parsed_whatever_the_status() {
        let body = serde_json::to_string(&AdminActionReply::Recorded).unwrap();
        assert_eq!(parse_reply(200, &body), Ok(AdminActionReply::Recorded));
        assert_eq!(parse_reply(403, &body), Ok(AdminActionReply::Recorded));
    }

    #[test]
    fn a_non_json_body_is_a_transport_error_carrying_the_status() {
        let err = parse_reply(504, "timed out waiting for a reply").unwrap_err();
        assert!(err.contains("504"), "{err}");
        assert!(err.contains("timed out waiting for a reply"), "{err}");
    }
}
