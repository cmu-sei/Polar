--| Configuration schema for the Polar OCI resolver.
--
--  This file is the *source of truth* for the shape of the resolver's
--  configuration. The YAML that the resolver actually reads at runtime is a
--  lowered, flattened projection of these types produced by `./render.dhall`.
--
--  The reason for the split: Dhall's sum types let us make invalid states
--  unrepresentable (you cannot specify a plaintext host list while also
--  claiming TLS-everywhere), but `dhall-to-yaml` erases union tags on
--  serialisation. So the union lives here, the discriminator is emitted
--  explicitly by the renderer, and the Rust loader reconstitutes the sum type
--  on parse. See `render.dhall` for the lowering.

--| How the resolver talks to a registry.
--
--  `HttpsExcept` entries are matched by *exact string equality* against the
--  value of `Reference::resolve_registry()` in `oci-client`, which is the
--  post-normalisation host, including the port. Two consequences that bite:
--
--    * Docker Hub must be spelled `index.docker.io`, not `docker.io`, because
--      `resolve_registry()` rewrites the latter to the former.
--    * A registry on a non-443 port must include the port, e.g.
--      `registry.local-registry.svc.cluster.local:5000`. Omitting it silently
--      does nothing.
--
--  `HttpInsecure` disables TLS for *every* registry, including Docker Hub. It
--  exists so that integration tests can run against a throwaway `registry:2`
--  without a PKI, and should never appear in a deployed configuration.
let Protocol =
      < Https
      | HttpsExcept : List Text
      | HttpInsecure
      >

--| What to prepend to an image reference that carries no registry component.
--
--  `namespace` is inserted only for single-segment repositories: `nginx`
--  becomes `<registry>/<namespace>/nginx`, while `rancher/klipper-helm` is left
--  with two segments and only gains the registry. This mirrors Docker's
--  `library/` rule, which is Docker-Hub-specific; if you point `registry` at a
--  Harbor or Artifactory instance you almost certainly want a different
--  namespace (Harbor's default project is also called `library`, which is a
--  coincidence, not a standard).
let DefaultRegistry =
      { registry : Text
      , namespace : Optional Text
      }

--| Policy for image references with no registry component, e.g.
--  `polar-nu-init:latest` or `rancher/klipper-helm:v0.9.14`.
--
--  `Skip` is the behaviour described in cmu-sei/Polar#219: short-circuit before
--  the network call and drop the event. It is honest about provenance — an
--  unqualified ref genuinely has no verifiable origin — but it is wrong as a
--  *default*, because the overwhelming majority of unqualified refs in the wild
--  are Docker Hub shorthand that every other OCI client resolves without
--  complaint. Reserve `Skip` for air-gapped clusters where a Docker Hub lookup
--  is guaranteed to fail and the resulting egress attempt is itself a finding.
--
--  `Qualify` assumes the configured registry and attempts resolution. If that
--  attempt fails, the resolver falls back to the `Skip` outcome for that event:
--  a warning, no emitted `OCIArtifactResolved`, no actor failure.
let UnqualifiedRefPolicy =
      < Skip
      | Qualify : DefaultRegistry
      >

--| A pull-through mirror, expressed as containerd does: requests for
--  `upstream` are sent to `mirror` with `?ns=<upstream>` appended.
--
--  WARNING: this maps onto `Reference::set_mirror_registry`, which upstream
--  marks `#[doc(hidden)]` and explicitly exempts from semver guarantees. Pin
--  `oci-client` exactly if you depend on this.
let Mirror =
      { upstream : Text
      , mirror : Text
      }

let Auth =
      { --| Directory containing `config.json`. `None` defers to
        --  `docker-credential`'s own search (`$DOCKER_CONFIG`, then
        --  `~/.docker`).
        dockerConfig : Optional Text
      , --| Fall back to anonymous access when no credential is found for a
        --  registry. Turning this off makes credential misconfiguration loud
        --  rather than silent, at the cost of losing public registries.
        anonymousFallback : Bool
      }

let Http =
      { connectTimeoutMs : Optional Natural
      , readTimeoutMs : Optional Natural
      , httpsProxy : Optional Text
      , httpProxy : Optional Text
      , noProxy : Optional Text
      , userAgent : Text
      }

let Tls =
      { protocol : Protocol
      , --| PEM files added to the trust store, on top of the platform roots.
        --  `PROXY_CA_CERT`, if present in the environment, is appended to this
        --  list at startup rather than replacing it.
        extraRootCertificates : List Text
      , --| Trust *only* `extraRootCertificates`, discarding platform and
        --  built-in roots. Maps to `ClientConfig::tls_certs_only`.
        exclusiveTrustStore : Bool
      }

--  Deliberately absent: `accept_invalid_certificates`. It is available on
--  `ClientConfig`, and exposing it would be a mistake. Plaintext HTTP at least
--  announces itself in the config file and on the wire; TLS with verification
--  disabled looks secure in every log and dashboard while providing nothing.
--  If you need a local registry, use `HttpsExcept`. If you are behind a TLS
--  inspection proxy, add its CA to `extraRootCertificates`.

let Config =
      { Type =
          { unqualifiedRefs : UnqualifiedRefPolicy
          , tls : Tls
          , auth : Auth
          , http : Http
          , mirrors : List Mirror
          }
      , default =
          { unqualifiedRefs =
              UnqualifiedRefPolicy.Qualify
                { registry = "docker.io", namespace = Some "library" }
          , tls =
              { protocol = Protocol.Https
              , extraRootCertificates = [] : List Text
              , exclusiveTrustStore = False
              }
          , auth = { dockerConfig = None Text, anonymousFallback = True }
          , http =
              { connectTimeoutMs = Some 5000
              , readTimeoutMs = Some 30000
              , httpsProxy = None Text
              , httpProxy = None Text
              , noProxy = None Text
              , userAgent = "polar-oci-resolver"
              }
          , mirrors = [] : List Mirror
          }
      }

in  { Protocol
    , DefaultRegistry
    , UnqualifiedRefPolicy
    , Mirror
    , Auth
    , Http
    , Tls
    , Config
    }
