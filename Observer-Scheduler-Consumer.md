# Polar Actor Model Architecture

We seek to implement, as much as possible, a framework for observation of networked resources, using the Actor model as an implementation detail. The Actor model of concurrency has numerous benefits for our application, some of which will be described implicitly in the following write-up. However, we will not re-hash most of this here. Our paper on Polar covers these topics in greater detail.

The Polar Information Update Model identifies:
 - A general purpose "User", who carries out multiple roles
 - Support infrastructure: version control, message broker, graph database
 - Observables: Systems and services we want to know more about and consider in the larger context of the operating environment
 - Architectural notions of the various parts of the Polar architecture, which include:
   - policy agents
   - information processors
   - resource observers
   - resource observer supervisors

There are also some implied support infrastructures, such as orchestration, authentication and authorization (A&A) services, and certificate management (PKI) infrastructures. These are very real and necessary items, but are beyond the bounds of the architecture and its concerns, with the exception of certificate management/A&A. At this time, Polar is in a research state and makes no prescription about how to handle certificates, distribute them, and how to interact with other security mechanisms. It is however, assumed that Polar will eventually need these mechanisms, as the default model for deployment of Polar is expected to use Zero Trust principles. For example, all communications pathways utilize mutual TLS and systems create well-defined "circuits" of communications pathways that can be enforced at the networking layer, using VLANs, routers, firewalls, etc.

This write-up will focus in on the architectural notions of Observers, Supervisors, Information Processors, and Policy Agents, and how those can be realized, using an Actor framework for distributed and concurrent computation.

## Agents

Polar Agents are deployed as a triple:
 - Policy Agent
 - Information Processor Agent
 - Observer Agent

These three Agents work together to provide the necessary functionality to set Observer Policy, Observe some resource, and Process Information into a form suitable for the knowledge graph. It is envisioned there will be other types of Agents in the future. For example, we may eventually have Agents that do post-processing of data, once it is in the knowledge graph. These Agents may create additional nodes, perform clean-up routines on the graph, etc. These Agents may even employ AI to provide a higher-level query interface for users.

## Messaging

We will start with a focus on messages sent and received by each major participant in the architecture.

Users: commit policy documents for a given observer, to a git repo. Policy documents contain:
 - Scheduling policy (How frequently observations shall occur)
 - Configuration policy (What shall be observed)

*** It is envisioned to have a one-to-one mapping of agents by resource

Policy Agents: Detect policy changes by listening on a specific git repo.
Receives:
 - Configuration Request messages
Sends:
 - Configuration Policy Event
 - Schedule Policy Event

Information Processors: Receive Observations and update the knowledge graph
Receives:
 - Observations

Resource Observer Supervisor: Manages a tree of Actors and their lifecycles.
Receives:
 - Observer Schedules
Sends (via Actor runtime message):
 - New schedule for existing Actor, lifecycle event to start or stop/start, an Actor with the new schedule

Resource Observers: Observe some resource and report their observations
Receives:
 - (via Actor runtime message) schedule and lifecycle events
 - (via broker message) Configuration Policy Events
Sends (via broker message):
 - Observation events
 - Configuration Policy request events

 The decision to have Observers communicate both via Actor messages and broker messages is one that I go back-and-forth with. We could have all broker messages that necessarily originated from the Observer or are destined for the Observer, pass through the Supervisor first. This removes a protocol that the Observers need to understand/handle, but forces the supervisor to handle messages it doesn't care about, when really the supervisor can maintain a focus on managing lifecycle events for the Observers, which includes managing their schedule of observations. Configuration events need only be processed/requested by the Observers. One way of fixing this would be to have the Observer implement an Actor, specifically for processing broker messages and passing through requests/responses to/from broker messages, as Actor messages that get placed in the Mailbox for a given Actor (the actual observer). This would isolate the protocol handling of the broker to a special purpose Actor, allowing the other Actors making up the Observer to only handle Actor messages.

## Observers

### Actor Lifecycle Management

 The Supervisor is responsible for managing the lifecycle events of all other Actors in Rank 1 that make up the Observer. The first Actor the Supervisor will start up will be the BrokerComms Actor, which will handle all message broker activity, converting broker messages into Actor messages and routing messages between various Actor mailboxes.

 The next Actors that the Supervisor will start will be the Observers, responsible for making Observations. Observers will spin up ephemeral Actors to handle their processing needs. These ephemeral Actors are represented as rank 2 within the Observer Actor:
  - Protocol Handlers: Make the actual protocol request to an Observable, using whatever protocol is appropriate for this task.
  - Transform Handlers: Make any transforms necessary to make the Protocol Result useful for the Observer. There may be any number of transform handlers for an Observer.

Observers will compile the results of all Protocol Handlers and Transform Handlers, then send an appropriate Observation message to the BrokerComms Actor. The BrokerComms Actor will handle this message and send the Observation to the message broker.

*** State ***

Observers are intended to be stateless. I.e., we don't care if they lose data. If they lose anything, it's at most a fraction of an Observation and the Supervisor may simply restart the Observer.

## Information Processors

