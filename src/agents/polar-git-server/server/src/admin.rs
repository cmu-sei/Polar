//! Creating repositories while the server runs: `PUT /admin/repos/<name>` with an optional JSON
//! body `{"readers": [..], "writers": [..]}`. Only an administrator, identified by the client
//! certificate like every caller, may do it. It is idempotent: the repository is created if it is
//! missing and its access list is set to the body's either way.

use crate::access::{DeclareError, Identity, RepoAcl};
use crate::http::{PeerIdentity, ServerState, refuse};
use crate::store::{Created, StoreError, valid_name};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::Deserialize;
use tracing::{info, warn};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRepo {
    #[serde(default)]
    readers: Vec<String>,
    #[serde(default)]
    writers: Vec<String>,
}

pub async fn create_repo(
    State(state): State<ServerState>,
    axum::Extension(who): axum::Extension<PeerIdentity>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    if !state.authorizer.is_admin(&Identity(who.0.clone())) {
        info!(who = %who.0, repo = %name, "repository creation denied");
        return refuse(StatusCode::FORBIDDEN, "not permitted");
    }
    if !valid_name(&name) {
        return refuse(StatusCode::BAD_REQUEST, "not a valid repository name");
    }
    let request: CreateRepo = if body.is_empty() {
        CreateRepo::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return refuse(StatusCode::BAD_REQUEST, &format!("bad request body: {e}")),
        }
    };
    if let Err(e) = state.runtime.declare(
        &name,
        RepoAcl {
            readers: request.readers.into_iter().collect(),
            writers: request.writers.into_iter().collect(),
        },
    ) {
        return match e {
            DeclareError::Declared => refuse(StatusCode::CONFLICT, &e.to_string()),
            DeclareError::Persist(_) => {
                warn!(error = %e, "repository creation failed");
                refuse(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cannot save the access lists",
                )
            }
        };
    }
    match state.store.create(&name).await {
        Ok(created) => {
            info!(who = %who.0, repo = %name, ?created, "repository created");
            let status = match created {
                Created::New => StatusCode::CREATED,
                Created::AlreadyExisted => StatusCode::OK,
            };
            (status, format!("{name}\n")).into_response()
        }
        Err(StoreError::BadName(_)) => {
            refuse(StatusCode::BAD_REQUEST, "not a valid repository name")
        }
        Err(e) => {
            warn!(error = %e, "repository creation failed");
            refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot create the repository",
            )
        }
    }
}
