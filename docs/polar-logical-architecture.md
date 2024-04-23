# Polar Information Update Model

## Color Key Supplement to the Diagram
* Green - Information Endpoints
* Blue - Polar Service Components
* Grey - Supporting Infrastructure

## Elaboration of Architecture Components

### Graph Database
The graph database is the user-facing endpoint for information collected from
network resource, by Polar. The current graph database implementation uses
Neo4J. The system is architected in a manner to support other graph databases,
such as ArangoDB, with minimal code/configuration changes. The graph is the
central collection point for all information about the network.

### Resource
Resource is an abstract way of describing anything being observed by Polar. For
instance, Polar can currently observe AWS cloud resources, such as EC2, S3,
VPC, etc. It's capable of collecting Observables on anything that is currently
supported by the AWS SDK. Our decision to start with enumeration of cloud
resources is not a limitation of the architecture. The goal is to be able to
enumerate information on any Observable Resource and send updates to the
information model.

### Polar Service Components
Resource Observer and Resource Observer Consumer are the primary abstractions
of the architecture. They represent individual components for collecting
information on a specific Resource and transforming the collected information
into graph data, respectively.

A Resource Observer can take many forms, all based on the method required for
introspection of the Resource. Once information has been collected, the
Resource Observer is responsible for raising an Event that contains the
observation payload. A message broker facilitates the passing of events from
producer to consumer.

The Resource Consumer component is responsible for receiving events from its
corresponding Resource Observer and transforming the payload into queries that
will load the new data into the graph. The initial updates will create new
nodes, where future updates occurring on a specified interval will update an
existing node that must first be queried and updated.

There are a number of reasons for separating the Resource Observer from the
Resource Observer Consumer, addressing a number of non-functional attributes of
the overall system. For example, Resource Observers may need to exist outside
of a security boundary, close to an edge service. This component needs to be
small, hardened against attacks, and have limited access to data, such as the
large repository of data being compiled in the graph.

The Message Broker needs to be reachable from both sides of any given security
boundary. However, the attack surface can be limited by controlling for which
systems are allowed to communicate with it, i.e., firewall rules. Additionally,
communications between components and the message broker shall be secured via
mutual TLS. Both the components and the Message Broker shall refuse to
downgrade the TLS connection from mTLS. The presence of the Message Broker
presents the possibility of additional message consumers that are not part of
the Polar Information Model. This can facilitate a number of use-cases that
need such data, but the design of further integration is out of scope.

The Resource Observer Supervisor is a component that is solely responsible for
scheduling Resource Observers and tasking them with any specific information
collection goals. Again, the goal is to keep components small, easily auditable
and easily hardened against attacks. Resource Observers will be managed by the
supervisor component using an Actor framework. As such, Resource Observers will
implement a messaging inbox and receive lifecycle directives and collection
directives from the supervisor.

The Configuration Service is an acknowledgment that over time, collection
goals for the various Resource Observers can change. The component is therefore
responsible for taking directive, based on detected user changes to a specific
version-controlled repository. This is a pattern that should be familiar to
anyone familiar with the GitOps workflow as a reconciliation loop. As file
updates are detected in the version-controlled repository, the Configuration
Service will process the changes and send schedule and collection update events
to the Message Broker.

These Events will be processed by the Resource Observer Supervisor, updating the
scheduling plan for a given Resource Observer. As with the Resource Observer
and Resource Observer Consumer components, these components are separated to
address a desired set of non-functional requirements. The Configuration Service
needs access to a version-controlled repository, which could (likely) be a
protected resource. The supervisor itself will need to exist in proximity to
Resource Observers. The separation allows us to control and minimize
touch-points to the Resource Observers, while allowing the supervisor
flexibility in how and when to communicate with observers.
