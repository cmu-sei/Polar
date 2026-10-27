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

//! A test CA and a daemon on a socket in a temporary directory, for the tests that go through the socket.

#![allow(dead_code)]

use polar_custody::custody::Custody;
use polar_custody::server::{Callers, bind, serve};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub const IDENTITY: &str = "git-repo-observer.polar.serviceaccount.cluster.local";

pub struct TestCa {
    pub issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    pub pem: String,
}

pub fn test_ca() -> TestCa {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Polar Internal CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let key = rcgen::KeyPair::generate().unwrap();
    let pem = params.self_signed(&key).unwrap().pem();
    TestCa {
        issuer: rcgen::Issuer::new(params, key),
        pem,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// What cert-issuer does with a CSR: a client certificate for an hour from a minute ago.
pub fn certify(ca: &TestCa, csr_pem: &str) -> String {
    let mut csr = rcgen::CertificateSigningRequestParams::from_pem(csr_pem).unwrap();
    let epoch = rcgen::date_time_ymd(1970, 1, 1);
    csr.params.not_before = epoch + std::time::Duration::from_secs((now() - 60) as u64);
    csr.params.not_after = epoch + std::time::Duration::from_secs((now() + 3600) as u64);
    csr.params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    csr.params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    csr.signed_by(&ca.issuer).unwrap().pem()
}

pub fn own_uid() -> u32 {
    // SAFETY: getuid takes nothing and cannot fail.
    unsafe { libc::getuid() }
}

pub struct Daemon {
    pub socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// A daemon serving `callers`, trusting `ca`.
pub async fn daemon(ca: &TestCa, callers: Callers) -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("custody.sock");
    let listener = bind(&socket).unwrap();
    let custody = Arc::new(RwLock::new(Custody::new(&ca.pem).unwrap()));
    tokio::spawn(serve(listener, custody, Arc::new(callers)));
    Daemon { socket, _dir: dir }
}

/// This process as both the installer and the workload.
pub fn both() -> Callers {
    Callers {
        installer_uid: own_uid(),
        workload_uids: vec![own_uid()],
    }
}
