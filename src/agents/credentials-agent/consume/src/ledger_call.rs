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
