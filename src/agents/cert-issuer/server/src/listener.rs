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

//! The listener, as an actor (docs/architecture/cert-issuer-isolation.md, "1b in detail"). axum is not an actor; this
//! one runs it, linked under cert-issuer's supervisor. On [`ListenerMessage::Drain`] it stops accepting, lets the
//! connections in flight finish within its drain timeout, replies, and ends. Serving that ends any other way (it
//! returns, or it panics) fails the actor, so the supervisor sees it as it sees any child, and a CA front that serves
//! nothing cannot look healthy.

use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, oneshot};
use tracing::{info, warn};

/// What the listener runs: given the signal that stops it and the count of connections in flight, the future that
/// serves until it is stopped.
pub type Serve = Box<
    dyn FnOnce(Arc<Notify>, Arc<AtomicUsize>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send,
>;

pub struct ListenerArgs {
    pub serve: Serve,
    /// How long a drain waits for the connections in flight.
    pub drain_timeout: Duration,
}

impl ListenerArgs {
    /// Serve `app` over TLS on `tcp` ([`crate::tls::serve_tls`]).
    pub fn tls(
        tcp: tokio::net::TcpListener,
        acceptor: tokio_rustls::TlsAcceptor,
        app: axum::Router,
        drain_timeout: Duration,
    ) -> Self {
        Self {
            serve: Box::new(move |stop, active| {
                Box::pin(crate::tls::serve_tls(tcp, acceptor, app, stop, active))
            }),
            drain_timeout,
        }
    }
}

pub enum ListenerMessage {
    /// Stop accepting, let the connections in flight finish within the drain timeout, reply, and end.
    Drain(RpcReplyPort<()>),
    /// Serving ended while no drain was asked for, and why.
    Ended(String),
}

pub struct Listener;

pub struct ListenerState {
    stop: Arc<Notify>,
    active: Arc<AtomicUsize>,
    /// Set before a drain stops the serving, so its ending is not reported as a failure.
    draining: Arc<AtomicBool>,
    /// Fires when the serving has ended, however it ended.
    served: Option<oneshot::Receiver<()>>,
    drain_timeout: Duration,
}

#[ractor::async_trait]
impl Actor for Listener {
    type Msg = ListenerMessage;
    type State = ListenerState;
    type Arguments = ListenerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: ListenerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        let stop = Arc::new(Notify::new());
        let active = Arc::new(AtomicUsize::new(0));
        let draining = Arc::new(AtomicBool::new(false));
        let (served_tx, served) = oneshot::channel();
        let serving = tokio::spawn((args.serve)(stop.clone(), active.clone()));
        // Watches the serving: its ending, unless a drain asked for it, is this actor's failure.
        let watched = draining.clone();
        tokio::spawn(async move {
            let outcome = serving.await;
            let _ = served_tx.send(());
            if watched.load(Ordering::SeqCst) {
                return;
            }
            // A panic's message is in the error's own words.
            let why = match outcome {
                Ok(()) => "the listener stopped serving on its own".to_string(),
                Err(e) => format!("the listener's task ended: {e}"),
            };
            let _ = myself.send_message(ListenerMessage::Ended(why));
        });
        Ok(ListenerState {
            stop,
            active,
            draining,
            served: Some(served),
            drain_timeout: args.drain_timeout,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            ListenerMessage::Drain(reply) => {
                info!("draining the listener: no new connections");
                state.draining.store(true, Ordering::SeqCst);
                state.stop.notify_one();
                if let Some(served) = state.served.take() {
                    let _ = served.await;
                }
                let active = state.active.clone();
                let drained = tokio::time::timeout(state.drain_timeout, async move {
                    while active.load(Ordering::SeqCst) > 0 {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await;
                if drained.is_err() {
                    warn!(
                        "connections were still open after {:?}; the drain goes on without them",
                        state.drain_timeout
                    );
                }
                let _ = reply.send(());
                myself.stop(Some("drained".to_string()));
                Ok(())
            }
            ListenerMessage::Ended(why) => Err(ActorProcessingErr::from(why)),
        }
    }
}
