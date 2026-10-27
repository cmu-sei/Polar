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

//! The daemon's socket (docs/architecture/key-custody.md, "Step 1 in detail"). Who a connection is comes from the
//! kernel (`SO_PEERCRED`), never from what it says: `polar-init`'s UID is the installer, the workload's UIDs sign, and
//! anyone else is turned away before a frame is read. Every request is answered on its own task, its reply carrying its
//! id, so a caller may have many in flight on one connection; a signature takes a shared lock, so signatures run side
//! by side, and only making a key or installing a certificate takes the slots for itself.

use crate::custody::{Custody, Roles};
use crate::protocol::{Envelope, FrameError, Reply, Request, decode_request, frame, frame_length};
use std::path::Path;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Who may connect, by UID.
#[derive(Debug, Clone)]
pub struct Callers {
    /// `polar-init`'s.
    pub installer_uid: u32,
    /// The workload containers'.
    pub workload_uids: Vec<u32>,
}

impl Callers {
    pub fn roles(&self, uid: u32) -> Roles {
        Roles {
            installer: uid == self.installer_uid,
            workload: self.workload_uids.contains(&uid),
        }
    }
}

/// The socket at `path`, for the pod's own containers: a stale one removed first, and readable and writable by the
/// daemon's user and group alone.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The reply to one request: a key or a certificate changes the slots, under the write lock; anything else reads.
pub fn respond(custody: &RwLock<Custody>, roles: Roles, request: Request) -> Reply {
    match request {
        Request::MakeCsr { .. } | Request::Install { .. } => match custody.write() {
            Ok(mut custody) => custody.handle(roles, request, now()),
            Err(_) => Reply::Refused(crate::protocol::Refusal::BadRequest(
                "the custody is poisoned".to_string(),
            )),
        },
        _ => match custody.read() {
            Ok(custody) => custody.handle_read(roles, request),
            Err(_) => Reply::Refused(crate::protocol::Refusal::BadRequest(
                "the custody is poisoned".to_string(),
            )),
        },
    }
}

/// Serves `listener` until it fails.
pub async fn serve(
    listener: UnixListener,
    custody: Arc<RwLock<Custody>>,
    callers: Arc<Callers>,
) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let uid = match stream.peer_cred() {
            Ok(credentials) => credentials.uid(),
            Err(e) => {
                warn!("a connection whose caller cannot be known: {e}");
                continue;
            }
        };
        let roles = callers.roles(uid);
        if roles == Roles::default() {
            warn!(
                uid,
                "a connection from a caller that is neither the installer nor the workload: closed"
            );
            continue;
        }
        tokio::spawn(connection(stream, roles, custody.clone()));
    }
}

async fn connection(stream: UnixStream, roles: Roles, custody: Arc<RwLock<Custody>>) {
    let (mut reader, mut writer) = stream.into_split();
    let (replies, mut outgoing) = mpsc::channel::<Vec<u8>>(64);
    let writing = tokio::spawn(async move {
        while let Some(bytes) = outgoing.recv().await {
            if writer.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });
    loop {
        let body = match read_one(&mut reader).await {
            Ok(body) => body,
            Err(FrameError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                debug!("the connection ends: {e}");
                break;
            }
        };
        let request = match decode_request(&body) {
            Ok(request) => request,
            Err(e) => {
                warn!("a request that does not read; the connection is closed: {e}");
                break;
            }
        };
        let custody = custody.clone();
        let replies = replies.clone();
        tokio::spawn(async move {
            let reply = respond(&custody, roles, request.body);
            if let Ok(bytes) = frame(&Envelope {
                id: request.id,
                body: reply,
            }) {
                let _ = replies.send(bytes).await;
            }
        });
    }
    drop(replies);
    let _ = writing.await;
}

async fn read_one(reader: &mut (impl AsyncReadExt + Unpin)) -> Result<Vec<u8>, FrameError> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).await?;
    let mut body = vec![0u8; frame_length(header)?];
    reader.read_exact(&mut body).await?;
    Ok(body)
}
