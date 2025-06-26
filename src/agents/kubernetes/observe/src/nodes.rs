use cassini::client::{TcpClientActor, TcpClientArgs, TcpClientMessage};
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};

pub struct NodeObserver;
pub struct NodeObserverState;
pub struct NodeObserverArgs;

impl Actot for NodeObserver {
    type Msg = KubeMessage;
    type State = NodeObserverState;
    type Arguments = NodeObserverArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: PodObserverArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }
}
