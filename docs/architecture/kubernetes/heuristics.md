
In order to provide a proper historical snapshot (or attempt to) of a kubernetes deployment or replicaset, we need to reason about the question "When is a pod being replaced, rather than deleted?"

A user could put the same pod definition in a whole new namespace, with different names, and volume configurations but still use the same container image. Or they could just be updating the deployment to use a new container image and change nothing else.
To start, we to take a look at aspects of the kubernetes data model.

## **Controller/Owner References**

Most pods are owned by a higher-level controller (Deployment, StatefulSet, DaemonSet, Job, etc.). In that case:

* **Owner UID**: Pods carry an `ownerReference.uid` that ties them to the ReplicaSet or StatefulSet. The `k8s_openapi` crate exposes this as part of the `OwnerReference` struct.
* **Template hash label**: Deployments stamp pods with a label like `pod-template-hash` matching the ReplicaSet.

**Proposed Heuristic**:

> If a deleted pod and a newly created pod share the *same* owner UID (or the same `pod-template-hash`) and occur within a short time window (say, 1–2 minutes), we can potentially treat the new pod as a replacement—*even if* the name changed or the image tag changed.

This is by far the strongest signal. In almost every rolling-update scenario, the old and new pods both belong to the same ReplicaSet/Deployment, so you can confidently link them.

---

## 2. **GenerateName / Naming Prefix**

For pods created directly (not via a controller), Kubernetes often uses `metadata.generateName`, which is a stable prefix:

```yaml
metadata:
  generateName: myapp-
```

so you get pods like `myapp-abc123`, then `myapp-def456`.

**Proposed Heuristic**:

> If two pods in the *same namespace* share the same `generateName` and one appears within N seconds of the other’s deletion, consider that a replacement.

This catches ad-hoc pods that aren’t controller-managed but still share a prefix.

---

## 3. **Exact Pod Spec Matching**

You can go even deeper and compare the *spec* of the deleted pod vs the new one:

* **Volumes**: Are the same PVC mounts present?
* **ServiceAccount**: Identical?
* **Labels/Annotations**: Identical or overlapping?

**Potential Heuristic**:

> If the new pod’s entire non-dynamic spec (volumes, envFrom, serviceAccount, labels) matches the old one (except for image tag), that’s a strong hint of replacement.

We might choose to only apply this rule when we can’t match on ownerRefs or generateName.

---

## 4. **Time Window**

In all of these, we need a **time bound** so we don’t accidentally link a pod deleted today with one created tomorrow. A reasonable default is:

> Only consider pods as “replacement pairs” if the new pod appears within **60–120 seconds** of the old pod’s deletion. Whether the new pod becomes "ready" may be irrelevant to our desire to track state.
---

## Putting It All Together

Our logic could result in a function that, given a deleted‐pod record and subsequently observed pods, scores “replacement likelihood”:

1. **Match owner uid?** +10
2. **Same `pod-template-hash`?** +8
3. **Same `generateName`?** +5
4. **Matching spec fingerprint?** +3
5. **Within time window?** ×2

If the total score crosses a threshold (say 10), treat it as a replacement—otherwise treat it as a standalone create or delete.

---

## Fallback: Treat Everything as Independent

If none of our heuristics match, we can fall back to:

* Deletion → `USED_TAG` the old pod
* Creation → `USES_TAG` for the new pod   
  (no linking between them)
