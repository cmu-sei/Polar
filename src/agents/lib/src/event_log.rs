//! A local, append-only, hash-chained event log -- the generic
//! mechanism only. Timestamping, hash-chaining, fsync'd appends, and
//! replay-with-verification live here; event semantics and derived
//! state are entirely the caller's responsibility.
//!
//! Built for the Credentials Agent's own durable audit log, but kept
//! generic since a future agent (the Scheduling Agent, for one) needs
//! the same durability pattern for its own, entirely different event
//! types.
//!
//! `E` must serialize deterministically for replay verification to be
//! meaningful -- avoid `HashMap` fields in event types used with this
//! log (`BTreeMap` is fine; its iteration order is a genuine, sorted
//! function of content, not insertion history or internal hashing).
//! Hashing at write time doesn't depend on this, since the exact bytes
//! written are the exact bytes hashed -- but see `open`'s doc comment
//! for why naive replay verification would otherwise be unsafe.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::marker::PhantomData;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to serialize event: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to parse log entry at line {line}: {source}")]
    Parse {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "failed to deserialize event at line {line} into its expected type: {source}"
    )]
    EventDeserialize {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "hash chain broken at line {line}: expected prev_hash '{expected}', found '{found}' -- \
         the log has been tampered with or corrupted"
    )]
    ChainBroken {
        line: usize,
        expected: String,
        found: String,
    },
    #[error(
        "entry at line {line} failed integrity check: recorded hash '{recorded}' does not \
         match computed hash '{computed}' -- the log has been tampered with or corrupted"
    )]
    IntegrityCheckFailed {
        line: usize,
        recorded: String,
        computed: String,
    },
}

/// The `prev_hash` used for the very first entry in a log -- there is
/// no prior entry to chain from.
const GENESIS_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// On-disk shape used for writing -- `event: &E`, so append() can hash
/// and serialize without an extra clone.
#[derive(Debug, Serialize)]
struct LogEntryWrite<'a, E> {
    timestamp: DateTime<Utc>,
    prev_hash: String,
    event: &'a E,
    hash: String,
}

/// On-disk shape used for reading -- `event` captured as raw,
/// unparsed JSON text (`RawValue`), not yet deserialized into `E`.
/// This is the fix for the HashMap-reordering hazard: hashing is done
/// against these exact bytes, byte-for-byte identical to what's on
/// disk, before `E` is ever reconstructed from them.
#[derive(Debug, serde::Deserialize)]
struct LogEntryRead<'a> {
    timestamp: DateTime<Utc>,
    prev_hash: String,
    #[serde(borrow)]
    event: &'a RawValue,
    hash: String,
}

fn compute_hash(prev_hash: &str, timestamp: &DateTime<Utc>, event_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(timestamp.to_rfc3339().as_bytes());
    hasher.update(event_json.as_bytes());
    hex::encode(hasher.finalize())
}

/// A local, append-only, hash-chained event log.
pub struct EventLog<E> {
    file: File,
    last_hash: String,
    _marker: PhantomData<E>,
}

impl<E> EventLog<E>
where
    E: Serialize + DeserializeOwned,
{
    /// Opens the log at `path` (creating it if absent), reads it once,
    /// verifies the entire hash chain, and returns both the replayed
    /// events (for the caller to fold into its own current state) and
    /// a ready-to-append handle.
    ///
    /// A broken hash chain -- genuine corruption or tampering -- is a
    /// hard, fatal error. This mechanism's entire point is
    /// tamper-evidence; silently continuing past a broken chain would
    /// defeat it.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Vec<E>), EventLogError> {
        let path = path.as_ref();
        let mut events = Vec::new();
        let mut last_hash = GENESIS_HASH.to_string();

        if path.exists() {
            let file = File::open(path)?;
            let reader = BufReader::new(file);
            for (i, line) in reader.lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }

                let entry: LogEntryRead = serde_json::from_str(&line)
                    .map_err(|source| EventLogError::Parse { line: i, source })?;

                if entry.prev_hash != last_hash {
                    return Err(EventLogError::ChainBroken {
                        line: i,
                        expected: last_hash,
                        found: entry.prev_hash,
                    });
                }

                // Hash the exact raw bytes as they appear on disk --
                // never a re-serialization of an already-deserialized
                // value, which could legitimately differ in byte
                // output for types containing a HashMap.
                let computed = compute_hash(&entry.prev_hash, &entry.timestamp, entry.event.get());
                if computed != entry.hash {
                    return Err(EventLogError::IntegrityCheckFailed {
                        line: i,
                        recorded: entry.hash,
                        computed,
                    });
                }

                let event: E = serde_json::from_str(entry.event.get())
                    .map_err(|source| EventLogError::EventDeserialize { line: i, source })?;

                last_hash = entry.hash.clone();
                events.push(event);
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;

        Ok((
            Self {
                file,
                last_hash,
                _marker: PhantomData,
            },
            events,
        ))
    }

    /// Appends a new event, `fsync`'d before returning -- the write is
    /// durable on disk before the caller is told it succeeded.
    pub fn append(&mut self, event: &E) -> Result<(), EventLogError> {
        let timestamp = Utc::now();
        let event_json = serde_json::to_string(event).map_err(EventLogError::Serialize)?;
        let hash = compute_hash(&self.last_hash, &timestamp, &event_json);

        let entry = LogEntryWrite {
            timestamp,
            prev_hash: self.last_hash.clone(),
            event,
            hash: hash.clone(),
        };

        let line = serde_json::to_string(&entry).map_err(EventLogError::Serialize)?;
        writeln!(self.file, "{line}")?;
        self.file.sync_all()?;

        self.last_hash = hash;
        Ok(())
    }

    /// The hash of the most recently appended entry -- `GENESIS_HASH`
    /// if nothing has been appended yet. Callers that need a stable
    /// position marker for the log (e.g. a graph projection's own
    /// checkpoint, tracking exactly how far it's caught up) use this
    /// rather than inventing a separate counter or timestamp, since
    /// it's already a content-derived, tamper-evident position by
    /// construction.
    pub fn current_hash(&self) -> &str {
        &self.last_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    enum TestEvent {
        Grant { identity: String, permissions: Vec<String> },
        Revoke { serial: String, reason: String },
    }

    #[test]
    fn empty_log_opens_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        let (_, events) = EventLog::<TestEvent>::open(&path).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn append_and_replay_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        let e1 = TestEvent::Grant {
            identity: "alice".into(),
            permissions: vec!["read".into(), "write".into()],
        };
        let e2 = TestEvent::Revoke {
            serial: "abc123".into(),
            reason: "compromised".into(),
        };

        {
            let (mut log, events) = EventLog::<TestEvent>::open(&path).unwrap();
            assert!(events.is_empty());
            log.append(&e1).unwrap();
            log.append(&e2).unwrap();
        }

        let (_, replayed) = EventLog::<TestEvent>::open(&path).unwrap();
        assert_eq!(replayed, vec![e1, e2]);
    }

    #[test]
    fn replay_then_append_continues_the_chain_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        let e1 = TestEvent::Grant {
            identity: "alice".into(),
            permissions: vec!["read".into()],
        };
        let e2 = TestEvent::Grant {
            identity: "bob".into(),
            permissions: vec!["write".into()],
        };
        let e3 = TestEvent::Revoke {
            serial: "xyz".into(),
            reason: "expired".into(),
        };

        {
            let (mut log, _) = EventLog::<TestEvent>::open(&path).unwrap();
            log.append(&e1).unwrap();
        }
        {
            // Simulates a restart: open again, replay, then append more.
            let (mut log, events) = EventLog::<TestEvent>::open(&path).unwrap();
            assert_eq!(events, vec![e1.clone()]);
            log.append(&e2).unwrap();
            log.append(&e3).unwrap();
        }

        let (_, all_events) = EventLog::<TestEvent>::open(&path).unwrap();
        assert_eq!(all_events, vec![e1, e2, e3]);
    }

    #[test]
    fn hashmap_containing_event_does_not_false_positive_on_replay() {
        // The real regression test for the bug this design specifically
        // avoids: if replay verification ever re-serialized an
        // already-deserialized event instead of hashing the exact raw
        // bytes on disk, a HashMap field's non-deterministic iteration
        // order could produce a different byte sequence than what was
        // originally written, causing a false "tampering" failure on
        // perfectly valid data. Many keys, to maximize the real chance
        // of a reordering if the raw-bytes fix weren't in place.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct MapEvent {
            props: HashMap<String, String>,
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        let mut props = HashMap::new();
        for i in 0..20 {
            props.insert(format!("key_{i}"), format!("value_{i}"));
        }
        let event = MapEvent { props };

        {
            let (mut log, _) = EventLog::<MapEvent>::open(&path).unwrap();
            log.append(&event).unwrap();
        }

        // Must succeed -- any IntegrityCheckFailed here would mean the
        // raw-bytes fix regressed.
        let (_, replayed) = EventLog::<MapEvent>::open(&path).unwrap();
        assert_eq!(replayed, vec![event]);
    }

    #[test]
    fn tampered_event_body_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        {
            let (mut log, _) = EventLog::<TestEvent>::open(&path).unwrap();
            log.append(&TestEvent::Revoke {
                serial: "abc123".into(),
                reason: "compromised".into(),
            })
            .unwrap();
        }

        // Directly corrupt the on-disk content -- change the reason
        // without recomputing the hash, simulating tampering.
        let contents = std::fs::read_to_string(&path).unwrap();
        let tampered = contents.replace("compromised", "totally-fine-actually");
        std::fs::write(&path, tampered).unwrap();

        let result = EventLog::<TestEvent>::open(&path);
        assert!(matches!(
            result,
            Err(EventLogError::IntegrityCheckFailed { .. })
        ));
    }

    #[test]
    fn tampered_chain_link_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");

        {
            let (mut log, _) = EventLog::<TestEvent>::open(&path).unwrap();
            log.append(&TestEvent::Grant {
                identity: "alice".into(),
                permissions: vec!["read".into()],
            })
            .unwrap();
            log.append(&TestEvent::Grant {
                identity: "bob".into(),
                permissions: vec!["write".into()],
            })
            .unwrap();
        }

        // Corrupt the second entry's prev_hash so it no longer points
        // at the first entry's real hash.
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        let mut second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        second["prev_hash"] = serde_json::Value::String("0".repeat(68));
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{}", lines[0]).unwrap();
        writeln!(file, "{}", serde_json::to_string(&second).unwrap()).unwrap();

        let result = EventLog::<TestEvent>::open(&path);
        assert!(matches!(result, Err(EventLogError::ChainBroken { .. })));
    }
}
