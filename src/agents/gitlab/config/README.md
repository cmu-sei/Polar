# Polar Configuration
Polar observers will be configured by a core dhall confuguration.

Why?

In essence, we want the agents to be incredibly "dumb" and only doing what they're told through messages from supervisors.

We achieve several strategic advantages by pushing *all* dynamic behavior through supervisor messages and using Dhall for **declarative, static config**. This way, we cut agents off from the environment.

> We guarantee they are **passive, observable, and fully deterministic** actors — powered only by version controlled configuration that will later be leveraged by the policy agent.

---

### **Benefits of “Dumb” Agents with Static Config**

#### 1. **Predictability**

* Agents behave identically every time they start.
* No surprises from runtime environment differences.
* Debugging becomes simpler — you can reproduce issues with just the config file and message logs.

#### 2. **Centralized Control**

* Supervisors control timing, retries, and backoff.
* All dynamic behavior (e.g. "pause", "stop", "backoff", "change interval") flows through explicit messages.
* Makes agents composable and testable as pure state machines.

#### 3. **Reproducibility and Auditing**

* Dhall config is version-controlled and type-safe.
* Supervisors can be driven by test cases or audit logs to replay or simulate behaviors.

#### 4. **Security and Isolation**

* No agent has access to environment variables like tokens or keys unless explicitly provided by the supervisor.
* You can sandbox agents more aggressively (e.g. no network access, read-only FS).

#### 5. **Testing and Simulation**

* Since the agents don’t rely on external env state, you can spin them up with mock configs and validate their response purely through message passing.

