//! The CommandActor: receives signed commands, checks them, runs the handler of a declared one, and
//! announces every step. Design: `docs/architecture/commands.md`.
//!
//! The document's own checks are [`crate::command::CommandVerifier`]'s. The actor adds what only the agent
//! can know, in this order: the command is declared, its arguments suit the declaration, this agent has not
//! already received the id, no other is running if the command is exclusive, and the agent is not draining.
//! Only then does the handler run, under the timeout the declaration gave, and the actor says `accepted`,
//! then `started`, and at the end `succeeded` or `failed`.
//!
//! Every outcome carries names and reasons, never an argument. What a handler says it did is capped, and is
//! the handler's to keep free of secrets.
//!
//! Domain code writes only the handler. A command is announced even when it is refused, so that a command
//! nobody carried out can be seen not to have been.

use crate::command::{Arg, CommandSpec, CommandVerifier, check_args};
use crate::signed::Rejection;
use polar_policy::PolicyMessage;
use polar_scheduler_common::{CommandOutcome, command_status};
use ractor::{Actor, ActorProcessingErr, ActorRef, OutputPort, RpcReplyPort};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// How many ids an agent remembers having received. Past this the oldest are forgotten, which is safe
/// because a command is also bounded by its expiry and the scheduler never dispatches an id twice.
pub const SEEN_IDS: usize = 4096;

/// The most characters of an untrusted label, or of what a handler reports, that an outcome carries.
const MAX_LABEL: usize = 128;
const MAX_DETAIL: usize = 1024;

/// What a handler is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    pub id: String,
    pub command: String,
    pub args: BTreeMap<String, Arg>,
}

/// Runs one command. Returns what it did, in a line, for the record, or why it could not. It must not put an
/// argument value, or anything secret, in either.
#[ractor::async_trait]
pub trait CommandHandler: Send + Sync {
    async fn run(&self, request: CommandRequest) -> Result<String, String>;
}

pub struct CommandActor;

pub struct CommandArgs {
    /// This agent's name, which its outcomes carry.
    pub agent: String,
    /// How documents are checked. `None` refuses every one: there is no CA to verify against.
    pub verifier: Option<CommandVerifier>,
    /// The commands the agent declared, each with its handler.
    pub commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
    pub outcomes: Arc<OutputPort<CommandOutcome>>,
    /// How many ids to remember having received (see [`SEEN_IDS`]).
    pub seen_capacity: usize,
    /// The agent's policy actor, asked before the signer's certificate whether they may run a command.
    pub policy: Option<ActorRef<PolicyMessage>>,
}

pub enum CommandMessage {
    /// A signed command document delivered on the agent's command topic.
    Deliver(Value),
    /// A handler ended (internal).
    Finished(Finished),
    /// The agent is shutting down: accept nothing more.
    Drain,
    /// The ids of the commands running now, for tests and inspection.
    Running(RpcReplyPort<Vec<String>>),
}

pub struct Finished {
    id: String,
    result: Result<String, String>,
}

struct RunningCommand {
    command: String,
    started: Instant,
    signer: String,
    abort: tokio::task::AbortHandle,
}

pub struct CommandState {
    agent: String,
    verifier: Option<CommandVerifier>,
    registry: HashMap<&'static str, (CommandSpec, Arc<dyn CommandHandler>)>,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    running: HashMap<String, RunningCommand>,
    draining: bool,
    outcomes: Arc<OutputPort<CommandOutcome>>,
    sequence: u64,
    seen_capacity: usize,
    policy: Option<ActorRef<PolicyMessage>>,
}

fn label(text: &str) -> String {
    text.chars().take(MAX_LABEL).collect()
}

fn capped(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_DETAIL).collect();
    if text.chars().count() > MAX_DETAIL {
        out.push('…');
    }
    out
}

/// What the document says its id and command are, read without trusting any of it: only to label an
/// outcome, and shortened.
fn claimed(document: &Value) -> (Option<String>, Option<String>) {
    let payload = &document["payload"];
    (
        payload["id"].as_str().map(label),
        payload["command"].as_str().map(label),
    )
}

impl CommandState {
    fn announce(
        &mut self,
        id: Option<String>,
        command: Option<String>,
        status: &str,
        detail: Vec<String>,
        signer: Option<String>,
        duration: Option<Duration>,
    ) {
        self.sequence += 1;
        self.outcomes.send(CommandOutcome {
            agent: self.agent.clone(),
            id,
            command,
            status: status.to_string(),
            detail,
            signer,
            duration_ms: duration.map(|d| d.as_millis() as u64),
            at_unix_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            sequence: self.sequence,
        });
    }

    fn refuse_unverified(&mut self, document: &Value, status: &str, why: String) {
        warn!(reason = %why, "signed command {status}");
        let (id, command) = claimed(document);
        self.announce(id, command, status, vec![why], None, None);
    }

    fn remember(&mut self, id: &str) {
        if self.seen_order.len() >= self.seen_capacity.max(1)
            && let Some(oldest) = self.seen_order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        self.seen.insert(id.to_string());
        self.seen_order.push_back(id.to_string());
    }

    async fn deliver(&mut self, document: &Value, myself: &ActorRef<CommandMessage>) {
        let Some(verifier) = &self.verifier else {
            return self.refuse_unverified(
                document,
                command_status::REFUSED,
                Rejection::NotConfigured.to_string(),
            );
        };
        // In stages with the question of authority in the middle: the signature and the shape, then whether
        // this signer may run this command (the policy first, the certificate second), then revocation and
        // the command's time.
        let opened = match verifier.open(document) {
            Ok(opened) => opened,
            Err(rejection) => {
                return self.refuse_unverified(
                    document,
                    command_status::REFUSED,
                    rejection.to_string(),
                );
            }
        };
        let request = opened.request(&self.agent);
        let floor = verifier.floor(&opened);
        let admitted =
            crate::policy_authority::admit(self.policy.as_ref(), &request, || floor.clone()).await;
        if let Err(refusal) = admitted {
            return self.refuse_unverified(
                document,
                command_status::REFUSED,
                Rejection::Unauthorized(refusal).to_string(),
            );
        }
        let Some(verifier) = &self.verifier else {
            return self.refuse_unverified(
                document,
                command_status::REFUSED,
                Rejection::NotConfigured.to_string(),
            );
        };
        let verified = match verifier.close(opened) {
            Ok(verified) => verified,
            Err(rejection @ Rejection::Window(crate::command::WindowError::Expired { .. })) => {
                return self.refuse_unverified(
                    document,
                    command_status::EXPIRED,
                    rejection.to_string(),
                );
            }
            Err(rejection) => {
                return self.refuse_unverified(
                    document,
                    command_status::REFUSED,
                    rejection.to_string(),
                );
            }
        };

        let id = Some(verified.id.clone());
        let command = Some(verified.command.clone());
        let signer = Some(format!(
            "{} (serial {})",
            verified
                .signer
                .username
                .as_deref()
                .or(verified.signer.identity.as_deref())
                .unwrap_or("unknown"),
            verified.signer.serial
        ));
        let refuse = |this: &mut Self, status: &str, detail: Vec<String>| {
            warn!(?detail, id = %verified.id, command = %verified.command, "signed command {status}");
            this.announce(
                id.clone(),
                command.clone(),
                status,
                detail,
                signer.clone(),
                None,
            );
        };

        // Declared, and its arguments suit the declaration.
        let Some((spec, handler)) = self.registry.get(verified.command.as_str()).cloned() else {
            return refuse(
                self,
                command_status::REFUSED,
                vec![format!("this agent has no command `{}`", verified.command)],
            );
        };
        if let Err(problems) = check_args(&spec, &verified.args) {
            return refuse(self, command_status::REFUSED, problems);
        }
        // Not received before.
        if self.seen.contains(&verified.id) {
            return refuse(
                self,
                command_status::DUPLICATE,
                vec!["this agent has already received this id".to_string()],
            );
        }
        // Not busy, and not shutting down.
        if spec.exclusive
            && self
                .running
                .values()
                .any(|running| running.command == verified.command)
        {
            return refuse(
                self,
                command_status::REFUSED,
                vec![format!("`{}` is already running", verified.command)],
            );
        }
        if self.draining {
            return refuse(
                self,
                command_status::REFUSED,
                vec!["the agent is shutting down".to_string()],
            );
        }

        // Accepted. The id is used up from here, whatever the handler does.
        self.remember(&verified.id);
        info!(id = %verified.id, command = %verified.command, "signed command accepted");
        self.announce(
            id.clone(),
            command.clone(),
            command_status::ACCEPTED,
            vec![],
            signer.clone(),
            None,
        );

        let request = CommandRequest {
            id: verified.id.clone(),
            command: verified.command.clone(),
            args: verified.args,
        };
        let timeout = spec.timeout;
        let join = tokio::spawn(async move { handler.run(request).await });
        let abort = join.abort_handle();
        let abort_on_timeout = abort.clone();
        let finished_id = verified.id.clone();
        let reply = myself.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(timeout, join).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) if error.is_panic() => Err("the handler panicked".to_string()),
                Ok(Err(_)) => Err("the handler was cancelled".to_string()),
                Err(_) => {
                    // The handler is stopped, not left running past what it was allowed.
                    abort_on_timeout.abort();
                    Err(format!("timed out after {} seconds", timeout.as_secs()))
                }
            };
            let _ = reply.cast(CommandMessage::Finished(Finished {
                id: finished_id,
                result,
            }));
        });
        self.running.insert(
            verified.id.clone(),
            RunningCommand {
                command: verified.command.clone(),
                started: Instant::now(),
                signer: signer.clone().unwrap_or_default(),
                abort,
            },
        );
        self.announce(id, command, command_status::STARTED, vec![], signer, None);
    }
}

#[ractor::async_trait]
impl Actor for CommandActor {
    type Msg = CommandMessage;
    type State = CommandState;
    type Arguments = CommandArgs;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        args: CommandArgs,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(CommandState {
            agent: args.agent,
            verifier: args.verifier,
            registry: args
                .commands
                .into_iter()
                .map(|(spec, handler)| (spec.name, (spec, handler)))
                .collect(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            running: HashMap::new(),
            draining: false,
            outcomes: args.outcomes,
            sequence: 0,
            seen_capacity: args.seen_capacity,
            policy: args.policy,
        })
    }

    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            CommandMessage::Deliver(document) => state.deliver(&document, &myself).await,
            CommandMessage::Finished(Finished { id, result }) => {
                // An interrupted command was announced when the actor stopped, and is not here.
                if let Some(running) = state.running.remove(&id) {
                    let took = running.started.elapsed();
                    let (status, detail) = match result {
                        Ok(done) => (command_status::SUCCEEDED, vec![capped(&done)]),
                        Err(why) => (command_status::FAILED, vec![capped(&why)]),
                    };
                    state.announce(
                        Some(id),
                        Some(running.command),
                        status,
                        detail,
                        Some(running.signer),
                        Some(took),
                    );
                }
            }
            CommandMessage::Drain => state.draining = true,
            CommandMessage::Running(reply) => {
                let mut ids: Vec<String> = state.running.keys().cloned().collect();
                ids.sort();
                let _ = reply.send(ids);
            }
        }
        Ok(())
    }

    /// A command still running when the actor stops is stopped and reported, not lost: "failed,
    /// interrupted" is what an operator reads, and it never succeeded.
    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        let running: Vec<(String, RunningCommand)> = state.running.drain().collect();
        for (id, command) in running {
            command.abort.abort();
            let took = command.started.elapsed();
            state.announce(
                Some(id),
                Some(command.command),
                command_status::FAILED,
                vec!["interrupted by a shutdown".to_string()],
                Some(command.signer),
                Some(took),
            );
        }
        Ok(())
    }
}

/// How the rest of an agent uses the CommandActor, without touching ractor.
#[derive(Clone)]
pub struct CommandHandle {
    actor: ActorRef<CommandMessage>,
    outcomes: Arc<OutputPort<CommandOutcome>>,
}

impl CommandHandle {
    /// Starts the actor as a child of `supervisor`, with no policy: the certificate alone decides.
    pub async fn spawn(
        name: Option<String>,
        agent: impl Into<String>,
        verifier: Option<CommandVerifier>,
        commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
        supervisor: ractor::ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        Self::spawn_with_policy(name, agent, verifier, commands, None, supervisor).await
    }

    /// Starts the actor with the agent's policy actor, which is asked before the signer's certificate
    /// whether they may run a command.
    pub async fn spawn_with_policy(
        name: Option<String>,
        agent: impl Into<String>,
        verifier: Option<CommandVerifier>,
        commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
        policy: Option<ActorRef<PolicyMessage>>,
        supervisor: ractor::ActorCell,
    ) -> Result<Self, ActorProcessingErr> {
        let outcomes = Arc::new(OutputPort::default());
        let (actor, _) = Actor::spawn_linked(
            name,
            CommandActor,
            CommandArgs {
                agent: agent.into(),
                verifier,
                commands,
                outcomes: outcomes.clone(),
                seen_capacity: SEEN_IDS,
                policy,
            },
            supervisor,
        )
        .await
        .map_err(ActorProcessingErr::from)?;
        Ok(Self { actor, outcomes })
    }

    /// Hands over a signed command document, in order with the others. What became of it is announced.
    pub fn deliver(&self, document: Value) {
        if let Err(e) = self.actor.cast(CommandMessage::Deliver(document)) {
            warn!("the command actor did not take a delivered command: {e}");
        }
    }

    /// The agent is shutting down: no further command is accepted.
    pub fn drain(&self) {
        let _ = self.actor.cast(CommandMessage::Drain);
    }

    /// The ids of the commands running now.
    pub async fn running(&self) -> Vec<String> {
        match self
            .actor
            .call(CommandMessage::Running, Some(Duration::from_secs(5)))
            .await
        {
            Ok(ractor::rpc::CallResult::Success(ids)) => ids,
            _ => Vec::new(),
        }
    }

    /// Where each step is announced. The infrastructure puts these on Cassini for the scheduler.
    pub fn outcomes(&self) -> Arc<OutputPort<CommandOutcome>> {
        self.outcomes.clone()
    }
}

/// The built-in subscription every agent has to its own command topic: the scheduler processor publishes a
/// signed command for `agent` there. A message that is not a command notification, or is for another agent,
/// is dropped; anything else is verified by the actor, whoever published it.
pub fn subscription(agent: &str, commands: CommandHandle) -> crate::Subscription {
    let agent = agent.to_string();
    crate::Subscription::new(
        polar_scheduler_common::command_topic(&agent),
        move |incoming| {
            let notification = match rkyv::from_bytes::<
                polar_scheduler_common::CommandNotification,
                rkyv::rancor::Error,
            >(&incoming.payload)
            {
                Ok(notification) => notification,
                Err(e) => {
                    warn!("a message on the command topic is not a notification: {e}");
                    return;
                }
            };
            if notification.agent != agent {
                warn!(
                    "a command notification for `{}` reached `{agent}`; dropped",
                    notification.agent
                );
                return;
            }
            match serde_json::from_str::<Value>(&notification.document_json) {
                Ok(document) => commands.deliver(document),
                Err(e) => warn!("a delivered command is not JSON: {e}"),
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{ArgKind, ArgSpec, Delivery};
    use crate::signed::UpdateVerifier;
    use polar_signing::testing::{self as certs, Spec, now, root, signer};
    use polar_signing::{Trust, sign};
    use serde_json::json;
    use std::sync::Mutex;

    const AGENT: &str = "git-agent";

    const BACKUP_ARGS: &[ArgSpec] = &[
        ArgSpec {
            name: "destination",
            kind: ArgKind::Text,
            required: true,
            doc: "where it goes",
        },
        ArgSpec {
            name: "keep",
            kind: ArgKind::Number,
            required: false,
            doc: "how many to keep",
        },
    ];

    const BACKUP: CommandSpec = CommandSpec {
        name: "backup-now",
        doc: "back up now",
        args: BACKUP_ARGS,
        delivery: Delivery::Once,
        exclusive: true,
        timeout: Duration::from_secs(5),
    };

    const PING: CommandSpec = CommandSpec {
        name: "ping",
        doc: "answer",
        args: &[],
        delivery: Delivery::Idempotent,
        exclusive: false,
        timeout: Duration::from_secs(5),
    };

    const SLOW: CommandSpec = CommandSpec {
        name: "slow",
        doc: "takes too long",
        args: &[],
        delivery: Delivery::Idempotent,
        exclusive: true,
        timeout: Duration::from_millis(150),
    };

    /// Records what it was asked, and says it is done.
    struct Recording(Arc<Mutex<Vec<CommandRequest>>>, String);

    #[ractor::async_trait]
    impl CommandHandler for Recording {
        async fn run(&self, request: CommandRequest) -> Result<String, String> {
            self.0.lock().unwrap().push(request);
            Ok(self.1.clone())
        }
    }

    struct Failing;
    #[ractor::async_trait]
    impl CommandHandler for Failing {
        async fn run(&self, _: CommandRequest) -> Result<String, String> {
            Err("the disk is full".to_string())
        }
    }

    struct Panicking;
    #[ractor::async_trait]
    impl CommandHandler for Panicking {
        async fn run(&self, _: CommandRequest) -> Result<String, String> {
            panic!("something broke")
        }
    }

    /// Waits until the test lets it go.
    struct Gate(Arc<tokio::sync::Notify>, Arc<Mutex<Vec<String>>>);
    #[ractor::async_trait]
    impl CommandHandler for Gate {
        async fn run(&self, request: CommandRequest) -> Result<String, String> {
            self.1.lock().unwrap().push(request.id.clone());
            self.0.notified().await;
            Ok("released".to_string())
        }
    }

    /// Never ends, as far as a test can tell, and says if it was dropped while it waited.
    struct Forever(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
    );

    struct Dropped(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[ractor::async_trait]
    impl CommandHandler for Forever {
        async fn run(&self, _: CommandRequest) -> Result<String, String> {
            let _dropped = Dropped(self.1.clone());
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok("unreachable".to_string())
        }
    }

    struct Sink(Arc<Mutex<Vec<CommandOutcome>>>);
    #[ractor::async_trait]
    impl Actor for Sink {
        type Msg = CommandOutcome;
        type State = Arc<Mutex<Vec<CommandOutcome>>>;
        type Arguments = ();
        async fn pre_start(
            &self,
            _: ActorRef<Self::Msg>,
            _: (),
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(self.0.clone())
        }
        async fn handle(
            &self,
            _: ActorRef<Self::Msg>,
            message: Self::Msg,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            state.lock().unwrap().push(message);
            Ok(())
        }
    }

    struct Rig {
        ca: certs::Authority,
        handle: CommandHandle,
        seen: Arc<Mutex<Vec<CommandOutcome>>>,
    }

    impl Rig {
        async fn with(
            commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
            verifier: bool,
            seen_capacity: usize,
        ) -> Self {
            let ca = root("Polar Internal CA");
            let verifier = verifier.then(|| {
                UpdateVerifier::new(
                    Trust::from_pem(std::slice::from_ref(&ca.pem)).unwrap(),
                    AGENT,
                )
                .with_clock(now)
                .commands()
            });
            let outcomes = Arc::new(OutputPort::default());
            let (actor, _) = Actor::spawn(
                None,
                CommandActor,
                CommandArgs {
                    agent: AGENT.to_string(),
                    verifier,
                    commands,
                    outcomes: outcomes.clone(),
                    seen_capacity,
                    policy: None,
                },
            )
            .await
            .unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let (sink, _) = Actor::spawn(None, Sink(seen.clone()), ()).await.unwrap();
            outcomes.subscribe(sink, Some);
            Self {
                ca,
                handle: CommandHandle { actor, outcomes },
                seen,
            }
        }

        async fn new(commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>) -> Self {
            Self::with(commands, true, SEEN_IDS).await
        }

        fn document(&self, capability: &'static str, payload: Value) -> Value {
            let s = signer(
                &self.ca,
                Spec {
                    capabilities: vec![capability],
                    not_after_year: 2090,
                    ..Spec::default()
                },
            );
            sign(payload, &[s.cert_pem], &s.key_pem).unwrap().to_value()
        }

        fn command(&self, id: &str, command: &str, args: Value) -> Value {
            self.document("c:run-command", payload(id, command, args))
        }

        fn deliver(&self, document: Value) {
            self.handle.deliver(document);
        }

        async fn settle(&self) {
            tokio::time::sleep(Duration::from_millis(80)).await;
        }

        fn outcomes(&self) -> Vec<CommandOutcome> {
            self.seen.lock().unwrap().clone()
        }

        fn statuses(&self) -> Vec<String> {
            self.outcomes().into_iter().map(|o| o.status).collect()
        }
    }

    fn payload(id: &str, command: &str, args: Value) -> Value {
        let issued = now() as u64;
        json!({
            "type": "command",
            "id": id,
            "target": {"agent": AGENT},
            "command": command,
            "args": args,
            "issued_at": issued,
            "not_after": issued + 900
        })
    }

    fn recording() -> (Arc<Mutex<Vec<CommandRequest>>>, Arc<dyn CommandHandler>) {
        let ran = Arc::new(Mutex::new(Vec::new()));
        (
            ran.clone(),
            Arc::new(Recording(ran, "backed up".to_string())),
        )
    }

    // -- the ordinary run -------------------------------------------------------------------

    #[tokio::test]
    async fn a_good_command_is_accepted_started_and_succeeds_and_its_handler_gets_the_request() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(BACKUP, handler)]).await;
        rig.deliver(rig.command(
            "run-1",
            "backup-now",
            json!({"destination": "nightly", "keep": 7}),
        ));
        rig.settle().await;

        assert_eq!(rig.statuses(), ["accepted", "started", "succeeded"]);
        let outcomes = rig.outcomes();
        for o in &outcomes {
            assert_eq!(o.agent, AGENT);
            assert_eq!(o.id.as_deref(), Some("run-1"));
            assert_eq!(o.command.as_deref(), Some("backup-now"));
            assert!(o.signer.as_deref().unwrap().contains("alice"));
            assert!(o.at_unix_secs > 1_700_000_000);
        }
        assert_eq!(outcomes[2].detail, vec!["backed up".to_string()]);
        assert!(outcomes[2].duration_ms.is_some());
        assert_eq!(
            outcomes.iter().map(|o| o.sequence).collect::<Vec<_>>(),
            [1, 2, 3]
        );

        {
            let ran = ran.lock().unwrap();
            assert_eq!(ran.len(), 1);
            assert_eq!(ran[0].id, "run-1");
            assert_eq!(ran[0].command, "backup-now");
            assert_eq!(
                ran[0].args,
                BTreeMap::from([
                    ("destination".to_string(), Arg::Text("nightly".into())),
                    ("keep".to_string(), Arg::Number(7)),
                ])
            );
        }
        assert!(rig.handle.running().await.is_empty());
    }

    #[tokio::test]
    async fn no_outcome_ever_holds_an_argument_value() {
        let (_, handler) = recording();
        let rig = Rig::new(vec![(BACKUP, handler)]).await;
        let secret = "hunter2-the-secret-value";
        // Run, refused for a wrong kind, refused as unknown: none repeats the value.
        rig.deliver(rig.command("a", "backup-now", json!({"destination": secret})));
        rig.deliver(rig.command("b", "backup-now", json!({"destination": 5, "keep": secret})));
        rig.deliver(rig.command(
            "c",
            "backup-now",
            json!({"destination": secret, "extra": secret}),
        ));
        rig.settle().await;
        let everything = format!("{:?}", rig.outcomes());
        assert!(!everything.contains("hunter2"), "{everything}");
        assert!(rig.statuses().iter().filter(|s| *s == "refused").count() >= 2);
    }

    #[tokio::test]
    async fn an_id_this_agent_has_received_is_a_duplicate_and_the_handler_runs_once() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(BACKUP, handler)]).await;
        let doc = rig.command("same", "backup-now", json!({"destination": "x"}));
        rig.deliver(doc.clone());
        rig.settle().await;
        rig.deliver(doc);
        rig.settle().await;
        assert_eq!(
            rig.statuses(),
            ["accepted", "started", "succeeded", "duplicate"]
        );
        assert_eq!(ran.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_set_of_ids_remembered_is_bounded_and_the_oldest_is_forgotten() {
        let (ran, handler) = recording();
        let rig = Rig::with(vec![(PING, handler)], true, 3).await;
        for i in 0..4 {
            rig.deliver(rig.command(&format!("id-{i}"), "ping", json!({})));
        }
        rig.settle().await;
        assert_eq!(ran.lock().unwrap().len(), 4);
        // id-0 has been pushed out of a set of three; id-3 has not.
        rig.deliver(rig.command("id-3", "ping", json!({})));
        rig.deliver(rig.command("id-0", "ping", json!({})));
        rig.settle().await;
        let last: Vec<String> = rig.statuses().into_iter().rev().take(4).collect();
        assert!(last.contains(&"duplicate".to_string()), "{last:?}");
        assert_eq!(ran.lock().unwrap().len(), 5, "id-0 ran again, id-3 did not");
    }

    // -- what is refused ---------------------------------------------------------------------

    #[tokio::test]
    async fn arguments_that_do_not_suit_the_declaration_are_refused_and_the_id_is_not_used_up() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(BACKUP, handler)]).await;
        let bad = rig.command("bad", "backup-now", json!({"keep": 7, "surprise": 1}));
        rig.deliver(bad.clone());
        rig.deliver(bad); // the same document again
        rig.settle().await;
        let outcomes = rig.outcomes();
        // Refused both times, not "duplicate" the second: a refusal consumes nothing.
        assert_eq!(rig.statuses(), ["refused", "refused"]);
        let detail = outcomes[0].detail.join(" | ");
        assert!(
            detail.contains("surprise") && detail.contains("destination"),
            "{detail}"
        );
        assert!(
            outcomes[0].signer.is_some(),
            "it was verified, then refused for its arguments"
        );
        assert!(ran.lock().unwrap().is_empty());

        // The id was not used up: the same id, corrected and signed again, is accepted and runs.
        rig.deliver(rig.command("bad", "backup-now", json!({"destination": "d"})));
        rig.settle().await;
        assert_eq!(
            rig.statuses(),
            ["refused", "refused", "accepted", "started", "succeeded"]
        );
        assert_eq!(ran.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_command_the_agent_does_not_declare_is_refused() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(PING, handler)]).await;
        rig.deliver(rig.command("x", "wipe-everything", json!({})));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["refused"]);
        assert!(rig.outcomes()[0].detail[0].contains("no command `wipe-everything`"));
        assert!(ran.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_agent_that_declares_no_commands_still_says_it_refused_one() {
        let rig = Rig::new(vec![]).await;
        rig.deliver(rig.command("x", "backup-now", json!({"destination": "d"})));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["refused"]);
    }

    #[tokio::test]
    async fn what_is_not_signed_properly_is_refused_and_announced_without_a_signer() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(PING, handler)]).await;

        // Unsigned (just the payload), altered after signing, for another agent, and from a signer who
        // may not run commands.
        rig.deliver(payload("unsigned", "ping", json!({})));
        let mut altered = rig.command("altered", "ping", json!({}));
        altered["payload"]["command"] = json!("backup-now");
        rig.deliver(altered);
        let mut elsewhere = payload("elsewhere", "ping", json!({}));
        elsewhere["target"]["agent"] = json!("another-agent");
        rig.deliver(rig.document("c:run-command", elsewhere));
        rig.deliver(rig.document(
            "c:update-configuration",
            payload("not-allowed", "ping", json!({})),
        ));
        rig.settle().await;

        assert_eq!(rig.statuses(), ["refused", "refused", "refused", "refused"]);
        let outcomes = rig.outcomes();
        assert!(outcomes.iter().all(|o| o.signer.is_none()));
        // What each said it was, as a label only.
        assert_eq!(outcomes[1].id.as_deref(), Some("altered"));
        assert_eq!(outcomes[2].id.as_deref(), Some("elsewhere"));
        assert!(ran.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_expired_command_is_announced_as_expired_and_one_not_yet_due_is_refused() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(PING, handler)]).await;
        let issued = now() as u64;
        let mut old = payload("old", "ping", json!({}));
        old["issued_at"] = json!(issued - 2000);
        old["not_after"] = json!(issued - 1);
        rig.deliver(rig.document("c:run-command", old));
        let mut early = payload("early", "ping", json!({}));
        early["not_before"] = json!(issued + 600);
        rig.deliver(rig.document("c:run-command", early));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["expired", "refused"]);
        assert!(ran.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_agent_with_no_ca_to_verify_against_refuses_every_command() {
        let (ran, handler) = recording();
        let rig = Rig::with(vec![(PING, handler)], false, SEEN_IDS).await;
        rig.deliver(rig.command("x", "ping", json!({})));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["refused"]);
        assert!(rig.outcomes()[0].detail[0].contains("no CA"));
        assert!(ran.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn labels_taken_from_an_unverified_document_are_shortened() {
        let rig = Rig::new(vec![]).await;
        let long = "x".repeat(5000);
        rig.deliver(json!({"payload": {"id": long, "command": long}, "signature": {}}));
        rig.settle().await;
        let o = &rig.outcomes()[0];
        assert_eq!(o.id.as_deref().unwrap().chars().count(), MAX_LABEL);
        assert_eq!(o.command.as_deref().unwrap().chars().count(), MAX_LABEL);
    }

    // -- running ----------------------------------------------------------------------------

    #[tokio::test]
    async fn a_second_run_of_an_exclusive_command_is_refused_while_one_is_running_and_allowed_after()
     {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(Mutex::new(Vec::new()));
        let rig = Rig::new(vec![(
            BACKUP,
            Arc::new(Gate(release.clone(), started.clone())),
        )])
        .await;
        let args = json!({"destination": "d"});

        rig.deliver(rig.command("first", "backup-now", args.clone()));
        rig.settle().await;
        assert_eq!(rig.handle.running().await, ["first"]);

        rig.deliver(rig.command("second", "backup-now", args.clone()));
        rig.settle().await;
        let refused = rig
            .outcomes()
            .into_iter()
            .find(|o| o.id.as_deref() == Some("second"))
            .unwrap();
        assert_eq!(refused.status, "refused");
        assert!(refused.detail[0].contains("already running"));

        release.notify_one();
        rig.settle().await;
        assert!(rig.handle.running().await.is_empty());
        // The refused one was not used up: it may be sent again now.
        rig.deliver(rig.command("second", "backup-now", args));
        rig.settle().await;
        release.notify_one();
        rig.settle().await;
        let second: Vec<String> = rig
            .outcomes()
            .into_iter()
            .filter(|o| o.id.as_deref() == Some("second"))
            .map(|o| o.status)
            .collect();
        assert_eq!(second, ["refused", "accepted", "started", "succeeded"]);
    }

    #[tokio::test]
    async fn a_command_that_is_not_exclusive_may_run_alongside_itself() {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(Mutex::new(Vec::new()));
        let mut concurrent = PING;
        concurrent.name = "ping";
        let rig = Rig::new(vec![(
            concurrent,
            Arc::new(Gate(release.clone(), started.clone())),
        )])
        .await;
        rig.deliver(rig.command("a", "ping", json!({})));
        rig.deliver(rig.command("b", "ping", json!({})));
        rig.settle().await;
        assert_eq!(rig.handle.running().await, ["a", "b"]);
        release.notify_waiters();
        rig.settle().await;
        assert!(rig.handle.running().await.is_empty());
    }

    #[tokio::test]
    async fn a_draining_agent_accepts_nothing_and_what_it_refused_is_not_used_up() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(PING, handler)]).await;
        rig.handle.drain();
        let doc = rig.command("late", "ping", json!({}));
        rig.deliver(doc);
        rig.settle().await;
        assert_eq!(rig.statuses(), ["refused"]);
        assert!(rig.outcomes()[0].detail[0].contains("shutting down"));
        assert!(ran.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_handler_that_takes_too_long_is_stopped_and_reported_and_the_agent_carries_on() {
        let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (ran, ping) = recording();
        let rig = Rig::new(vec![
            (SLOW, Arc::new(Forever(reached.clone(), stopped.clone()))),
            (PING, ping),
        ])
        .await;
        rig.deliver(rig.command("slow-1", "slow", json!({})));
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(rig.statuses(), ["accepted", "started", "failed"]);
        let failed = rig.outcomes().pop().unwrap();
        assert!(
            failed.detail[0].contains("timed out"),
            "{:?}",
            failed.detail
        );
        assert!(failed.duration_ms.unwrap() >= 100);
        assert!(rig.handle.running().await.is_empty());
        assert!(
            stopped.load(std::sync::atomic::Ordering::SeqCst),
            "the handler was stopped, not left running in the background"
        );

        // A command after it works, and the exclusive one is free to be tried again.
        rig.deliver(rig.command("ping-1", "ping", json!({})));
        rig.settle().await;
        assert_eq!(ran.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_handler_that_fails_or_panics_is_reported_and_the_agent_carries_on() {
        let (ran, ping) = recording();
        let mut failing = BACKUP;
        failing.name = "fails";
        failing.args = &[];
        let mut panicking = BACKUP;
        panicking.name = "panics";
        panicking.args = &[];
        let rig = Rig::new(vec![
            (failing, Arc::new(Failing)),
            (panicking, Arc::new(Panicking)),
            (PING, ping),
        ])
        .await;
        rig.deliver(rig.command("f", "fails", json!({})));
        rig.deliver(rig.command("p", "panics", json!({})));
        rig.deliver(rig.command("ok", "ping", json!({})));
        rig.settle().await;
        let by_id = |id: &str| -> Vec<CommandOutcome> {
            rig.outcomes()
                .into_iter()
                .filter(|o| o.id.as_deref() == Some(id))
                .collect()
        };
        let f = by_id("f");
        assert_eq!(f.last().unwrap().status, "failed");
        assert_eq!(
            f.last().unwrap().detail,
            vec!["the disk is full".to_string()]
        );
        let p = by_id("p");
        assert_eq!(p.last().unwrap().status, "failed");
        assert!(p.last().unwrap().detail[0].contains("panicked"));
        assert_eq!(by_id("ok").last().unwrap().status, "succeeded");
        assert_eq!(ran.lock().unwrap().len(), 1);
        assert!(rig.handle.running().await.is_empty());
    }

    #[tokio::test]
    async fn what_a_handler_reports_is_capped() {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let rig = Rig::new(vec![(PING, Arc::new(Recording(ran, "y".repeat(5000))))]).await;
        rig.deliver(rig.command("long", "ping", json!({})));
        rig.settle().await;
        let done = rig.outcomes().pop().unwrap();
        assert_eq!(done.detail[0].chars().count(), MAX_DETAIL + 1);
        assert!(done.detail[0].ends_with('…'));
    }

    #[tokio::test]
    async fn a_command_still_running_when_the_actor_stops_is_reported_as_interrupted() {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(Mutex::new(Vec::new()));
        let rig = Rig::new(vec![(BACKUP, Arc::new(Gate(release, started)))]).await;
        rig.deliver(rig.command("long-run", "backup-now", json!({"destination": "d"})));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["accepted", "started"]);

        rig.handle
            .actor
            .stop(Some("the agent is shutting down".to_string()));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let last = rig.outcomes().pop().unwrap();
        assert_eq!(last.status, "failed");
        assert!(last.detail[0].contains("interrupted"), "{:?}", last.detail);
        assert_eq!(last.id.as_deref(), Some("long-run"));
        assert!(last.signer.is_some());
    }

    // -- the subscription --------------------------------------------------------------------

    fn notification(agent: &str, document: &Value) -> polar_router::IncomingMessage {
        let payload =
            rkyv::to_bytes::<rkyv::rancor::Error>(&polar_scheduler_common::CommandNotification {
                agent: agent.to_string(),
                document_json: document.to_string(),
            })
            .unwrap()
            .to_vec();
        polar_router::IncomingMessage {
            topic: polar_scheduler_common::command_topic(agent),
            payload,
            published_by: None,
            trace_ctx: None,
        }
    }

    #[tokio::test]
    async fn a_command_delivered_on_the_topic_is_verified_and_run_in_order() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(PING, handler)]).await;
        let sub = subscription(AGENT, rig.handle.clone());
        assert_eq!(sub.topic, "agent.git-agent.command");
        (sub.deliver)(notification(AGENT, &rig.command("one", "ping", json!({}))));
        (sub.deliver)(notification(AGENT, &rig.command("two", "ping", json!({}))));
        rig.settle().await;
        let ids: Vec<String> = ran.lock().unwrap().iter().map(|r| r.id.clone()).collect();
        assert_eq!(ids, ["one", "two"]);
    }

    #[tokio::test]
    async fn the_topic_is_not_trusted_a_forged_command_is_refused_and_junk_is_dropped() {
        let (ran, handler) = recording();
        let rig = Rig::new(vec![(PING, handler)]).await;
        let sub = subscription(AGENT, rig.handle.clone());

        // An unsigned command on the topic, as anyone who can publish there could send it.
        (sub.deliver)(notification(AGENT, &payload("forged", "ping", json!({}))));
        // For another agent, not a notification, and a document that is not JSON.
        (sub.deliver)(notification(
            "another-agent",
            &rig.command("x", "ping", json!({})),
        ));
        (sub.deliver)(polar_router::IncomingMessage {
            topic: sub.topic.clone(),
            payload: vec![1, 2, 3],
            published_by: None,
            trace_ctx: None,
        });
        let mut bad = notification(AGENT, &json!({}));
        bad.payload =
            rkyv::to_bytes::<rkyv::rancor::Error>(&polar_scheduler_common::CommandNotification {
                agent: AGENT.to_string(),
                document_json: "not json".to_string(),
            })
            .unwrap()
            .to_vec();
        (sub.deliver)(bad);
        rig.settle().await;
        // Only the forged one reached the actor, and it was refused.
        assert_eq!(rig.statuses(), ["refused"]);
        assert!(ran.lock().unwrap().is_empty());
    }

    // -- policy first, the certificate second ------------------------------------------------------

    impl Rig {
        /// A rig whose agent has a policy actor with `policy_source` loaded.
        async fn with_policy(
            commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
            policy_source: &str,
        ) -> Self {
            use polar_permissions::PermissionsActor;
            use polar_policy::{PolicyActor, PolicyActorArgs, PolicyMessage};
            let ca = root("Polar Internal CA");
            let verifier = UpdateVerifier::new(
                Trust::from_pem(std::slice::from_ref(&ca.pem)).unwrap(),
                AGENT,
            )
            .with_clock(now)
            .commands();
            let (permissions, _) = Actor::spawn(None, PermissionsActor, ()).await.unwrap();
            let (policy, _) = Actor::spawn(
                None,
                PolicyActor,
                PolicyActorArgs {
                    permissions_actor: permissions,
                },
            )
            .await
            .unwrap();
            ractor::call!(policy, |reply| PolicyMessage::UpdatePolicy {
                policy_source: policy_source.to_string(),
                reply,
            })
            .unwrap()
            .unwrap();
            let outcomes = Arc::new(OutputPort::default());
            let (actor, _) = Actor::spawn(
                None,
                CommandActor,
                CommandArgs {
                    agent: AGENT.to_string(),
                    verifier: Some(verifier),
                    commands,
                    outcomes: outcomes.clone(),
                    seen_capacity: SEEN_IDS,
                    policy: Some(policy),
                },
            )
            .await
            .unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let (sink, _) = Actor::spawn(None, Sink(seen.clone()), ()).await.unwrap();
            outcomes.subscribe(sink, Some);
            Self {
                ca,
                handle: CommandHandle { actor, outcomes },
                seen,
            }
        }
    }

    #[tokio::test]
    async fn a_policy_that_permits_lets_a_signer_run_a_command_their_certificate_would_not() {
        let (ran, handler) = recording();
        let rig = Rig::with_policy(
            vec![(PING, handler)],
            r#"permit(principal == Identity::"alice", action == Action::"run-command", resource == Command::"git-agent/ping");"#,
        )
        .await;
        // alice holds update-configuration only, not run-command.
        rig.deliver(rig.document(
            "c:update-configuration",
            payload("by-policy", "ping", json!({})),
        ));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["accepted", "started", "succeeded"]);
        assert_eq!(ran.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_policy_that_forbids_refuses_a_command_from_a_signer_whose_certificate_would_admit() {
        let (ran, handler) = recording();
        let rig = Rig::with_policy(
            vec![(PING, handler)],
            r#"forbid(principal == Identity::"alice", action == Action::"run-command", resource == Command::"git-agent/ping");"#,
        )
        .await;
        rig.deliver(rig.command("forbidden", "ping", json!({})));
        rig.settle().await;
        assert_eq!(rig.statuses(), ["refused"]);
        let detail = rig.outcomes()[0].detail.join(" ");
        assert!(
            detail.contains("policy") && detail.contains("ping"),
            "{detail}"
        );
        assert!(ran.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_policy_that_does_not_mention_the_command_leaves_it_to_the_certificate() {
        let (ran, handler) = recording();
        let rig = Rig::with_policy(
            vec![(PING, handler)],
            r#"forbid(principal == Identity::"alice", action == Action::"run-command", resource == Command::"git-agent/something-else");"#,
        )
        .await;
        rig.deliver(rig.command("by-certificate", "ping", json!({})));
        // One at a time: the first runs on its own task, and its last outcome would otherwise race the
        // second's refusal.
        rig.settle().await;
        rig.deliver(rig.document(
            "c:update-configuration",
            payload("neither", "ping", json!({})),
        ));
        rig.settle().await;
        assert_eq!(
            rig.statuses(),
            ["accepted", "started", "succeeded", "refused"]
        );
        assert_eq!(ran.lock().unwrap().len(), 1);
    }
}
