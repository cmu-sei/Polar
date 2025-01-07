use std::sync::Arc;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::ClientConfig;
use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use crate::ClientMessage;
use tokio::sync::Mutex;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info, warn};


/// Messages handled by the TCP client actor
pub enum TcpClientMessage {
    Send(ClientMessage),
    RegistrationResponse(String),
    ErrorMessage(String),
    GetRegistrationId(RpcReplyPort<String>)
}

/// Actor state for the TCP client
pub struct TcpClientState {
    bind_addr: String,
    writer: Option<Arc<Mutex<BufWriter<WriteHalf<TlsStream<TcpStream>>>>>>,
    reader: Option<ReadHalf<TlsStream<TcpStream>>>, // Use Option to allow taking ownership
    registration_id: Option<String>,
    client_config: Arc<ClientConfig>,
    
}

pub struct TcpClientArgs {
    pub bind_addr: String,
    //TOOD: Just pass in the finished config
    pub ca_cert_file: String,
    pub client_cert_file: String,
    pub private_key_file: String,
    pub registration_id: Option<String>
}

/// TCP client actor
pub struct TcpClientActor;

#[async_trait]
impl Actor for TcpClientActor {
    type Msg = TcpClientMessage;
    type State = TcpClientState;
    type Arguments = TcpClientArgs;

    async fn pre_start(&self,
        _: ActorRef<Self::Msg>,
        args: TcpClientArgs) -> Result<Self::State, ActorProcessingErr> {
        info!("TCP Client Actor starting...");
                        
        let mut root_cert_store = rustls::RootCertStore::empty();
        let _ = root_cert_store.add(CertificateDer::from_pem_file(args.ca_cert_file).expect("Expected to read server cert as pem"));
    
        let client_cert = CertificateDer::from_pem_file(args.client_cert_file).expect("Expected to read server cert as pem");
        let private_key = PrivateKeyDer::from_pem_file(args.private_key_file).unwrap();
        let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store)).build().expect("Expected to build server verifier");

        let config = rustls::ClientConfig::builder()
            .with_webpki_verifier(verifier).with_client_auth_cert(vec![client_cert], private_key).unwrap();

        let state = TcpClientState { bind_addr: args.bind_addr, reader: None, writer: None, registration_id: args.registration_id , client_config: Arc::new(config)};

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        
        let addr = state.bind_addr.clone();
        info!("{myself:?} started. Connecting to {addr}...");
        let connector = TlsConnector::from(Arc::clone(&state.client_config));
        
        match TcpStream::connect(&addr).await {
            Ok(tcp_stream) => {
                //TODO: establish better "Common Name" for the broker server
                let domain = ServerName::try_from("polar").expect("invalid DNS name");
                
                match connector.connect(domain, tcp_stream).await {                  
                    Ok(tls_stream) => {
                        info!("mTLS connection established. ");
                        let (reader, write_half) = split(tls_stream);
                        
                        let writer = tokio::io::BufWriter::new(write_half);
                        
                        state.reader = Some(reader);
                        state.writer = Some(Arc::new(Mutex::new(writer)));
                        
                        info!("{myself:?} Listening... ");
                        let reader = tokio::io::BufReader::new(state.reader.take().expect("Reader already taken!"));
    
                        //start listening
                        let _ = tokio::spawn(async move {
                            let mut buf = String::new();
    
                            let mut buf_reader = tokio::io::BufReader::new(reader);
    
                                while let Ok(bytes) = buf_reader.read_line(&mut buf).await {                
                                    if bytes == 0 { () } else {
                                        if let Ok(msg) = serde_json::from_slice::<ClientMessage>(buf.as_bytes()) {
                                            debug!("recieved: {msg:?}");
                                            match msg {
                                                ClientMessage::RegistrationResponse { registration_id, success, error } => {
                                                    if success {
                                                        info!("Successfully began session with id: {registration_id}");
                                                        myself.send_message(TcpClientMessage::RegistrationResponse(registration_id)).expect("Could not forward message to {myself:?");
                                                        
                                                    } else {
                                                        warn!("Failed to register session with the server. {error:?}");
                                                    }
                                                },
                                                ClientMessage::PublishResponse { topic, payload, result } => {
                                                    //new message on topic
                                                    if result.is_ok() {
                                                        info!("New message on topic {topic}: {payload}");
                                                        //TODO: Forward message to some consumer
                                                    } else {
                                                        warn!("Failed to publish message to topic: {topic}");
                                                    }
                                                },
                                                ClientMessage::PublishRequestAck(topic) => {
                                                    info!("published msg to topic {topic}");
                                                }
    
                                                ClientMessage::SubscribeAcknowledgment { topic, result } => {
                                                    if result.is_ok() {
                                                        debug!("Successfully subscribed to topic: {topic}");
                                                    } else {
                                                        warn!("Failed to subscribe to topic: {topic}");
                                                    }
                                                },
    
                                                ClientMessage::UnsubscribeAcknowledgment { topic, result } => {
                                                    if result.is_ok() {
                                                        debug!("Successfully unsubscribed from topic: {topic}");
                                                    } else {
                                                        warn!("Failed to unsubscribe from topic: {topic}");
                                                    }  
                                                },
                                                _ => {
                                                    warn!("Unexpected message {buf}");
                                                }
                                            }
                                        } else {
                                            //bad data
                                            warn!("Failed to parse message: {buf}");
                                        }
                                    }
                                    buf.clear(); //empty the buffer
                                }        
                        });
                    }
                    Err(e) => {
                        error!("Failed to establish mTLS connection! {e}");
                        myself.stop(Some("Failed to establish mTLS connection! {e}".to_string()));
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect to server: {e}");
                myself.stop(Some("Failed to connect to server: {e}".to_string()));
                
            }
        };

            

        Ok(())
    }

    async fn post_stop(&self, myself: ActorRef<Self::Msg>, _: &mut Self::State) -> Result<(), ActorProcessingErr> {
        debug!("Successfully stopped {myself:?}");
        Ok(())
    }
    
    async fn handle(&self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,) -> Result<(), ActorProcessingErr> {
        match message {
            TcpClientMessage::Send(broker_msg) => {
                let str = serde_json::to_string(&broker_msg).unwrap();
                let msg = format!("{str}\n");
                debug!("Sending message: {msg}");
                let unwrapped_writer = state.writer.clone().unwrap();
                let mut writer = unwrapped_writer.lock().await;        
                if let Err(e) = writer.write(msg.as_bytes()).await {
                    error!("Failed to flush stream {e}, stopping client");
                    myself.stop(Some("UNEXPECTED_DISCONNECT".to_string()))
                }
                
                if let Err(e) = writer.flush().await {
                    error!("Failed to flush stream {e}, stopping client");
                    myself.stop(Some("UNEXPECTED_DISCONNECT".to_string()))
                }
            }
            TcpClientMessage::RegistrationResponse(registration_id) => state.registration_id = Some(registration_id),
            TcpClientMessage::GetRegistrationId(reply) => {
                if let Some(registration_id) = &state.registration_id { reply.send(registration_id.to_owned()).expect("Expected to send registration_id to reply port"); } 
                else { reply.send(String::default()).expect("Expected to send default string"); }
            }
            _ => todo!()
                                
        }
        Ok(())
    }

}