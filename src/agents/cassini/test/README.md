# Cassini Test Harness

A flexible test harness for exercising our message broker under a variety of workloads. The harness provides reusable actors that simulate producers and consumers, enabling us to test throughput, durability, and correctness at scale.

Unlike one-off integration tests, the harness is designed for **repeatable performance experiments** and **adversarial scenarios** that go beyond correctness checks.


---

## Why?

Static Integration tests only cover correctness of broker operations (topic creation, deletion, publish/subscribe semantics). The harness extends this by providing:

* **Parameterized message generation** – Control message size, type, rate, and payload entropy.
* **Supervised producer/consumer actors** – Each client runs under its own supervisor, enabling fault injection and lifecycle management.
* **Metrics collection** – Measure throughput, latency, message loss, and errors.
* **Scenario composition** – Run long-lived stress tests, spike workloads, or topic churn scenarios.
* **Reusability** – Producers and consumers are just actors; they can be combined in different topologies without rewriting logic.

---

## Concepts

* **Producer Supervisor(Source)**

  * Configurable rate (msgs/sec) and duration.
  * Payloads may be fixed or random (text, numbers, enums).
  * Logs metrics: sent count, errors, time elapsed.

* **Consumer Supervisor (Sink)**

  * Subscribes to a topic.
  * Collects received messages and verifies integrity (sequence/checksum optional).
  * Logs metrics: received count, loss rate, time elapsed.

* **Metrics**

  * Collected in-memory and queriable via messages.
  * Useful for CI/CD pipelines, regression tests, and dashboards.

---

## Usage

The sink and producer services are two distinct components of the harness. Both are also configured to leverage mTLS and so must be configured with SSL certificates. See the example.env file at the root of this project for further details.


---

## Scenarios

The harness is intended to simulate a wide range of workloads, including:

* **Throughput tests** – small messages at high rates.
* **Batch tests** – fewer, larger payloads.
* **Topic churn** – rapid create/delete of topics while publishing.
* **Mixed workloads** – producers and consumers running simultaneously on multiple topics.
