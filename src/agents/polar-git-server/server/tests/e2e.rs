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

//! The server against the real `git` CLI over real mTLS, with certificates minted for the test.

use polar_git_server::access::{RepoAcl, RuntimeAcl, StaticAcl};
use polar_git_server::git::GitRunner;
use polar_git_server::http::{ServerState, router};
use polar_git_server::notify::{PushLog, Recorder, RepoChanged, Tee};
use polar_git_server::store::RepoStore;
use polar_git_server::tls::{serve_tls, server_config};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, SanType,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;

const REPO: &str = "polar-schedules";
/// The name the cluster gives the server, which its certificate carries.
const CLUSTER_NAME: &str = "polar-git-server.polar.svc.cluster.local";

struct Ca {
    issuer: Issuer<'static, KeyPair>,
    pem: String,
}

fn new_ca(name: &str) -> Ca {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().unwrap();
    let pem = params.self_signed(&key).unwrap().pem();
    Ca {
        issuer: Issuer::new(params, key),
        pem,
    }
}

/// Writes `<name>.pem` and `<name>.key` signed by `ca`; returns their paths.
fn leaf(
    ca: &Ca,
    dir: &Path,
    name: &str,
    sans: Vec<SanType>,
    eku: ExtendedKeyUsagePurpose,
) -> (PathBuf, PathBuf) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.subject_alt_names = sans;
    params.extended_key_usages = vec![eku];
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, &ca.issuer).unwrap();
    let (c, k) = (
        dir.join(format!("{name}.pem")),
        dir.join(format!("{name}.key")),
    );
    std::fs::write(&c, cert.pem()).unwrap();
    std::fs::write(&k, key.serialize_pem()).unwrap();
    (c, k)
}

fn client(ca: &Ca, dir: &Path, identity: &str) -> (PathBuf, PathBuf) {
    leaf(
        ca,
        dir,
        identity,
        vec![SanType::DnsName(identity.try_into().unwrap())],
        ExtendedKeyUsagePurpose::ClientAuth,
    )
}

struct Harness {
    dir: tempfile::TempDir,
    ca: Ca,
    ca_file: PathBuf,
    port: u16,
    notes: Arc<Recorder>,
    stop: Arc<Notify>,
}

impl Harness {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ca = new_ca("polar-test-ca");
        let ca_file = dir.path().join("ca.pem");
        std::fs::write(&ca_file, &ca.pem).unwrap();
        let (cert, key) = leaf(
            &ca,
            dir.path(),
            "server",
            vec![
                SanType::IpAddress("127.0.0.1".parse().unwrap()),
                SanType::DnsName(CLUSTER_NAME.try_into().unwrap()),
            ],
            ExtendedKeyUsagePurpose::ServerAuth,
        );

        let store = RepoStore::new(dir.path().join("repos"));
        std::fs::create_dir_all(dir.path().join("repos")).unwrap();
        store.create(REPO).await.unwrap();
        let seed = dir.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(seed.join("credentials.dhall"), "{ seeded = True }").unwrap();
        store.seed(REPO, &seed).await.unwrap();

        let acl = StaticAcl {
            admins: ["root".to_string()].into(),
            repos: [(
                REPO.to_string(),
                RepoAcl {
                    readers: ["polar-scheduler-observer".to_string()].into(),
                    writers: ["alice".to_string()].into(),
                },
            )]
            .into(),
        };
        let notes = Arc::new(Recorder::default());
        let acl = Arc::new(
            RuntimeAcl::new(acl, Some(dir.path().join("repos/runtime-acl.json"))).unwrap(),
        );
        let app = router(ServerState {
            store,
            authorizer: acl.clone(),
            runtime: acl,
            runner: GitRunner::default(),
            notifier: Arc::new(Tee(vec![
                Arc::new(PushLog::new(dir.path().join("repos/push-log.jsonl"))),
                notes.clone(),
            ])),
            max_body: 64 * 1024 * 1024,
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = TlsAcceptor::from(server_config(&cert, &key, &ca_file).unwrap());
        let stop = Arc::new(Notify::new());
        tokio::spawn(serve_tls(
            listener,
            acceptor,
            app,
            stop.clone(),
            Default::default(),
        ));
        Self {
            dir,
            ca,
            ca_file,
            port,
            notes,
            stop,
        }
    }

    fn url(&self) -> String {
        format!("https://127.0.0.1:{}/{REPO}.git", self.port)
    }

    fn changes(&self) -> Vec<RepoChanged> {
        self.notes.0.lock().unwrap().clone()
    }

    /// Runs git as the holder of `creds` (None: no client certificate) in `cwd`.
    async fn git(
        &self,
        cwd: &Path,
        creds: Option<&(PathBuf, PathBuf)>,
        args: &[&str],
    ) -> (bool, String) {
        let mut cmd = tokio::process::Command::new("git");
        cmd.current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_SSL_CAINFO")
            .env_remove("SSL_CERT_FILE")
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .args(["-c", &format!("http.sslCAInfo={}", self.ca_file.display())])
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"]);
        if let Some((c, k)) = creds {
            cmd.args(["-c", &format!("http.sslCert={}", c.display())])
                .args(["-c", &format!("http.sslKey={}", k.display())]);
        }
        let out = cmd.args(args).output().await.unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    async fn clone_as(
        &self,
        creds: Option<&(PathBuf, PathBuf)>,
        into: &str,
    ) -> (bool, PathBuf, String) {
        let dest = self.dir.path().join(into);
        let (ok, text) = self
            .git(
                self.dir.path(),
                creds,
                &["clone", &self.url(), dest.to_str().unwrap()],
            )
            .await;
        (ok, dest, text)
    }

    async fn commit(&self, repo: &Path, file: &str, content: &str) {
        std::fs::write(repo.join(file), content).unwrap();
        let (ok, t) = self.git(repo, None, &["add", "--all"]).await;
        assert!(ok, "{t}");
        let (ok, t) = self
            .git(
                repo,
                None,
                &["commit", "-m", &format!("chore: edit {file}")],
            )
            .await;
        assert!(ok, "{t}");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

#[tokio::test]
async fn an_authorized_reader_clones_the_seeded_content() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "polar-scheduler-observer");
    let (ok, dest, text) = h.clone_as(Some(&creds), "obs").await;
    assert!(ok, "{text}");
    assert_eq!(
        std::fs::read_to_string(dest.join("credentials.dhall")).unwrap(),
        "{ seeded = True }"
    );
}

#[tokio::test]
async fn an_identity_with_no_access_is_refused() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "mallory");
    let (ok, _, text) = h.clone_as(Some(&creds), "m").await;
    assert!(!ok, "{text}");
    assert!(text.contains("403"), "{text}");
}

#[tokio::test]
async fn there_is_no_anonymous_access() {
    let h = Harness::start().await;
    let (ok, _, text) = h.clone_as(None, "anon").await;
    assert!(!ok, "{text}");
}

#[tokio::test]
async fn a_certificate_from_another_ca_is_refused() {
    let h = Harness::start().await;
    let rogue = new_ca("rogue");
    let creds = client(&rogue, h.dir.path(), "alice");
    let (ok, _, text) = h.clone_as(Some(&creds), "rogue").await;
    assert!(!ok, "{text}");
}

#[tokio::test]
async fn a_writer_pushes_and_the_change_is_announced() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "alice");
    let (ok, repo, text) = h.clone_as(Some(&creds), "a").await;
    assert!(ok, "{text}");
    h.commit(&repo, "new.dhall", "1").await;
    let (ok, text) = h
        .git(&repo, Some(&creds), &["push", "origin", "HEAD:main"])
        .await;
    assert!(ok, "{text}");

    let changes = h.changes();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].repo, REPO);
    assert_eq!(changes[0].refname, "refs/heads/main");
    assert_eq!(changes[0].pusher, "alice");

    // And a reader now sees it.
    let obs = client(&h.ca, h.dir.path(), "polar-scheduler-observer");
    let (ok, dest, text) = h.clone_as(Some(&obs), "obs").await;
    assert!(ok, "{text}");
    assert!(dest.join("new.dhall").exists());
}

#[tokio::test]
async fn a_reader_cannot_push_and_nothing_is_announced() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "polar-scheduler-observer");
    let (ok, repo, text) = h.clone_as(Some(&creds), "r").await;
    assert!(ok, "{text}");
    h.commit(&repo, "sneaky.dhall", "x").await;
    let (ok, text) = h
        .git(&repo, Some(&creds), &["push", "origin", "HEAD:main"])
        .await;
    assert!(!ok, "{text}");
    assert!(h.changes().is_empty());
}

#[tokio::test]
async fn a_rewrite_of_history_is_refused_and_not_announced() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "alice");
    let (_, repo, _) = h.clone_as(Some(&creds), "w").await;
    h.commit(&repo, "one.dhall", "1").await;
    let (ok, text) = h
        .git(&repo, Some(&creds), &["push", "origin", "HEAD:main"])
        .await;
    assert!(ok, "{text}");
    assert_eq!(h.changes().len(), 1);

    let (ok, t) = h.git(&repo, None, &["reset", "--hard", "HEAD~1"]).await;
    assert!(ok, "{t}");
    h.commit(&repo, "two.dhall", "2").await;
    let (ok, text) = h
        .git(
            &repo,
            Some(&creds),
            &["push", "--force", "origin", "HEAD:main"],
        )
        .await;
    assert!(!ok, "{text}");
    assert_eq!(
        h.changes().len(),
        1,
        "a refused update must not be announced"
    );
}

#[tokio::test]
async fn a_push_to_a_tag_is_refused() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "alice");
    let (_, repo, _) = h.clone_as(Some(&creds), "t").await;
    let (ok, t) = h.git(&repo, None, &["tag", "v1"]).await;
    assert!(ok, "{t}");
    let (ok, text) = h.git(&repo, Some(&creds), &["push", "origin", "v1"]).await;
    assert!(!ok, "{text}");
    assert!(h.changes().is_empty());
}

/// What `just polar-git-clone` relies on: the URL keeps the name on the certificate, and curl is
/// told that name lives on the forwarded local port.
#[tokio::test]
async fn the_cluster_name_works_through_a_local_forward() {
    let h = Harness::start().await;
    let creds = client(&h.ca, h.dir.path(), "polar-scheduler-observer");
    let dest = h.dir.path().join("by-name");
    let url = format!("https://{CLUSTER_NAME}:{}/{REPO}.git", h.port);
    let resolve = format!("http.curloptResolve={CLUSTER_NAME}:{}:127.0.0.1", h.port);
    let (ok, text) = h
        .git(
            h.dir.path(),
            Some(&creds),
            &["-c", &resolve, "clone", &url, dest.to_str().unwrap()],
        )
        .await;
    assert!(ok, "{text}");
    assert!(dest.join("credentials.dhall").exists());

    // Without the mapping the name does not resolve, so the test is not passing by accident.
    let other = h.dir.path().join("unmapped");
    let (ok, _) = h
        .git(
            h.dir.path(),
            Some(&creds),
            &["clone", &url, other.to_str().unwrap()],
        )
        .await;
    assert!(!ok);
}

impl Harness {
    /// Calls the admin interface as the holder of `creds` with curl, which can present a client
    /// certificate; returns the HTTP status and body.
    async fn admin_put(&self, creds: &(PathBuf, PathBuf), name: &str, body: &str) -> (u16, String) {
        let out = tokio::process::Command::new("curl")
            .args(["--silent", "--show-error", "--noproxy", "*"])
            .args(["--cacert", self.ca_file.to_str().unwrap()])
            .args(["--cert", creds.0.to_str().unwrap()])
            .args(["--key", creds.1.to_str().unwrap()])
            .args(["--request", "PUT", "--data", body])
            .args(["--write-out", "\n%{http_code}"])
            .arg(format!(
                "https://127.0.0.1:{}/admin/repos/{name}",
                self.port
            ))
            .output()
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, code) = text.rsplit_once('\n').unwrap_or(("", "0"));
        (code.trim().parse().unwrap_or(0), body.to_string())
    }
}

#[tokio::test]
async fn an_admin_creates_a_repository_that_its_writers_then_use() {
    let h = Harness::start().await;
    let root = client(&h.ca, h.dir.path(), "root");
    let (status, body) = h
        .admin_put(&root, "extra", r#"{"readers":["bob"],"writers":["carol"]}"#)
        .await;
    assert_eq!(status, 201, "{body}");
    // Asking again is not an error, and does not say "created" a second time.
    let (status, _) = h
        .admin_put(&root, "extra", r#"{"readers":["bob"],"writers":["carol"]}"#)
        .await;
    assert_eq!(status, 200);

    let carol = client(&h.ca, h.dir.path(), "carol");
    let url = format!("https://127.0.0.1:{}/extra.git", h.port);
    let work = h.dir.path().join("extra-work");
    let (ok, text) = h
        .git(
            h.dir.path(),
            Some(&carol),
            &["clone", &url, work.to_str().unwrap()],
        )
        .await;
    assert!(ok, "{text}");
    h.commit(&work, "a.dhall", "1").await;
    let (ok, text) = h
        .git(&work, Some(&carol), &["push", "origin", "HEAD:main"])
        .await;
    assert!(ok, "{text}");

    let bob = client(&h.ca, h.dir.path(), "bob");
    let (ok, text) = h
        .git(
            h.dir.path(),
            Some(&bob),
            &["clone", &url, h.dir.path().join("bob").to_str().unwrap()],
        )
        .await;
    assert!(ok, "{text}");
    let mallory = client(&h.ca, h.dir.path(), "mallory");
    let (ok, _) = h
        .git(
            h.dir.path(),
            Some(&mallory),
            &["clone", &url, h.dir.path().join("mal").to_str().unwrap()],
        )
        .await;
    assert!(!ok);
}

#[tokio::test]
async fn only_an_admin_may_create_and_a_declared_repository_is_not_reconfigured() {
    let h = Harness::start().await;
    let alice = client(&h.ca, h.dir.path(), "alice");
    let (status, _) = h.admin_put(&alice, "mine", "{}").await;
    assert_eq!(
        status, 403,
        "a writer of another repository is not an admin"
    );

    let root = client(&h.ca, h.dir.path(), "root");
    let (status, _) = h.admin_put(&root, REPO, r#"{"writers":["mallory"]}"#).await;
    assert_eq!(status, 409);
    let (status, _) = h.admin_put(&root, "Bad..Name", "{}").await;
    assert_eq!(status, 400);
    let (status, _) = h.admin_put(&root, "ok", r#"{"writerz":[]}"#).await;
    assert_eq!(status, 400, "a misspelled key is an error");

    // The refusal left alice's access to the declared repository as it was.
    let (ok, _, text) = h.clone_as(Some(&alice), "still").await;
    assert!(ok, "{text}");
}

#[tokio::test]
async fn accepted_pushes_are_recorded_and_refused_ones_are_not() {
    let h = Harness::start().await;
    let alice = client(&h.ca, h.dir.path(), "alice");
    let (_, repo, _) = h.clone_as(Some(&alice), "log").await;
    h.commit(&repo, "one.dhall", "1").await;
    let (ok, t) = h
        .git(&repo, Some(&alice), &["push", "origin", "HEAD:main"])
        .await;
    assert!(ok, "{t}");

    let mallory = client(&h.ca, h.dir.path(), "mallory");
    let (ok, _, _) = h.clone_as(Some(&mallory), "nope").await;
    assert!(!ok);

    let log = std::fs::read_to_string(h.dir.path().join("repos/push-log.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert_eq!(lines[0]["pusher"], "alice");
    assert_eq!(lines[0]["repo"], REPO);
    assert_eq!(lines[0]["ref"], "refs/heads/main");
}
