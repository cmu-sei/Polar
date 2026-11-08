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

//! Git smart HTTP: `info/refs`, `git-upload-pack` and `git-receive-pack`. The caller's identity
//! arrives as an extension set by the TLS listener; this module trusts nothing else about who is
//! asking. Every request is authorized before a git process is started.

use crate::access::{Access, Authorizer, Identity, RuntimeAcl, authorize_push};
use crate::git::{GitError, GitRunner, Service};
use crate::notify::{Notifier, RepoChanged};
use crate::pktline::parse_ref_updates;
use crate::store::RepoStore;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use bytes::Bytes;
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use tracing::{info, warn};

/// The identity the TLS listener took from the client certificate.
#[derive(Debug, Clone)]
pub struct PeerIdentity(pub String);

#[derive(Clone)]
pub struct ServerState {
    pub store: RepoStore,
    pub authorizer: Arc<dyn Authorizer>,
    /// The access lists of repositories created at runtime; normally the same object as
    /// `authorizer`.
    pub runtime: Arc<RuntimeAcl>,
    pub runner: GitRunner,
    pub notifier: Arc<dyn Notifier>,
    /// The largest request body, compressed or not.
    pub max_body: usize,
}

pub fn router(state: ServerState) -> Router {
    let limit = state.max_body;
    Router::new()
        .route("/admin/repos/:name", put(crate::admin::create_repo))
        .route("/:repo/info/refs", get(info_refs))
        .route("/:repo/git-upload-pack", post(upload_pack))
        .route(
            polar_git_server_common::RECEIVE_PACK_ROUTE,
            post(receive_pack),
        )
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state)
}

pub(crate) fn refuse(status: StatusCode, why: &str) -> Response {
    (status, format!("{why}\n")).into_response()
}

fn git_response(content_type: String, body: Vec<u8>) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CACHE_CONTROL,
            "no-cache, max-age=0, must-revalidate",
        )
        .body(Body::from(body))
        .expect("a response with static headers")
}

fn git_failure(e: GitError) -> Response {
    warn!(error = %e, "git failed");
    refuse(StatusCode::INTERNAL_SERVER_ERROR, "git failed")
}

/// `repo.git` and `repo` name the same repository.
fn repo_name(segment: &str) -> &str {
    segment.strip_suffix(".git").unwrap_or(segment)
}

fn protocol(headers: &HeaderMap) -> Option<&str> {
    headers.get("git-protocol").and_then(|v| v.to_str().ok())
}

/// Authorizes `who` for `access` on `repo` and finds the repository. Denial is reported before
/// existence, so a caller learns nothing about repositories it may not touch.
fn open(
    state: &ServerState,
    who: &PeerIdentity,
    repo: &str,
    access: Access,
) -> Result<std::path::PathBuf, Box<Response>> {
    let identity = Identity(who.0.clone());
    if !state.authorizer.allows(&identity, repo, access) {
        info!(who = %who.0, repo, ?access, "denied");
        return Err(Box::new(refuse(StatusCode::FORBIDDEN, "not permitted")));
    }
    if !state.store.exists(repo) {
        return Err(Box::new(refuse(
            StatusCode::NOT_FOUND,
            "no such repository",
        )));
    }
    state
        .store
        .path(repo)
        .map_err(|_| Box::new(refuse(StatusCode::NOT_FOUND, "no such repository")))
}

fn access_for(service: Service) -> Access {
    match service {
        Service::UploadPack => Access::Read,
        Service::ReceivePack => Access::Write,
    }
}

async fn info_refs(
    State(state): State<ServerState>,
    axum::Extension(who): axum::Extension<PeerIdentity>,
    Path(repo): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let repo = repo_name(&repo);
    // Only the smart protocol is served; there is no dumb-HTTP fallback.
    let Some(service) = query.get("service").and_then(|s| Service::from_query(s)) else {
        return refuse(StatusCode::FORBIDDEN, "the smart protocol is required");
    };
    let path = match open(&state, &who, repo, access_for(service)) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    match state
        .runner
        .advertise(&path, service, protocol(&headers))
        .await
    {
        Ok(body) => git_response(service.advertisement_content_type(), body),
        Err(e) => git_failure(e),
    }
}

/// Inflates a gzip-encoded request body, refusing one that grows past `limit`.
fn inflate(body: &[u8], limit: usize) -> Result<Vec<u8>, ()> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(body)
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| ())?;
    if out.len() > limit { Err(()) } else { Ok(out) }
}

fn request_body(headers: &HeaderMap, body: Bytes, limit: usize) -> Result<Vec<u8>, Box<Response>> {
    let gzip = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    if gzip {
        inflate(&body, limit)
            .map_err(|_| Box::new(refuse(StatusCode::BAD_REQUEST, "bad or oversized body")))
    } else {
        Ok(body.to_vec())
    }
}

async fn upload_pack(
    State(state): State<ServerState>,
    axum::Extension(who): axum::Extension<PeerIdentity>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let repo = repo_name(&repo);
    let path = match open(&state, &who, repo, Access::Read) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let body = match request_body(&headers, body, state.max_body) {
        Ok(b) => b,
        Err(r) => return *r,
    };
    match state
        .runner
        .rpc(&path, Service::UploadPack, protocol(&headers), &body)
        .await
    {
        Ok(out) => git_response(Service::UploadPack.content_type(), out),
        Err(e) => git_failure(e),
    }
}

async fn receive_pack(
    State(state): State<ServerState>,
    axum::Extension(who): axum::Extension<PeerIdentity>,
    Path(repo): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let repo = repo_name(&repo);
    let path = match open(&state, &who, repo, Access::Write) {
        Ok(p) => p,
        Err(r) => return *r,
    };
    let body = match request_body(&headers, body, state.max_body) {
        Ok(b) => b,
        Err(r) => return *r,
    };

    // The commands precede the pack: decide before git sees anything.
    let (updates, _) = match parse_ref_updates(&body) {
        Ok(parsed) => parsed,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    if let Err(why) = authorize_push(
        state.authorizer.as_ref(),
        &Identity(who.0.clone()),
        repo,
        &updates,
    ) {
        info!(who = %who.0, repo, %why, "push denied");
        return refuse(StatusCode::FORBIDDEN, &why);
    }

    let out = match state
        .runner
        .rpc(&path, Service::ReceivePack, protocol(&headers), &body)
        .await
    {
        Ok(out) => out,
        Err(e) => return git_failure(e),
    };

    // Git can refuse individual updates (for example a non-fast-forward) and still succeed as a
    // process, so announce only the refs that now point where the push said they would.
    for update in updates {
        if state
            .store
            .resolve_ref(repo, &update.refname)
            .await
            .as_deref()
            == Some(update.new.as_str())
        {
            state
                .notifier
                .repo_changed(RepoChanged {
                    repo: repo.to_string(),
                    refname: update.refname,
                    old: update.old,
                    new: update.new,
                    pusher: who.0.clone(),
                })
                .await;
        }
    }
    git_response(Service::ReceivePack.content_type(), out)
}
