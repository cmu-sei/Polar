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

//! What a client configured by `polar::egress` trusts and where it goes. The system's roots and the proxy
//! variables are read from the process environment, which a test must not change in place, so the tests that
//! set them run again in a child process (`testing::rerun`) and report what they reached.

use polar::egress::Egress;
use polar::egress::testing::{TestCa, given, https_server, recording_proxy, rerun};
use proptest::prelude::*;
use reqwest::ClientBuilder;
use std::time::Duration;

/// The client an agent makes: reqwest's defaults, the egress settings, and a bound on each request.
async fn client(egress: &Egress) -> reqwest::Client {
    let bundle = egress.ca_bundle().await.unwrap();
    egress
        .configure(
            ClientBuilder::new().timeout(Duration::from_secs(10)),
            &bundle,
        )
        .unwrap()
        .build()
        .unwrap()
}

/// Whether `url` answered `ok`, said as a word, so a child can report it.
async fn reach(client: &reqwest::Client, url: &str) -> &'static str {
    let Ok(response) = client.get(url).send().await else {
        return "refused";
    };
    match response.text().await {
        Ok(body) if body == "ok" => "ok",
        _ => "refused",
    }
}

/// In a child: what each URL of EGRESS_TEST_URLS gave, one `RESULT <url> <ok|refused>` line each.
async fn report_reach() {
    let egress = Egress {
        ca_cert: given("POLAR_EGRESS_CA_CERT"),
        ..Egress::default()
    };
    let client = client(&egress).await;
    for url in given("EGRESS_TEST_URLS").unwrap().split(',') {
        println!("RESULT {url} {}", reach(&client, url).await);
    }
}

fn results(output: &std::process::Output) -> Vec<String> {
    assert!(
        output.status.success(),
        "the child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The test harness's own `test <name> ...` may begin the first line.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| {
            l.find("RESULT ")
                .map(|at| l[at + "RESULT ".len()..].to_string())
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_egress_ca_is_trusted_beside_the_system_roots_not_in_their_place() {
    if given("EGRESS_TEST_URLS").is_some() {
        return report_reach().await;
    }
    let dir = tempfile::tempdir().unwrap();
    // The image's roots are SSL_CERT_FILE; the egress CA is another.
    let system = TestCa::new("system roots");
    let corporate = TestCa::new("corporate root");
    let system_file = system.write(dir.path(), "system.pem");
    let corporate_file = corporate.write(dir.path(), "corporate.pem");
    let inside = https_server(&system).await.url;
    let outside = https_server(&corporate).await.url;
    let urls = format!("{inside},{outside}");
    let name = "the_egress_ca_is_trusted_beside_the_system_roots_not_in_their_place";

    let both = rerun(
        name,
        &[
            ("SSL_CERT_FILE", system_file.to_str().unwrap()),
            ("POLAR_EGRESS_CA_CERT", corporate_file.to_str().unwrap()),
            ("EGRESS_TEST_URLS", &urls),
        ],
    )
    .await;
    assert_eq!(
        results(&both),
        [format!("{inside} ok"), format!("{outside} ok")]
    );

    // Without the egress CA the corporate server is not trusted: the test can tell.
    let system_only = rerun(
        name,
        &[
            ("SSL_CERT_FILE", system_file.to_str().unwrap()),
            ("EGRESS_TEST_URLS", &urls),
        ],
    )
    .await;
    assert_eq!(
        results(&system_only),
        [format!("{inside} ok"), format!("{outside} refused")]
    );
}

#[tokio::test]
async fn every_certificate_of_a_bundle_is_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let first = TestCa::new("first");
    let second = TestCa::new("second");
    let bundle = dir.path().join("bundle.pem");
    std::fs::write(&bundle, format!("{}{}", first.pem, second.pem)).unwrap();
    let one = https_server(&first).await.url;
    let two = https_server(&second).await.url;
    let egress = Egress {
        ca_cert: Some(bundle.display().to_string()),
        ..Egress::default()
    };

    assert_eq!(egress.ca_bundle().await.unwrap().len(), 2);
    assert_eq!(egress.pem_blocks().await.unwrap().len(), 2);
    let client = client(&egress).await;
    assert_eq!(reach(&client, &one).await, "ok");
    assert_eq!(reach(&client, &two).await, "ok");
}

#[tokio::test]
async fn a_call_outside_goes_through_the_proxy_and_a_bypassed_host_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let ca = TestCa::new("cluster");
    let ca_file = ca.write(dir.path(), "ca.pem");
    let inside = https_server(&ca).await.url;
    let (proxy, seen) = recording_proxy().await;
    let egress = Egress {
        proxy: Some(proxy),
        no_proxy: Some("localhost,127.0.0.1,::1,.svc,.cluster.local".to_string()),
        ca_cert: Some(ca_file.display().to_string()),
    };
    let client = client(&egress).await;

    assert_eq!(
        reach(&client, "https://outside.example.org/repo").await,
        "refused"
    );
    assert_eq!(
        seen.lock().unwrap().clone(),
        ["CONNECT outside.example.org:443 HTTP/1.1"]
    );
    // 127.0.0.1 is in the bypass list: the server is reached directly.
    assert_eq!(reach(&client, &inside).await, "ok");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// In a child: one request out with the client EGRESS_TEST_CLIENT names, `egress` (no egress set) or `plain`
/// (reqwest's own defaults).
async fn request_out() {
    let client = match given("EGRESS_TEST_CLIENT").as_deref() {
        Some("egress") => client(&Egress::default()).await,
        _ => ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    };
    let _ = client.get("http://egress-test.invalid/").send().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_egress_set_the_process_proxy_is_not_used() {
    if given("EGRESS_TEST_CLIENT").is_some() {
        return request_out().await;
    }
    let (proxy, seen) = recording_proxy().await;
    let name = "with_no_egress_set_the_process_proxy_is_not_used";
    let env = |client: &'static str| {
        vec![
            ("HTTP_PROXY", proxy.clone()),
            ("HTTPS_PROXY", proxy.clone()),
            ("ALL_PROXY", proxy.clone()),
            ("NO_PROXY", String::new()),
            ("EGRESS_TEST_CLIENT", client.to_string()),
        ]
    };
    let run = |client: &'static str| {
        let env = env(client);
        async move {
            let pairs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let output = rerun(name, &pairs).await;
            assert!(output.status.success(), "{output:?}");
        }
    };

    run("egress").await;
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
    // reqwest's defaults would have used it: the test can tell.
    run("plain").await;
    assert_eq!(
        seen.lock().unwrap().clone(),
        ["GET http://egress-test.invalid/ HTTP/1.1"]
    );
}

#[test]
fn git_is_given_no_proxy_without_one_and_none_for_an_ssh_remote() {
    let egress = Egress {
        proxy: Some("http://proxy.corp.example:3128".to_string()),
        no_proxy: Some(".svc,.cluster.local".to_string()),
        ca_cert: None,
    };
    assert_eq!(
        egress
            .proxy_for("https://github.com/org/repo.git")
            .as_deref(),
        Some("http://proxy.corp.example:3128")
    );
    assert_eq!(
        egress.proxy_for("https://polar-git-server.polar.svc.cluster.local/x.git"),
        None
    );
    assert_eq!(egress.proxy_for("ssh://git@github.com/org/repo.git"), None);
    assert_eq!(egress.proxy_for("git@github.com:org/repo.git"), None);
    assert_eq!(
        Egress::default().proxy_for("https://github.com/org/repo.git"),
        None
    );
}

fn label() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9-]{0,10}[a-z0-9]"
}

proptest! {
    /// The defaults the render writes keep the cluster's own names off the proxy; everything else goes through it.
    #[test]
    fn a_host_in_the_cluster_bypasses_the_proxy_and_any_other_does_not(
        names in prop::collection::vec(label(), 1..4),
        tld in prop::sample::select(vec!["com", "org", "net", "io", "example"]),
        domain in prop::sample::select(vec!["cluster.local", "corp.internal"]),
        https in any::<bool>(),
        path in "(/[a-z0-9]{1,8}){0,3}",
    ) {
        let proxy = "http://proxy.corp.example:3128";
        let egress = Egress {
            proxy: Some(proxy.to_string()),
            no_proxy: Some(format!("localhost,127.0.0.1,::1,.svc,.{domain}")),
            ca_cert: None,
        };
        let scheme = if https { "https" } else { "http" };
        let host = names.join(".");
        for inside in [format!("{host}.svc"), format!("{host}.polar.svc.{domain}"), format!("{host}.{domain}")] {
            prop_assert_eq!(egress.proxy_for(&format!("{scheme}://{inside}{path}")), None);
        }
        let outside = format!("{host}.{tld}");
        prop_assert_eq!(egress.proxy_for(&format!("{scheme}://{outside}{path}")), Some(proxy.to_string()));
    }
}
