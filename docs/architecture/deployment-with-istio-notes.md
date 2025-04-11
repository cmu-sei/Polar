Here’s a clear, actionable summary plan to achieve your desired setup using Istio without sidecars, while enforcing strong mTLS, route control, 
and network isolation. I’ll break it down into 4 parts, with specifics for annotations, certs, routes, and policies.

1. Let Your Observer Services Call Mesh Services
🔧 Setup:

    Do not inject sidecars into your observer pods:
```yaml
    metadata:
      annotations:
        sidecar.istio.io/inject: "false"
```
    Expose mesh service endpoints you want to observe via Istio Gateway or ClusterIP + ServiceEntry.

Option A: via Ingress Gateway (for services exposed outside mesh)

Define a Gateway (HTTPS)
```yaml
apiVersion: networking.istio.io/v1beta1
kind: Gateway
metadata:
  name: neo4j-gateway
  namespace: neo4j
spec:
  selector:
    istio: ingressgateway  # This selects the default Istio ingress gateway
  servers:
    - port:
        number: 443
        name: https
        protocol: HTTPS
      tls:
        mode: SIMPLE  # Or MUTUAL if you want to require client certs
        credentialName: neo4j-tls  # Reference to your TLS cert
      hosts:
        - "neo4j.example.com"
```

VirtualService to route to mesh services

Use mode SIMPLE or MUTUAL in the Gateway

Trust Istio Services
Extract Istio root CA:

kubectl get configmap istio-ca-root-cert -n istio-system -o jsonpath='{.data.root-cert\.pem}' > istio-root.pem

Add istio-root.pem to the observers' TLS trust store.

Optionally, we can validate SPIFFE identity in the mesh service’s certificate (spiffe://cluster.local/ns/…). Not sure we'll need to do this.
    
2. Expose Your Graph Database to External Clients via Istio Gateway for querying and usage

🔧 Setup:

**Do not inject sidecar into your graph DB pod.**
Add annotation to the dhall configuration to get this output.
```yaml
metadata:
    annotations:
    sidecar.istio.io/inject: "false"
```
* Create an Istio Gateway that exposes your graph DB’s port:
* Create a VirtualService to route to the graph DB pod:

```yaml
apiVersion: networking.istio.io/v1beta1
kind: VirtualService
metadata:
name: graph-db-vs
spec:
hosts:
    - "graphdb.example.com"
gateways:
    - graph-db-gateway
http:
    - match:
        - uri:
            prefix: /
    route:
        - destination:
            host: graph-db.default.svc.cluster.local
            port:
            number: 7687
```
Mount TLS certs for Gateway into a Kubernetes TLS secret named graph-db-cert.

3. Restrict Graph DB Access to Only Your Internal Services
Goals:
* Allow only our non-meshed polar services to call your graph DB internally.
* Deny mesh services from reaching it.

Setup:
* Run graph DB in its own namespace (e.g., graph-system).
* Apply an Istio AuthorizationPolicy that only allows your services’ service accounts

```yaml 
apiVersion: security.istio.io/v1beta1
kind: AuthorizationPolicy
metadata:
name: graph-db-allow-observers
namespace: graph-system
spec:
selector:
    matchLabels:
    app: graph-db
action: ALLOW
rules:
    - from:
        - source:
            principals:
            - cluster.local/ns/your-nonmesh-ns/sa/your-observer-sa
    - from:
        - source:
            notRequestPrincipals: ["*"]  # allows non-mTLS (your services not using Istio SDS)
```
* Apply PeerAuthentication to accept plain TLS or mTLS from your services:
```yaml

apiVersion: security.istio.io/v1beta1
kind: PeerAuthentication
metadata:
name: allow-non-mesh
namespace: graph-system
spec:
selector:
    matchLabels:
    app: graph-db
mtls:
    mode: PERMISSIVE
```

This accepts both mesh and non-mesh traffic, but only non-mesh or specific SAs are allowed by the AuthorizationPolicy.

4. Prevent Your Services from Reaching the Mesh (Except Observers)
Setup:

Add this to all non-observer workloads:
```yaml
metadata:
    annotations:
    sidecar.istio.io/inject: "false"
```
Deny mesh egress by default in mesh policy:
```yaml
apiVersion: security.istio.io/v1beta1
kind: AuthorizationPolicy
metadata:
name: deny-to-mesh
namespace: default  # or mesh-wide in istio-system
spec:
selector:
    matchLabels:
    app: your-non-observer-app
action: DENY
rules:
    - to:
        - operation:
            ports: ["80", "443"]
```
Only observer services get egress access:
```yaml 
apiVersion: security.istio.io/v1beta1
kind: AuthorizationPolicy
metadata:
name: allow-observer-to-mesh
namespace: observer-ns
spec:
selector:
    matchLabels:
    app: observer
action: ALLOW
rules:
    - to:
        - operation:
            ports: ["443"]
```

📌 Final Notes
Component	Action
All your pods	sidecar.istio.io/inject: "false"
Observers	Mount Istio CA as trusted root for TLS validation
Mesh services	Exposed via Gateway or ClusterIP
Graph DB	Exposed via Gateway for outside, restricted by policy inside
Internal access to DB	Controlled by AuthorizationPolicy + PeerAuthentication
Default deny to mesh	Prevent your services from accessing mesh unless they're observers


 the external istio gateway should expose port 7474 to our neo4j instance, but the bolt port should be off limits.
ChatGPT said:

Here’s exactly how to do that.

1. Neo4j Deployment: Disable Sidecar Injection

Ensure your Neo4j pod has no Istio sidecar:
```yaml
metadata:
  annotations:
    sidecar.istio.io/inject: "false"
``` 

2. Istio Gateway: Expose Only Port 7473 (HTTPS API)

```yaml 
apiVersion: networking.istio.io/v1beta1
kind: Gateway
metadata:
  name: neo4j-gateway
  namespace: neo4j
spec:
  selector:
    istio: ingressgateway  # This selects the default Istio ingress gateway
  servers:
    - port:
        number: 7473
        name: https
        protocol: HTTPS
      tls:
        mode: SIMPLE  # Or MUTUAL if you want to require client certs
        credentialName: neo4j-tls  # Reference to your TLS cert
      hosts:
        - "neo4j.example.com"
```
✅ 3. VirtualService: Route Only to Port 7474

```yaml apiVersion: networking.istio.io/v1beta1
kind: VirtualService
metadata:
  name: neo4j-vs
  namespace: neo4j
spec:
  hosts:
    - "neo4j.example.com"
  gateways:
    - neo4j-gateway
  http:
    - match:
        - uri:
            prefix: /  # matches all paths
      route:
        - destination:
            host: neo4j.neo4j.svc.cluster.local
            port:
              number: 7473
```
✅ This only exposes port 7473, so external users can hit the HTTPS API (https://neo4j.example.com) but can’t reach Bolt (7687) because it’s not routed at all.
✅ 4. Restrict Internal Access to Bolt (7687)

To protect the Bolt port inside the cluster:
A. Allow access only from your services
```yaml
apiVersion: security.istio.io/v1beta1
kind: AuthorizationPolicy
metadata:
  name: allow-observers-to-bolt
  namespace: neo4j
spec:
  selector:
    matchLabels:
      app: neo4j
  action: ALLOW
  rules:
    - from:
        - source:
            principals:
              - cluster.local/ns/your-nonmesh-ns/sa/observer-sa
    - from:
        - source:
            notRequestPrincipals: ["*"]  # Allow non-mTLS (for your non-mesh clients)
      to:
        - operation:
            ports: ["7687"]
```
If you're not using Istio mTLS inside your observer services, the notRequestPrincipals: ["*"] rule permits non-Istio traffic.

B. Deny everything else (for safety)
```yaml 
apiVersion: security.istio.io/v1beta1
kind: AuthorizationPolicy
metadata:
  name: deny-all-neo4j
  namespace: neo4j
spec:
  selector:
    matchLabels:
      app: neo4j
  action: DENY
```

💡 Istio evaluates DENY before ALLOW, so this works like a default-drop firewall rule.

If you want extra enforcement at the network layer, apply a NetworkPolicy to only allow 7687 from observer pods.
🔒 TL;DR Summary
Feature	Action
Neo4j HTTP exposed externally	VirtualService routing only to port 7474 via Gateway
Neo4j Bolt port (7687) externally	Not exposed at all via Istio
Internal access to Bolt	AuthorizationPolicy limits access to observer service account only
Unwanted access to Neo4j	Denied by AuthorizationPolicy at the service level
Sidecars	Not injected in Neo4j or your observer pods