
## Quick critique (concrete, not hand-wavey)

2. **Controller spawns binaries but doesn’t manage them robustly.**
   You `tokio::spawn(Command::new(...).spawn()...)` and keep only an `AbortHandle` from the wrapper. You don’t keep the `Child` handle to inspect exit codes, restart, or stream logs. You need explicit process handles and restart policies.

3. **Mix of blocking and async APIs / unmanaged tasks.**
   There are spawned tasks that `.await` inside and also `std::thread::sleep` in the Tokio runtime. That’s sloppy and can cause surprising behavior. Keep everything async.

4. **No formal handshake / registration between controller and clients.**
   You accept connections, spawn a listener actor, and `cast` the TestPlan. But there’s no ACK/Ready or version check; the controller assumes the client will behave. Add versioning and negotiation.


6. **Message framing and rkyv usage is fragile.**
   You write length (to_be_bytes) and then serialized bytes — fine — but ensure both client and controller use the exact same length width (u64 vs u32) and endianness. Use helper functions for framing and tests for boundary cases.

7. **No metrics / heartbeat of spawned binaries.**
   You want metrics collection; currently you don’t capture stdout/stderr or expose Prom-style metrics from clients. You should expose a per-client metrics/health endpoint or a simple heartbeat message over the controller socket.

8. **Test plan delivery style is “fire-and-forget.”**
   Controller `cast`-s the plan to the client and keeps going. That’s fine for some tests but you should optionally support synchronous delivery (ack) and acceptance checking.

9. **Error handling and supervision events are incomplete.**
   `handle_supervisor_evt` has TODOs for `ActorFailed` to shut everything down gracefully — implement a policy (fail fast vs try to recover) and test it.

10. **Security/secret handling for TLS certs in CI** needs a plan. Don’t hardcode file paths; use injected secrets, ephemeral certs, or generate short-lived cert pairs for test runs.

---

## Immediate checklist — do these first (highest priority)


2. **Manage processes explicitly.**

   * Keep `tokio::process::Child` handles.
   * Capture stdout/stderr to files or streamed logs (for CI failure triage).
   * Record exit code and restart policy (no restart by default; test plan should decide).
   * Implement clean shutdown sequencing (SIGTERM -> graceful -> SIGKILL).

3. **Make client registration explicit.**

   * After accept + TLS handshake, have a small protocol message `REGISTER { client_type, version, pid }` from client to controller and `REGISTER_ACK { ok, id }` back. Only send TestPlan after registration ack.
   * The controller should track `client_id -> ActorRef/connection` and timeouts.

4. **Add timeouts everywhere.**

   * Socket read/write and any RPC (including reply ports you add) must use conservative timeouts. Tests should not hang.

5. **Promote logging and structured events.**

   * Emit structured JSON logs about lifecycle events (spawn, ready, crash, plan-accepted, metrics).
   * Capture child logs centrally.

6. **Return clear failures to orchestrator (CI).**

   * The controller should exit with a clear non-zero status when critical things fail (broker crashes, client fails to register, plan errors) or provide a machine-readable failure summary.

---

## Short-term checklist — make harness usable & robust

7. **Implement a small “control RPC” for the controller↔client:**

   * `REGISTER`, `REGISTER_ACK`, `PLAN`, `PLAN_ACK`, `HEARTBEAT`, `METRICS_PUSH`, `SHUTDOWN`.
   * Use rkyv (fast) but wrap it with explicit version tags so you can evolve the protocol.

8. **Instrument metrics and health:**

   * Have each client and the controller expose a Prom metrics endpoint (or push metrics via the control channel).
   * Controller aggregates per-run metrics in memory and exposes a run summary.

9. **Test plan semantics & validation:**

   * Validate the Dhall plan on load (schema-check).
   * Split plan into: lifecycle (start/stop), behavior (rate, payload), fault-injection schedule, assertions (expected delivered counts, latencies).

10. **Improve the ClientSession framing helper:**

* Centralize framing encode/decode helpers and test them.
* Use a consistent header width (u32 or u64) and assert on extreme sizes.

11. **Add graceful shutdown sequence:**

* Controller must be able to signal `SHUTDOWN` to clients and `SIGTERM` to processes and wait for cleanup.

12. **Capture artifacts for CI:**

* Save logs, metrics, and test summaries to a known artifacts directory.

---

## Medium-term checklist — features that make the harness powerful

13. **Containerized orchestration for clients & broker:**

* Use Testcontainers (Rust) or Docker Compose for local CI; CI systems can run Docker images. This allows you to run the same harness against container images.
* Provide canonical images for broker, producer, sink that accept the control-plane TLS cert and test-plan via env or mounted files.

14. **Fault injection primitives:**

* Controller APIs to kill a client process, pause it (tc netem), or inject latency/drops (iptables/netem inside container).
* Allow scheduled fault injections described in the Dhall plan.

15. **Scenario replay & deterministic seeding:**

* Provide deterministic RNG seeds for message payloads and timelines so tests are reproducible.

16. **Replayable artifacts & metrics comparisons:**

* Save run metadata (commit, binary versions), metrics snapshots, and diffs so you can track regressions.

17. **Test harness API for remote/local:**

* Allow harness to start remote nodes (via SSH) or local containers so the same harness code can run CI and local dev.

---

## Code-level fixes I’d implement immediately in your code

* In `post_start`, replace:

  ```rust
  let broker = crate::spawn_broker(myself.clone(), state.broker_path.clone()).await;
  std::thread::sleep(Duration::from_secs(5));
  ```

  with:

  1. `let broker_child = spawn_child_process(state.broker_path.clone(), args...).await?;`
  2. `wait_for_broker_ready(&mut broker_child, Duration::from_secs(cfg.wait_timeout)).await?;`
  3. store both `Child` and a supervisory `AbortHandle`/task handle in state.

* Replace `tokio::spawn(Command::new(...).spawn()...)` with code that returns `Child` and sets up readers for stdout/stderr:

  ```rust
  let mut child = Command::new(path)
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .expect(...);
  let stdout = child.stdout.take().unwrap();
  tokio::spawn(stream_logs(stdout, "producer", run_id));
  state.producer_child = Some(child);
  ```

* Add a strongly-typed `HarnessControllerMessage::REGISTER` and require clients to send it upon connect before sending other messages. Only after `REGISTER_ACK` does the controller consider the session live and send the TestPlan.

* Replace raw `expect()`s on I/O and cert parsing with controlled errors and human-readable messages. In tests, fail early but gracefully.

* Centralize error handling and make `handle_supervisor_evt` implement a clear policy: on `ActorFailed` either shutdown or escalate and collect diagnostics.

---

## Testing and CI integration

* Add small invariant tests (the ones we discussed earlier) that drive the harness in-process (no containers) to exercise the protocol.
* Add a second CI job that runs containerized full-harness scenarios (longer, nightly or gated depending on duration).
* Have a “fast” mode for the harness that runs smoke scenarios for PRs (e.g., 1 producer, 1 consumer, 100 messages).

---

## Security + secrets

* Don’t store static certs in repo. Use ephemeral cert generation or CI secrets injection.
* Consider generating a CA per test-run (fast) and signing client/server certs dynamically. This avoids leaking secrets and keeps tests isolated.

---
