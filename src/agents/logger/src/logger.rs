//! logging/consume/src/logs.rs
//! Polar-style consumer actor for logs:
//! - Actor name MUST match topic string so dispatcher can route via where_is(topic). :contentReference[oaicite:3]{index=3}
//! - pre_start subscribes via cassini TCP client Subscribe(topic). :contentReference[oaicite:4]{index=4}
//! - handle receives already-routed messages (you can wire dispatcher -> this actor).

use std::{path::PathBuf, sync::Arc};

use cassini_client::TcpClientMessage;
use ractor::{async_trait, registry::where_is, Actor, ActorProcessingErr, ActorRef};
use rocksdb::{Options, WriteBatch, WriteOptions, DB};
use tracing::{debug, error, info, warn};

use polar::DispatcherMessage; // same message used by Polar dispatchers :contentReference[oaicite:5]{index=5}

/// Match the Polar pattern: the actor's registry name == the topic string the dispatcher uses. :contentReference[oaicite:6]{index=6}
pub const LOG_CONSUMER_TOPIC: &str = "logging:consumer:logs";

/// Match Polar’s pattern of locating the TCP client actor by a well-known name. :contentReference[oaicite:7]{index=7}
pub const BROKER_CLIENT_NAME: &str = "LOGGING_CONSUMER_CLIENT";

/// --- Durability profile ---
#[derive(Clone, Copy, Debug)]
pub enum Durability {
    DevLoose,    // fast: do not fsync every commit
    AuditStrict, // safe: fsync (WAL) before returning
}

/// --- Actor args/state (Polar-style) ---
#[derive(Clone, Debug)]
pub struct LoggingConsumerArgs {
    pub registration_id: String, // match the “registration_id” pattern in Polar consumers :contentReference[oaicite:8]{index=8}
    pub db_path: PathBuf,
    pub durability: Durability,
}

pub struct LoggingConsumerState {
    pub registration_id: String,
    pub db: Arc<DB>,
}

/// --- Wire decoding results ---
/// Your producers send: [FixedHeader][BatchEnvelope][N * (MiniHeader + rkyv_payload_bytes)]
/// The actor does NOT deserialize payload into owned Rust structs; it validates then stores the bytes.
#[derive(Debug)]
pub struct DecodedBatch<'a> {
    pub ingest_ns: u64,
    pub events: Vec<DecodedEvent<'a>>,
}

#[derive(Debug)]
pub struct DecodedEvent<'a> {
    pub stream_id: [u8; 16],
    pub event_id: [u8; 16],
    pub schema_id: u32,
    pub schema_ver: u32,
    pub ts_observed_ns: u64,
    pub archived_payload: &'a [u8], // rkyv bytes (already serialized by producer)
}

/// --- Your rkyv log payload type(s) (example) ---
/// In practice you’ll have schema_id/schema_ver -> validator mapping.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Debug)]
#[archive(check_bytes)]
pub struct LogRecordV1 {
    pub level: u8,
    pub msg: String,
    pub agent: String,
    pub tags: Vec<String>,
}

/// Decode headers + carve payload slices.
///
/// IMPORTANT: This is a *skeleton*. You’ll plug in your actual header format.
/// Keep it boring: bounds checks first, then parse, then return slices.
fn decode_batch(frame: &[u8]) -> Result<DecodedBatch<'_>, String> {
    // TODO: parse your fixed header + batch envelope + per-event mini headers.
    // TODO: enforce declared lengths == actual lengths.
    // TODO: return slices into `frame` to avoid copies.
    Err("decode_batch: not implemented".into())
}

/// Validate archived rkyv bytes before touching them.
/// This is the “no footguns” step.
fn validate_archived(schema_id: u32, schema_ver: u32, payload: &[u8]) -> Result<(), String> {
    match (schema_id, schema_ver) {
        (1, 1) => rkyv::check_archived_root::<LogRecordV1>(payload)
            .map(|_| ())
            .map_err(|e| format!("rkyv validation failed: {e}")),
        _ => Err(format!("unknown schema {schema_id} v{schema_ver}")),
    }
}

/// RocksDB keying: stream_id(16) || seq_be_u64(8)
fn event_key(stream_id: &[u8; 16], seq: u64) -> [u8; 24] {
    let mut k = [0u8; 24];
    k[..16].copy_from_slice(stream_id);
    k[16..].copy_from_slice(&seq.to_be_bytes());
    k
}

/// Store stream head: stream_id -> next_seq (be u64)
fn encode_next_seq(next: u64) -> [u8; 8] {
    next.to_be_bytes()
}

/// Load stream head or default 0.
/// Skeleton: you can cache these in state later for throughput.
fn load_next_seq(db: &DB, stream_id: &[u8; 16]) -> u64 {
    db.get(stream_id)
        .ok()
        .flatten()
        .and_then(|v| (v.len() == 8).then(|| u64::from_be_bytes(v.as_slice().try_into().ok()?)))
        .unwrap_or(0)
}

/// Subscribe helper (mirrors Polar consumer pattern: find TCP client, send Subscribe). :contentReference[oaicite:9]{index=9}
async fn subscribe_to_topic(topic: String) -> Result<(), ActorProcessingErr> {
    let client = where_is(BROKER_CLIENT_NAME.to_string())
        .ok_or_else(|| ActorProcessingErr::from("Expected to find TCP client".to_string()))?;
    client
        .send_message(TcpClientMessage::Subscribe(topic))
        .map_err(|e| ActorProcessingErr::from(format!("Subscribe failed: {e}")))?;
    Ok(())
}

/// --- The Logging Consumer Actor ---
/// This actor receives `DispatcherMessage::Dispatch` (raw bytes + topic) like other Polar dispatch paths. :contentReference[oaicite:10]{index=10}
pub struct LoggingConsumer;

#[async_trait]
impl Actor for LoggingConsumer {
    type Msg = DispatcherMessage;
    type State = LoggingConsumerState;
    type Arguments = LoggingConsumerArgs;

    async fn pre_start(
        &self,
        myself: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("{myself:?} starting, subscribing to {LOG_CONSUMER_TOPIC}");

        subscribe_to_topic(LOG_CONSUMER_TOPIC.to_string()).await?;

        let mut opts = Options::default();
        opts.create_if_missing(true);
        let db = DB::open(&opts, &args.db_path)
            .map_err(|e| ActorProcessingErr::from(format!("DB open failed: {e}")))?;

        Ok(LoggingConsumerState {
            registration_id: args.registration_id,
            db: Arc::new(db),
        })
    }

    async fn post_start(
        &self,
        myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("{:?} waiting to consume logs", myself.get_name());
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            DispatcherMessage::Dispatch { message, topic } => {
                // Dispatcher uses topic as routing key; you can sanity-check it here if you want.
                if topic != LOG_CONSUMER_TOPIC {
                    warn!("Unexpected topic '{topic}' delivered to LoggingConsumer");
                    return Ok(());
                }

                // 1) decode headers
                let decoded = match decode_batch(&message) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("drop: decode failed: {e}");
                        return Ok(());
                    }
                };

                // 2) validate + 3) write (payload bytes) to RocksDB
                if let Err(e) = persist_batch(&state.db, decoded) {
                    // If this is audit mode, you probably want to crash-stop here.
                    // In dev mode, you may prefer to log and continue.
                    error!("persist failed: {e}");
                }
            }
            // If Polar adds more DispatcherMessage variants later, ignore safely.
            _ => {}
        }

        Ok(())
    }
}

/// Persist a decoded batch with optional strict durability.
/// For audit mode you’ll set sync=true on WriteOptions (fsync WAL boundary).
fn persist_batch(db: &DB, batch: DecodedBatch<'_>) -> Result<(), String> {
    // NOTE: column families omitted in this skeleton. You’ll likely split:
    // - events CF for event data
    // - streams CF for stream heads
    let mut wb = WriteBatch::default();

    // Per-stream seq bookkeeping for this batch
    use std::collections::HashMap;
    let mut next_seq: HashMap<[u8; 16], u64> = HashMap::new();

    for ev in batch.events {
        // 2) validate rkyv payload bytes
        validate_archived(ev.schema_id, ev.schema_ver, ev.archived_payload)?;

        // Allocate sequence
        let cur = *next_seq
            .entry(ev.stream_id)
            .or_insert_with(|| load_next_seq(db, &ev.stream_id));

        // Write the payload bytes as-is (already serialized by producer)
        wb.put(event_key(&ev.stream_id, cur), ev.archived_payload);

        // Advance seq
        next_seq.insert(ev.stream_id, cur + 1);
    }

    // Update stream heads
    for (stream_id, nxt) in next_seq {
        wb.put(stream_id, encode_next_seq(nxt));
    }

    // Commit: caller decides sync policy via config; keep skeleton minimal here.
    let mut wo = WriteOptions::default();
    // TODO: wire in Durability (DevLoose vs AuditStrict) from actor args/state
    wo.set_sync(true); // assume audit strict in this skeleton

    db.write_opt(wb, &wo)
        .map_err(|e| format!("rocksdb write failed: {e}"))?;

    Ok(())
}
