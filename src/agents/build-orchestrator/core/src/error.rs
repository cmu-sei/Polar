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

use thiserror::Error;

use crate::types::BuildState;

#[derive(Debug, Error)]
pub enum CyclopsError {
    #[error("invalid state transition from {from} to {to}")]
    InvalidStateTransition { from: BuildState, to: BuildState },

    #[error("build {0} not found")]
    BuildNotFound(uuid::Uuid),

    #[error("backend error: {0}")]
    Backend(#[from] BackendError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("actor error: {0}")]
    Actor(String),

    #[error("configuration error: {0}")]
    Config(String),
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("job submission failed: {0}")]
    SubmissionFailed(String),

    #[error("job poll failed: {0}")]
    PollFailed(String),

    #[error("job cancellation failed: {0}")]
    CancellationFailed(String),

    #[error("log stream unavailable: {0}")]
    LogStreamUnavailable(String),

    #[error("backend not reachable: {0}")]
    Unreachable(String),
}
