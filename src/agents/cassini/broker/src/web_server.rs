mod routes {
    pub const HEALTHCHECK_ROUTE: &str = "/health";
}

mod handlers {
    use axum::response::IntoResponse;

    pub async fn get_health() -> impl IntoResponse {
        "ok"
    }
}

use axum::{routing::get, Router};
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tokio::net::TcpListener;
use tracing::{debug, error, info};

pub struct AxumServer;

pub struct AxumServerState {
    bind_addr: String,
}

pub struct AxumServerArgs {
    pub bind_addr: String,
}

#[async_trait]
impl Actor for AxumServer {
    type Msg = ();
    type State = AxumServerState;
    type Arguments = AxumServerArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: AxumServerArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("Initializing Axum server on {}", args.bind_addr);

        Ok(AxumServerState {
            bind_addr: args.bind_addr,
        })
    }

    async fn post_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let app = Router::new().route(routes::HEALTHCHECK_ROUTE, get(handlers::get_health));
        match TcpListener::bind(&state.bind_addr).await {
            Ok(listener) => {
                info!("Web server listening on {}", state.bind_addr);
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app.into_make_service()).await {
                        error!("Axum server failed: {}", e);
                    }
                });
                Ok(())
            }
            Err(e) => {
                error!("Failed to bind {}: {}", state.bind_addr, e);
                Err(ActorProcessingErr::from(e))
            }
        }
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        _msg: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}
