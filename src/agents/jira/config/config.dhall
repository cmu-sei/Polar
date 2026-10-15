-- ACE Tool
--
-- Copyright 2026 Carnegie Mellon University.
--
-- NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
-- FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
-- EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
-- PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
-- MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
-- TRADEMARK, OR COPYRIGHT INFRINGEMENT.
--
-- Licensed under a MIT (SEI)-style license, please see license.txt or contact
-- permission@sei.cmu.edu for full terms.
--
-- [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
-- distribution.  Please see Copyright notice for non-US Government use and distribution.
--
-- This Software includes and/or makes use of Third-Party Software each subject to its own license.
--
-- DM26-0670

-- TODO: Figure out a good configuration schema for the gitlab observer
let GitlabConfig = {
  graphql_endpoint : Text,
  token : Text,
  proxy_ca_cert_file : Optional Text,
  tls : {
    client_cert : Text,
    client_key : Text,
    ca_cert : Text
  },
  broker : {
    address : Text,
    server_name : Text
  },
  base_interval_secs : Natural,
  initial_backoff_secs : Natural,
  max_backoff_secs : Natural
}

let gitlabConfig : GitlabConfig = {
    base_interval_secs = 300,
    initial_backoff_secs = 60,
    max_backoff_secs = 1800,
    graphql_endpoint = env:GITLAB_ENDPOINT as Text,
    token = env:GITLAB_TOKEN as Text,
    proxy_ca_cert_file = Some env:PROXY_CA_CERT as Text,
    
    tls = {
        client_cert = env:TLS_CLIENT_CERT as Text,
        client_key = env:TLS_CLIENT_KEY as Text,
        ca_cert = env:TLS_CA_CERT as Text
    },
    broker = {
        address = env:BROKER_ADDR as Text,
        server_name = env:CASSINI_SERVER_NAME as Text
    },

}

in gitlabConfig
