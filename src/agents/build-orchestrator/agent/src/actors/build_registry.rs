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

use std::collections::HashMap;

use orchestrator_core::types::{BuildRecord, BuildRequest, BuildState};
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use uuid::Uuid;

/// Messages the BuildRegistryActor accepts.
pub enum RegistryMessage {
    /// Insert a new BuildRecord in Pending state.
    Insert(BuildRequest),

    /// Attempt a state transition on an existing build.
    /// Only the fields that are Some will overwrite the existing record values —
    /// None means "leave whatever is already there unchanged".
    Transition {
        build_id: Uuid,
        next_state: BuildState,
        /// Set when the build is submitted to the backend (Scheduled state).
        backend_handle: Option<String>,
        /// Set when image resolution completes (ResolvingImage → Scheduled, or
        /// Bootstrapping → Scheduled). Records the digest so Polar can correlate
        /// the build with the exact image that ran it.
        resolved_image: Option<String>,
        failure_reason: Option<String>,
    },

    /// Retrieve a snapshot of a BuildRecord by ID.
    Get(Uuid, RpcReplyPort<Option<BuildRecord>>),

    /// Retrieve all records in a given state.
    ListByState(BuildState, RpcReplyPort<Vec<BuildRecord>>),
}

pub struct BuildRegistryActor;

pub struct RegistryState {
    records: HashMap<Uuid, BuildRecord>,
}

impl RegistryState {
    fn new() -> Self {
        Self {
            records: HashMap::new(),
        }
    }
}

#[ractor::async_trait]
impl Actor for BuildRegistryActor {
    type Msg = RegistryMessage;
    type State = RegistryState;
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        tracing::info!("BuildRegistryActor starting");
        Ok(RegistryState::new())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            RegistryMessage::Insert(request) => {
                let build_id = request.build_id;
                let record = BuildRecord::new(request);
                tracing::info!(build_id = %build_id, "inserting build record");
                state.records.insert(build_id, record);
            }

            RegistryMessage::Transition {
                build_id,
                next_state,
                resolved_image,
                backend_handle,
                failure_reason,
            } => {
                match state.records.get_mut(&build_id) {
                    Some(record) => {
                        if let Err(e) = record.transition(next_state) {
                            // Log but do not crash — a bad transition message should not
                            // bring down the registry. The caller owns responsibility for
                            // sending valid transitions.
                            tracing::error!(
                                build_id = %build_id,
                                error = %e,
                                "illegal state transition ignored"
                            );
                        } else {
                            if let Some(handle) = backend_handle {
                                record.backend_handle = Some(handle);
                            }
                            if let Some(image) = resolved_image {
                                record.resolved_image = Some(image);
                            }
                            if let Some(reason) = failure_reason {
                                record.failure_reason = Some(reason);
                            }
                        }
                    }
                    None => {
                        tracing::warn!(
                            build_id = %build_id,
                            "transition requested for unknown build_id"
                        );
                    }
                }
            }

            RegistryMessage::Get(build_id, reply) => {
                let record = state.records.get(&build_id).cloned();
                if reply.send(record).is_err() {
                    tracing::warn!(build_id = %build_id, "Get reply port closed before send");
                }
            }

            RegistryMessage::ListByState(filter_state, reply) => {
                let records: Vec<BuildRecord> = state
                    .records
                    .values()
                    .filter(|r| r.state == filter_state)
                    .cloned()
                    .collect();
                if reply.send(records).is_err() {
                    tracing::warn!("ListByState reply port closed before send");
                }
            }
        }

        Ok(())
    }
}
