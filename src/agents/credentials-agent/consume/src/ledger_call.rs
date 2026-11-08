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

//! One place for the ledger request/reply call every handler makes.

use crate::ledger::LedgerMessage;
use ractor::rpc::CallResult;
use ractor::{ActorRef, RpcReplyPort};

/// Sends the message `build` makes to the ledger and waits for its reply.
/// The error is a plain description: callers decide what a ledger that
/// can't answer means for the request they are handling.
pub(crate) async fn ask<T: Send + 'static>(
    ledger: &ActorRef<LedgerMessage>,
    build: impl FnOnce(RpcReplyPort<T>) -> LedgerMessage,
) -> Result<T, String> {
    match ractor::rpc::call(ledger, build, None).await {
        Ok(CallResult::Success(value)) => Ok(value),
        Ok(_) => Err("ledger did not reply".to_string()),
        Err(e) => Err(format!("ledger call failed: {e}")),
    }
}
