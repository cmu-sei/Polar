use crate::{ArchivedClientMessage, ClientMessage};
use std::sync::Arc;
use polar::DispatcherMessage;
use ractor::registry::where_is;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use rkyv::deserialize;
use rkyv::rancor::Error;
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::ClientConfig;
use tokio::io::{split, AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info, warn};

/// Messages handled by the TCP client actor
pub enum TcpClientMessage {
    Send(ClientMessage),
    RegistrationResponse(String),
    ErrorMessage(String),
    GetRegistrationId(RpcReplyPort<Option<String>>)
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
        // install default crypto provider
        let provider = rustls::crypto::aws_lc_rs::default_provider().install_default();
        if let Err(_) = provider { debug!("Crypto provider configured"); }
                        
        let mut root_cert_store = rustls::RootCertStore::empty();
        let _ = root_cert_store.add(CertificateDer::from_pem_file(args.ca_cert_file).expect("Expected to read CA cert as pem"));
    
        let client_cert = CertificateDer::from_pem_file(args.client_cert_file).expect("Expected to read client cert as pem");
        let private_key = PrivateKeyDer::from_pem_file(args.private_key_file).expect("Expected to read client cert as pem");
        let verifier = WebPkiServerVerifier::builder(Arc::new(root_cert_store)).build().expect("Expected to build client verifier");
        let mut certs = Vec::new();
        certs.push(client_cert);
        let config = rustls::ClientConfig::builder()
            .with_webpki_verifier(verifier).with_client_auth_cert(certs, private_key).unwrap();

        let state = TcpClientState { bind_addr: args.bind_addr, reader: None, writer: None, registration_id: args.registration_id , client_config: Arc::new(config)};

        Ok(state)
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        state: &mut Self::State ) ->  Result<(), ActorProcessingErr> {
        
        let addr = state.bind_addr.clone();
        info!("Connecting to {addr}...");
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
                        
                        let cloned_self = myself.clone();
                        
                        //start listening
                        let _ = tokio::spawn(async move {
    
                            let mut buf_reader = tokio::io::BufReader::new(reader);
                            
                            while let Ok(incoming_msg_length) = buf_reader.read_u32().await {
                                
                                if incoming_msg_length > 0 {
                                    let mut buffer = vec![0; incoming_msg_length as usize];
                                    if let Ok(_) = buf_reader.read_exact(&mut buffer).await {
                                        
                                        let archived = rkyv::access::<ArchivedClientMessage, Error>(&buffer[..]).unwrap();
                                        // And you can always deserialize back to the original type
                                        
                                        if let Ok(message) = deserialize::<ClientMessage, Error>(archived) {
                                            match message {
                                                ClientMessage::RegistrationResponse { registration_id, success, error } => {
                                                    if success {
                                                        info!("Successfully began session with id: {registration_id}");
                                                        cloned_self.send_message(TcpClientMessage::RegistrationResponse(registration_id)).expect("Could not forward message to {myself:?");
                                                        
                                                    } else {
                                                        warn!("Failed to register session with the server. {error:?}");
                                                    }
                                                },
                                                ClientMessage::PublishResponse { topic, payload, result } => {
                                                    //new message on topic
                                                    if result.is_ok() {
                                                        
                                                        //try to find dispatcher
                                                        if let Some(dispatcher) = where_is("DISPATCH".to_string()) {
                                                            if let Err(e) = dispatcher.send_message(DispatcherMessage::Dispatch { message: payload, topic }) {
                                                                warn!("Failed to forward new message to agent");
                                                            }
                                                        }
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
                                                    warn!("Unexpected message {message:?}");
                                                }
                                            }
                                        }   
                                    }
                                }
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

        //Send registration request
        if let Err(e) = myself.send_message(TcpClientMessage::Send(ClientMessage::RegistrationRequest { registration_id: state.registration_id.clone() })) {
            error!("{e}");
            myself.stop(Some(format!("{e}")));
        }

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
                match rkyv::to_bytes::<Error>(&broker_msg){
                    Ok(mut bytes) => {   
                        
                        //create new buffer
                        let mut buffer = Vec::new();

                        //get message length as header
                        let len = bytes.len().to_be_bytes();
                        buffer.extend_from_slice(&len);

                        //add message to buffer
                        buffer.extend_from_slice(&bytes);

                        //write message                        

                        let unwrapped_writer = state.writer.clone().unwrap();
                        let mut writer = unwrapped_writer.lock().await;        
                        if let Err(e) = writer.write(&buffer).await {
                            error!("Failed to flush stream {e}, stopping client");
                            myself.stop(Some("UNEXPECTED_DISCONNECT".to_string()))
                        }
                        
                        if let Err(e) = writer.flush().await {
                            error!("Failed to flush stream {e}, stopping client");
                            myself.stop(Some("UNEXPECTED_DISCONNECT".to_string()))
                        }
                    }
                    Err(e) => {
                        warn!("Failed to serialize message. {e}")
                    }
                }

            }
            TcpClientMessage::RegistrationResponse(registration_id) => {
                state.registration_id = Some(registration_id)
                
            },
            TcpClientMessage::GetRegistrationId(reply) => {
                if let Some(registration_id) = &state.registration_id { reply.send(Some(registration_id.to_owned())).expect("Expected to send registration_id to reply port"); } 
                else { reply.send(None).expect("Expected to send default string"); }
            }
            _ => todo!()
                                
        }
        Ok(())
    }

}