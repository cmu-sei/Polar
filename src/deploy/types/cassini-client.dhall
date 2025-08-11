let ClientTLSConfig =
      { broker_endpoint : Text
      , server_name : Text
      , proxy_ca_cert_path : Optional Text
      , client_certificate_path : Text
      , client_key_path : Text
      , client_ca_cert_path : Text
      }
in ClientTLSConfig