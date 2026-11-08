/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

pub mod backend;
pub mod error;
pub mod types;

/// The path inside every job where source code is available.
/// In the kubernetes backends both the init container (writer) and the main container (reader) mount
/// the workspace emptyDir volume here. This is a framework convention.
pub const WORKSPACE_PATH: &str = "/workspace";

pub const TLS_CLIENT_KEY: &str = "tls.key";
pub const TLS_CLIENT_CERT: &str = "tls.crt";
pub const TLS_CA_CERT: &str = "ca.crt";
pub const TLS_CERT_DIR: &str = "/etc/tls";

pub fn tls_client_cert_path() -> String {
    format!("{TLS_CERT_DIR}/{TLS_CLIENT_CERT}")
}

pub fn tls_client_key_path() -> String {
    format!("{TLS_CERT_DIR}/{TLS_CLIENT_KEY}")
}

pub fn tls_ca_cert_path() -> String {
    format!("{TLS_CERT_DIR}/{TLS_CA_CERT}")
}
