# Cassini Test Control Plane

**TODO:** An executable control plane that will automatically start services to test the cassini broker.
Ideallty, this could auto-provision the necessary infrastructure within desired environments, such as k8s clusters, on-prem bare metal, cloud, or just local svcs.

## Implementation notes

## 🧩 The Goal

We want:

* To **coordinate** multiple independent executables (`broker`, `source`, `sink`)
* To **synchronize start/stop** phases for repeatable runs
* To **collect metrics and traces** (latency, throughput, errors)
* Without needing to fundamentally change how each binary works

---

## 🧠 1. Define a Tiny Control Protocol

We already have a control protocol for comms between the source and sink.

---

## The harness becomes the conductor

This orchestration logic is easy to extend with:

* Per-run metadata
* JSON metrics aggregation
* Conditional tracing collection
* Repetition for performance regression testing

---

## 📊 4. Metrics Collection

Let each service write metrics to a shared directory or push them over the control channel, e.g.:

```json
{
  "messages_sent": 10000,
  "messages_received": 9999,
  "avg_latency_ms": 1.2,
  "max_latency_ms": 5.4
}
```

The harness can gather those into a single `results.json` per run.

You can even plug that into Criterion or Grafana later.

---

## 🧠 5. Optional: extend to distributed mode

If you ever need to run these across different machines:

* Just make the control plane listen on the LAN interface instead of loopback.
* Your harness can then coordinate remote runs (via SSH + HTTP).
* Same protocol, no code change.
