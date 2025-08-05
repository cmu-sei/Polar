let types =
      ./lib/types.dhall
        sha256:0942ea9905d797d10cca312d28181d48d4d7bba2ac2472096e13f7e1736388ab

let tls
    : types.ClientTLSConfig
    = { broker_endpoint =
          env:BROKER_ADDR
            sha256:4dad9d81db9fbce03a95f15e4a20e17673c8e9957e12867c927f4f8c2bdbd07c as Text
      , server_name =
          env:CASSINI_SERVER_NAME
            sha256:7e4f0245fe821e1533ef237767499e5e3bb38cb7b89725ce9f2957ef8dcea924 as Text
      , proxy_ca_cert_path = Some
          (   env:PROXY_CA_CERT
                sha256:f08ed5225480d827ff3ce74b756afa6330f66d974d6f0d6160d767b5c45642aa as Text
            ? ""
          )
      , client_certificate_path =
          env:TLS_CLIENT_CERT
            sha256:244747b913d159bb70711397be424228344c65a87b3bc0c91dbb530158614c4e as Text
      , client_key_path =
          env:TLS_CLIENT_KEY
            sha256:130c38561edb5929894cd4479c59ea9a294950dd63e0f9527faa3fccc4c4c7ea as Text
      , client_ca_cert_path =
          env:TLS_CA_CERT
            sha256:9ad892e2e871b0b26799ca3bc937ff37a24b935a37d3ce66e5fbdf625ee758c1 as Text
      }

let gitlabObserver
    : types.GitlabObserver
    = { base_interval_secs = 300
      , max_backoff_secs = 600
      , gitlab_endpoint =
          env:GITLAB_ENDPOINT
            sha256:ddb7e29b9a81d5ead0423cb4cc704bb207b390ff85f39203b04c2a1936298bcd as Text
      , gitlab_token = Some
          env:GITLAB_TOKEN
            sha256:848243f5c88753f40ca90ce6f5ca3a6ff62e702b37738e5b7f066a988f2e7723 as Text
      , tls
      }

in  gitlabObserver
