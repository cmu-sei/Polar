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

//! What a custodian asks the credentials agent for on its workload's behalf (docs/architecture/key-custody.md, "Part
//! A's bootstrap: the custodian"). The custodian is the custody daemon's own identity, `custody.<X>`
//! (`cert_issuer_common::custody`): its pod's token bought its certificate, and the certificate buys the rest, over the
//! custodian's own Cassini session, for five minutes from its issue.

use crate::issuance::WorkloadCertType;
use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// How long after its certificate's issue a custodian may ask for anything: the bound the renewal rule already uses
/// (docs/architecture/deployments.md, item 2). After that its authority is spent, and its certificate can only listen.
pub const CUSTODIAN_WINDOW_SECS: i64 = 300;

/// A custodian asks for one of its workload's certificates: the workload's key is the daemon's, so the CSR is the
/// daemon's, naming the workload. Published on `credentials-custodian-issuance-requests`; answered with an
/// `IssuanceReply`.
#[derive(
    Debug, Clone, PartialEq, Deserialize, Serialize, Archive, SerdeDeserialize, SerdeSerialize,
)]
pub struct CustodianIssuanceRequest {
    pub csr_pem: String,
    /// A client or a server certificate; never a custodian's.
    pub cert_type: WorkloadCertType,
    /// A server certificate's names besides the identity, as on the token path.
    pub names: Vec<String>,
}
