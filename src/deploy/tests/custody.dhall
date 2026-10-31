{-
  tests/custody.dhall — what the library renders for a pod with the custody daemon, and what it refuses.

    dhall type --quiet --file src/deploy/tests/custody.dhall

  Every `assert` is checked when this file is type-checked; there is nothing to run. The rules are those of the
  fork's catalog (ACE docs/architecture/key-custody.md, "Step 4", and infra/rules.nu).
-}
let Builder = ../types/builder.dhall

let kubernetes = ../types/kubernetes.dhall

let Prelude =
      https://prelude.dhall-lang.org/package.dhall
        sha256:931cbfae9d746c4611b07633ab1e547637ab4ba138b16bf65ef1b9ad66a60b7f

let base =
      Builder.Platform::{
      , cassiniAddr = "cassini:8080"
      , cassiniServerName = "cassini.polar"
      , issuer = Builder.Issuer.CredentialsAgent "http://observer:8080"
      , polarInitImage = "polar-init:latest"
      }

let withCustody =
      base // { custodyImage = Some "polar-custody:latest", pinnedCa = Some "polar-ca" }

let agent = Builder.BuilderAgent::{ name = "a", namespace = "n", image = "a:latest" }

let names =
      λ(vars : List kubernetes.EnvVar.Type) →
        Prelude.List.map
          kubernetes.EnvVar.Type
          Text
          (λ(e : kubernetes.EnvVar.Type) → e.name)
          vars

let initNames =
      λ(r : Builder.PolarAgentResource) →
        Prelude.List.map
          kubernetes.Container.Type
          Text
          (λ(c : kubernetes.Container.Type) → c.name)
          r.spec.pod.initContainers

-- polar-init is the last init container; the daemon, when there is one, is before it.
let lastInit =
      λ(r : Builder.PolarAgentResource) →
        Prelude.List.last kubernetes.Container.Type r.spec.pod.initContainers

let envNamesOf =
      λ(r : Builder.PolarAgentResource) →
        merge
          { Some =
              λ(c : kubernetes.Container.Type) →
                names
                  ( Prelude.Optional.default
                      (List kubernetes.EnvVar.Type)
                      ([] : List kubernetes.EnvVar.Type)
                      c.env
                  )
          , None = [] : List Text
          }
          (lastInit r)

let argsOf =
      λ(r : Builder.PolarAgentResource) →
        merge
          { Some =
              λ(c : kubernetes.Container.Type) →
                Prelude.Optional.default (List Text) ([] : List Text) c.args
          , None = [] : List Text
          }
          (lastInit r)

let healthNames = [ "POLAR_HEALTH_FILE", "POLAR_HEALTH_CERTS", "POLAR_HEALTH_DEP_ENDPOINTS" ]

-- ── No daemon: polar-init alone, and the key is a file ──────────────────────────────────────────────
let off = Builder.makePolarAgent base agent

let offContainers = assert : initNames off ≡ [ "polar-init" ]

let offArgs =
      assert :
          argsOf off
        ≡ [ "--check-dep-cert"
          , "cassini:8080:300"
          , "--cert"
          , "client:/home/polar/certs:ecdsa-p256"
          ]

let offWorkloadEnv =
      assert :
          names off.spec.pod.env
        ≡   [ "TLS_CA_CERT"
            , "TLS_CLIENT_CERT"
            , "TLS_CLIENT_KEY"
            , "BROKER_ADDR"
            , "CASSINI_SERVER_NAME"
            ]
          # healthNames
          # [ "POLAR_CAPABILITIES" ]

-- ── The daemon: a native sidecar before polar-init, the key is its, and the pod says where to sign ────────
let on = Builder.makePolarAgent withCustody agent

let onContainers = assert : initNames on ≡ [ "polar-custody", "polar-init" ]

let onArgs =
      assert :
          argsOf on
        ≡ [ "--check-dep-cert"
          , "cassini:8080:300"
          , "--cert"
          , "client:/home/polar/certs:custody"
          ]

let onInitEnv =
      assert :
          envNamesOf on
        ≡ [ "POLAR_INIT_ISSUER"
          , "POLAR_INIT_OBSERVER_URL"
          , "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY"
          , "POLAR_INIT_TOKEN_PATH"
          , "POLAR_INIT_CA_CERT"
          , "POLAR_CUSTODY_SOCKET"
          ]

let onWorkloadEnv =
      assert :
          names on.spec.pod.env
        ≡   [ "TLS_CA_CERT"
            , "TLS_CLIENT_CERT"
            , "BROKER_ADDR"
            , "CASSINI_SERVER_NAME"
            , "POLAR_CUSTODY_SOCKET"
            ]
          # healthNames
          # [ "POLAR_CAPABILITIES" ]

-- The sidecar is told its socket, the CA it judges certificates by, who it serves, and where the broker is.
let sidecarEnv =
      assert :
          names
            ( Prelude.Optional.default
                (List kubernetes.EnvVar.Type)
                ([] : List kubernetes.EnvVar.Type)
                (Builder.custodySidecar withCustody agent).env
            )
        ≡ [ "POLAR_CUSTODY_SOCKET"
          , "POLAR_CUSTODY_CA_CERT"
          , "POLAR_CUSTODY_INSTALLER_UID"
          , "POLAR_CUSTODY_WORKLOAD_UIDS"
          , "BROKER_ADDR"
          , "CASSINI_SERVER_NAME"
          ]

-- An agent that encrypts asks polar-init for its escrow key, and nothing else changes.
let encrypting = Builder.makePolarAgent withCustody (agent // { encrypts = True })

let encryptingEnv =
      assert :
          envNamesOf encrypting
        ≡ [ "POLAR_INIT_ISSUER"
          , "POLAR_INIT_OBSERVER_URL"
          , "POLAR_INIT_CREDENTIALS_AGENT_IDENTITY"
          , "POLAR_INIT_TOKEN_PATH"
          , "POLAR_INIT_CA_CERT"
          , "POLAR_CUSTODY_SOCKET"
          , "POLAR_INIT_ESCROW_BOOTSTRAP"
          ]

-- A server's certificate is the daemon's too, and a server that is not a client has no client variables.
let server =
      Builder.makePolarAgent
        withCustody
        ( agent
          // { cert = Builder.CertKind.Server "a.polar.svc"
             , client = False
             , waitForBroker = False
             }
        )

let serverArgs =
      assert : argsOf server ≡ [ "--cert", "server:/home/polar/certs:custody:a.polar.svc" ]

let serverEnv =
      assert :
          names server.spec.pod.env
        ≡ [ "POLAR_CUSTODY_SOCKET", "POLAR_HEALTH_FILE", "POLAR_HEALTH_CERTS", "POLAR_CAPABILITIES" ]

-- The deployment says what it provides, as the agent's builder wants it: the capabilities, comma-joined, last of the
-- workload's variables.
let capabilitiesAreProvided =
      assert :
          Prelude.Optional.concat
            Text
            ( Prelude.Optional.map
                kubernetes.EnvVar.Type
                (Optional Text)
                (λ(e : kubernetes.EnvVar.Type) → e.value)
                ( Prelude.List.last
                    kubernetes.EnvVar.Type
                    ( Builder.makePolarAgent
                        withCustody
                        (agent // { provides = [ "cassini", "graph-write" ] })
                      ).spec.pod.env
                )
            )
        ≡ Some "cassini,graph-write"

-- An agent that cannot use the daemon says so, and is as it was.
let optedOut = Builder.makePolarAgent withCustody (agent // { custody = False })

let optedOutContainers = assert : initNames optedOut ≡ [ "polar-init" ]

let optedOutArgs =
      assert :
          argsOf optedOut
        ≡ [ "--check-dep-cert"
          , "cassini:8080:300"
          , "--cert"
          , "client:/home/polar/certs:ecdsa-p256"
          ]

-- ── The rules ───────────────────────────────────────────────────────────────────────────────────────
let fine = assert : Builder.problems withCustody agent ≡ ([] : List Text)

let fineEncrypting =
      assert : Builder.problems withCustody (agent // { encrypts = True }) ≡ ([] : List Text)

-- It encrypts and has no daemon (and the platform has no pinned CA, which its issuer path needs): two reasons.
let encryptsWithoutDaemon =
      assert :
          Prelude.List.length Text (Builder.problems base (agent // { encrypts = True }))
        ≡ 2

-- With the CA but no daemon, one: the escrow key.
let encryptsWithoutDaemonWithCa =
      assert :
          Prelude.List.length
            Text
            ( Builder.problems
                (base // { pinnedCa = Some "polar-ca" })
                (agent // { encrypts = True })
            )
        ≡ 1

-- A daemon needs the CA it judges by, and the credentials agent's path needs it of polar-init.
let daemonWithoutCa =
      assert :
          Prelude.List.length
            Text
            (Builder.problems (base // { custodyImage = Some "polar-custody:latest" }) agent)
        ≡ 2

in  {=}
