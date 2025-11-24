## 🔹 Abstract Control Plane Architecture

### 1. **Roles**

| Component            | Responsibility                                                                                                               |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| **Control Plane**    | Orchestrates the test: reads Dhall plan, starts broker, sink, producer, distributes scenarios, collects metrics and tracing. |
| **Broker**           | Core message broker: handles registrations, subscriptions, publishes messages, etc.                                          |
| **Producer Harness** | Acts as a client that publishes messages according to the test plan.                                                         |
| **Sink Harness**     | Acts as a client that subscribes to topics, receives messages, and reports metrics.                                          |

---

### 2. **Flow of a Test**

1. **Initialization**

   * Control plane reads the test plan (Dhall).
   * Control plane launches:

     * Broker process
     * Producer process
     * Sink process
   * Control plane exposes a **secure control API** (HTTP/GRPC/Unix sockets) for producer and sink to connect to.

2. **Registration**

   * Producer and sink connect to control plane.
   * Each harness registers itself with the control plane:

     * Assign client/session ID
     * Authenticate/authorize connection (TLS or token-based)
   * Control plane acknowledges readiness.

3. **Test Plan Distribution**

   * Control plane serializes the test plan (from Dhall) and sends it over the control channel to producer and sink.
   * Test plan may include:

     * Number of messages to publish
     * Topics
     * Timing or rate limits
     * Special scenarios (like “register-then-die” or “burst publish”)

4. **Test Execution**

   * Control plane signals start.
   * Producer and sink execute their parts according to the plan:

     * Producer publishes messages.
     * Sink subscribes and collects received messages.
   * Control plane can monitor progress, request interim metrics, or enforce timeouts.

5. **Tracing and Metrics Collection**

   * Each component (broker, producer, sink) emits tracing spans via `tracing` (or `opentelemetry`), sending to a centralized collector (Jaeger/OTLP).
   * Control plane can also collect custom metrics over its secure channel:

     * Messages sent/received
     * Latencies
     * Failures
   * Optionally, spans can be correlated across the distributed system using session IDs or message IDs.

6. **Completion**

   * Control plane signals end-of-test.
   * Producer and sink teardown.
   * Broker shuts down gracefully.
   * Control plane gathers final metrics and tracing data for analysis.

---

### 3. **Key Design Principles**

* **Separation of Concerns**

  * Harnesses are **dumb executors**: they follow the plan but don’t read Dhall themselves.
  * Control plane is **the source of truth**: it reads config, starts components, and monitors.

* **Secure, bidirectional control channel**

  * Needed for:

    * Harness registration
    * Scenario commands
    * Metrics / heartbeat reporting

* **Flexible scenario modeling**

  * Dhall test plan → serialized JSON or protobuf → transmitted to producer/sink.
  * Allows “just register then die” and full-blown stress tests with the same mechanism.

* **Observability-first**

  * All components emit spans/metrics.
  * Correlation IDs used to tie distributed traces together.
  * Eventually upload to Jaeger for visualization and analysis.

* **Repeatable & deterministic**

  * Control plane can orchestrate multiple runs with different scenarios.
  * Useful for CI/CD, benchmarking, or regression tests.

---

### 4. **High-Level Diagram (Abstract)**

```
        +-----------------------+
        |    Control Plane      |
        |  (reads Dhall plan)  |
        +----------+------------+
                   |
                   | Secure control channel
                   v
        +----------+------------+
        | Producer Harness      |
        | Executes test plan    |
        +----------+------------+
                   ^
                   |
                   v
        +----------+------------+
        | Sink Harness          |
        | Executes test plan    |
        +----------+------------+
                   ^
                   |
                   v
              +----+----+
              | Broker  |
              +---------+
                   |
                   v
             Jaeger/OTLP Collector
```

---

### 5. **Why This Approach Makes Sense**

* Decouples test orchestration from the harnesses themselves.
* Makes it easy to run **simple integration tests** (“register then die”) or **complex benchmarks** (high-volume publish/subscribe).
* Provides a natural path to **distributed tracing**, centralized metrics, and eventual dashboarding.
* Future-proof: you can add more harness types, scenarios, or observability sinks without touching the core broker logic.
