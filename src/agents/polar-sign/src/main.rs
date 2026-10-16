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

//! `polar-sign template` gives a file to fill in for an agent's runtime section or settings; `polar-sign sign`
//! checks it, seals its secrets, signs it and, with `--push`, puts it in the schedules repository;
//! `polar-sign configuration` and `runtime` sign a change given on the command line; `polar-sign verify`
//! checks a signed document. Design: `docs/architecture/signed-configuration.md`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use polar_env::Env;
use polar_sign::flow::{Signing, sign_file};
use polar_sign::scaffold::Scaffold;
use polar_sign::{
    author, configuration_payload, fetch, load_identity, parse_body, parse_change, push,
    runtime_payload, sign_to_dhall, split_pem, template, verify_source,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "polar-sign",
    about = "Sign and check Polar configuration documents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign a configuration update for an agent with your certificate and key.
    ///
    /// Writes one Dhall file to commit to the schedules repository (to stdout unless --out is
    /// given). The signer needs the c:update-configuration capability, and the agent refuses a
    /// version that is not higher than the last it applied.
    Configuration {
        /// The agent's name, as it calls itself (its service name).
        #[arg(long)]
        agent: String,
        /// Must be higher than the last version the agent applied.
        #[arg(long)]
        version: u64,
        /// A change, NAME=VALUE. Only variables the agent declares dynamic can be changed.
        #[arg(long = "set", value_name = "NAME=VALUE", required = true)]
        changes: Vec<String>,
        /// The item's id; a new one if not given.
        #[arg(long)]
        id: Option<String>,
        /// Whose certificate and key to sign with: <dir>/<user>.crt and .key. Required: it is never assumed.
        #[arg(long)]
        user: String,
        /// Where they are; ~/.config/polar-admin by default.
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Sign a runtime section for an agent: its policy, its groups, the repositories it watches.
    ///
    /// The body is JSON, an object, as the agent declared that section (`<agent> --help` lists them).
    /// The signer needs the capability the section asks for (c:update-runtime unless the agent says
    /// otherwise: c:update-policy for `policy`, c:update-groups for `groups`), and the agent refuses a
    /// version that is not higher than the last it applied to that section.
    Runtime {
        #[arg(long)]
        agent: String,
        /// The section's name.
        #[arg(long)]
        section: String,
        #[arg(long)]
        version: u64,
        /// The body as JSON, or --body-file.
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// A file holding the body as JSON; `-` reads standard input.
        #[arg(long)]
        body_file: Option<PathBuf>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        user: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print a file to fill in for an agent: one of its runtime sections, or the settings a signed document may
    /// change. It comes from the agent's own scaffold, with the defaults the agent declares and a comment on every
    /// field, and the secret fields marked.
    ///
    /// Edit it, then `polar-sign sign` it. `polar-sign template <agent>` alone lists what the agent takes.
    Template {
        /// The agent's name, as it calls itself.
        agent: String,
        /// The runtime section to print a template for.
        section: Option<String>,
        /// Print the template for the agent's settings instead.
        #[arg(long, conflicts_with = "section")]
        settings: bool,
        /// The agent's scaffold (`<agent> --scaffold`); <dir>/scaffolds/<agent>.json by default.
        #[arg(long)]
        scaffold: Option<PathBuf>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Check a file made from a template, seal its secrets, sign it with your certificate and key and write the
    /// signed Dhall file (to standard output unless --out or --push is given).
    ///
    /// The file is checked against the agent's scaffold first: nothing is signed if a field is missing, strange
    /// or of the wrong kind. A secret you typed in the clear (or `-`, to be asked for it on standard input) is
    /// encrypted so that only that agent can open it, to the agent's escrow certificate, which is fetched from
    /// the credentials agent's gateway and checked against your CA before it is trusted. With --push the file is
    /// committed to the schedules repository at permanent/<agent>/<section>.dhall and pushed.
    Sign {
        /// The file made from a template.
        file: PathBuf,
        #[arg(long)]
        user: String,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        scaffold: Option<PathBuf>,
        /// The credentials agent's gateway; POLAR_OBSERVER_URL, or http://localhost:8081, by default.
        #[arg(long)]
        observer_url: Option<String>,
        /// The identity the agent's escrow certificate must carry, if not <agent>.polar.serviceaccount.cluster.local.
        #[arg(long)]
        escrow_identity: Option<String>,
        #[arg(long)]
        out: Option<PathBuf>,
        /// Commit the signed file to the schedules repository and push it.
        #[arg(long)]
        push: bool,
        /// The schedules repository (anything `git clone` takes); POLAR_SCHEDULES_URL by default.
        #[arg(long)]
        repo: Option<String>,
        /// Where the tool keeps its clone; ~/.cache/polar-sign/schedules by default.
        #[arg(long)]
        workdir: Option<PathBuf>,
        /// KEY=VALUE for every git command of the push (`git -c`), such as http.curloptResolve.
        #[arg(long = "git-config", value_name = "KEY=VALUE")]
        git_config: Vec<String>,
    },
    /// Check a signed document the way a receiver checks its signature: the layout, the chain to the
    /// CA, the signer's certificate and the signature. Says who signed it and what it asks for.
    Verify {
        file: PathBuf,
        /// The CA certificate to trust; <dir>/ca.pem by default.
        #[arg(long)]
        ca: Option<PathBuf>,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

/// What a payload asks for, in a few words: the section of a runtime document, the variables of a
/// configuration one.
fn what_it_changes(payload: &serde_json::Value) -> String {
    if let Some(section) = payload["section"].as_str() {
        return format!("section: {section}");
    }
    let names = payload["changes"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter_map(|c| c["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!("changing: {names}")
}

fn default_dir(given: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = given {
        return Ok(dir);
    }
    let home = polar_env::ProcessEnv
        .var("HOME")
        .context("HOME is not set: say where your certificate is with --dir")?;
    Ok(PathBuf::from(home).join(".config/polar-admin"))
}

fn env_var(name: &str) -> Option<String> {
    polar_env::ProcessEnv.var(name).filter(|v| !v.is_empty())
}

/// The agent's scaffold, from --scaffold or the folder `nu infra/resolve.nu scaffolds <dir>` fills.
fn load_scaffold(agent: &str, given: Option<PathBuf>, dir: &std::path::Path) -> Result<Scaffold> {
    let path = given.unwrap_or_else(|| dir.join("scaffolds").join(format!("{agent}.json")));
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "cannot read the scaffold of {agent} at {}: make one with `{agent} --scaffold > <file>` and pass \
             --scaffold, or fill {} with `nu infra/resolve.nu scaffolds <dir>`",
            path.display(),
            dir.join("scaffolds").display()
        )
    })?;
    let scaffold = Scaffold::parse(&text).map_err(anyhow::Error::msg)?;
    if scaffold.agent != agent {
        bail!(
            "{} is the scaffold of {}, not of {agent}",
            path.display(),
            scaffold.agent
        );
    }
    Ok(scaffold)
}

/// Standard input, read when an author wrote `-` for a secret.
fn read_secret_from_stdin() -> Result<String, String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
        .map_err(|e| format!("cannot read standard input: {e}"))?;
    Ok(text.trim_end_matches(['\n', '\r']).to_string())
}

fn write_or_print(text: &str, out: Option<&std::path::Path>, what: &str) -> Result<()> {
    match out {
        Some(path) => {
            std::fs::write(path, text)
                .with_context(|| format!("cannot write {}", path.display()))?;
            eprintln!("wrote {} ({what})", path.display());
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Template {
            agent,
            section,
            settings,
            scaffold,
            dir,
            out,
        } => {
            let dir = default_dir(dir)?;
            let scaffold = load_scaffold(&agent, scaffold, &dir)?;
            if section.is_none() && !settings {
                println!("{} takes these runtime sections:", scaffold.agent);
                for r in &scaffold.runtime {
                    println!("  {}  {}", r.name, r.doc);
                }
                println!(
                    "and has {} setting(s) a signed document may change (--settings)",
                    scaffold.dynamic_variables().count()
                );
                return Ok(());
            }
            let text = match section {
                Some(section) => template::runtime_template(&scaffold, &section),
                None => template::settings_template(&scaffold),
            }
            .map_err(anyhow::Error::msg)?;
            write_or_print(
                &text,
                out.as_deref(),
                "fill it in, then `polar-sign sign` it",
            )?;
        }
        Command::Sign {
            file,
            user,
            dir,
            scaffold,
            observer_url,
            escrow_identity,
            out,
            push: push_it,
            repo,
            workdir,
            git_config,
        } => {
            let dir = default_dir(dir)?;
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            let input = author::read_input(&text).map_err(anyhow::Error::msg)?;
            let scaffold = load_scaffold(&input.agent, scaffold, &dir)?;
            let identity = load_identity(&dir, &user).map_err(anyhow::Error::msg)?;
            let ca_path = dir.join("ca.pem");
            let trust = split_pem(
                &std::fs::read_to_string(&ca_path)
                    .with_context(|| format!("cannot read {}", ca_path.display()))?,
            );
            if trust.is_empty() {
                bail!("{} holds no certificate", ca_path.display());
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("the clock is before 1970")?
                .as_secs() as i64;
            let observer_url = observer_url
                .or_else(|| env_var("POLAR_OBSERVER_URL"))
                .unwrap_or_else(|| "http://localhost:8081".to_string());
            let how = Signing {
                identity: &identity,
                trust_pem: &trust,
                now,
                escrow_identity: escrow_identity.as_deref(),
            };
            let fetch_certificate = |id: &str| fetch::fetch_certificate(&observer_url, id);
            let signed = match sign_file(
                &text,
                &scaffold,
                &how,
                &fetch_certificate,
                &mut read_secret_from_stdin,
            ) {
                Ok(signed) => signed,
                Err(problems) => {
                    for p in &problems {
                        eprintln!("  {p}");
                    }
                    bail!(
                        "nothing was signed: {} problem(s) in {}",
                        problems.len(),
                        file.display()
                    );
                }
            };
            let what = format!(
                "{} for {}, version {}, {} secret(s) sealed",
                signed.section.as_deref().unwrap_or("settings"),
                signed.agent,
                signed.version,
                signed.sealed.encrypted
            );
            if push_it {
                let (signer, _) =
                    verify_source(&signed.dhall, &trust, now).map_err(anyhow::Error::msg)?;
                let remote = repo.or_else(|| env_var("POLAR_SCHEDULES_URL")).context(
                    "--push needs the schedules repository: --repo, or POLAR_SCHEDULES_URL",
                )?;
                let workdir = match workdir {
                    Some(w) => w,
                    None => {
                        let base = env_var("XDG_CACHE_HOME")
                            .map(PathBuf::from)
                            .or_else(|| env_var("HOME").map(|h| PathBuf::from(h).join(".cache")));
                        base.context("HOME is not set: say where to keep the clone with --workdir")?
                            .join("polar-sign/schedules")
                    }
                };
                let mut config: Vec<(String, String)> = Vec::new();
                if remote.starts_with("https://") {
                    for (key, name) in [
                        ("http.sslCAInfo", "ca.pem".to_string()),
                        ("http.sslCert", format!("{user}.crt")),
                        ("http.sslKey", format!("{user}.key")),
                    ] {
                        config.push((
                            key.to_string(),
                            dir.join(name).to_string_lossy().to_string(),
                        ));
                    }
                }
                for item in &git_config {
                    config.push(parse_change(item).map_err(anyhow::Error::msg)?);
                }
                let relative = push::place(&signed.agent, signed.section.as_deref());
                let username = signer.username.clone().unwrap_or_else(|| user.clone());
                let message = push::message(
                    &signed.agent,
                    signed.section.as_deref(),
                    signed.version,
                    &username,
                );
                let plan = push::Plan {
                    remote: &remote,
                    workdir: &workdir,
                    relative: &relative,
                    content: &signed.dhall,
                    message: &message,
                    author_name: &username,
                    author_email: signer.email.as_deref().unwrap_or("polar-sign@invalid"),
                    git_config: &config,
                };
                match push::push(&plan).map_err(anyhow::Error::msg)? {
                    push::Pushed::Pushed => eprintln!("pushed {relative} ({what})"),
                    push::Pushed::Unchanged => {
                        eprintln!("{relative} already holds exactly this file; nothing was pushed")
                    }
                }
                if let Some(path) = out {
                    write_or_print(&signed.dhall, Some(&path), &what)?;
                }
            } else {
                write_or_print(&signed.dhall, out.as_deref(), &what)?;
            }
        }
        Command::Configuration {
            agent,
            version,
            changes,
            id,
            user,
            dir,
            out,
        } => {
            let changes = changes
                .iter()
                .map(|c| parse_change(c))
                .collect::<Result<Vec<_>, _>>()
                .map_err(anyhow::Error::msg)?;
            let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let payload = configuration_payload(&agent, version, &id, &changes);
            let identity = load_identity(&default_dir(dir)?, &user).map_err(anyhow::Error::msg)?;
            let file = sign_to_dhall(payload, &identity).map_err(anyhow::Error::msg)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, &file)
                        .with_context(|| format!("cannot write {}", path.display()))?;
                    eprintln!(
                        "wrote {} ({} change(s) for {agent}, version {version})",
                        path.display(),
                        changes.len()
                    );
                }
                None => print!("{file}"),
            }
        }
        Command::Runtime {
            agent,
            section,
            version,
            body,
            body_file,
            id,
            user,
            dir,
            out,
        } => {
            let text = match (body, body_file) {
                (Some(text), None) => text,
                (None, Some(path)) if path.as_os_str() == "-" => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                        .context("cannot read the body from standard input")?;
                    text
                }
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read {}", path.display()))?,
                _ => bail!("give the body with --body or --body-file"),
            };
            let body = parse_body(&text).map_err(anyhow::Error::msg)?;
            let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let payload = runtime_payload(&agent, &section, version, &id, body);
            let identity = load_identity(&default_dir(dir)?, &user).map_err(anyhow::Error::msg)?;
            let file = sign_to_dhall(payload, &identity).map_err(anyhow::Error::msg)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, &file)
                        .with_context(|| format!("cannot write {}", path.display()))?;
                    eprintln!(
                        "wrote {} (section {section} for {agent}, version {version})",
                        path.display()
                    );
                }
                None => print!("{file}"),
            }
        }
        Command::Verify { file, ca, dir } => {
            let ca_path = match ca {
                Some(path) => path,
                None => default_dir(dir)?.join("ca.pem"),
            };
            let trust = split_pem(
                &std::fs::read_to_string(&ca_path)
                    .with_context(|| format!("cannot read {}", ca_path.display()))?,
            );
            if trust.is_empty() {
                bail!("{} holds no certificate", ca_path.display());
            }
            let source = std::fs::read_to_string(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("the clock is before 1970")?
                .as_secs() as i64;
            let (signer, payload) =
                verify_source(&source, &trust, now).map_err(anyhow::Error::msg)?;
            println!(
                "signed by {} <{}> (identity {}, certificate {})",
                signer.username.as_deref().unwrap_or("?"),
                signer.email.as_deref().unwrap_or("?"),
                signer.identity.as_deref().unwrap_or("?"),
                signer.serial
            );
            let capabilities: Vec<String> = signer
                .capabilities
                .iter()
                .map(|c| c.to_san_string())
                .collect();
            println!("capabilities: {}", capabilities.join(" "));
            println!(
                "type {}, for {}, version {}, {}",
                payload["type"].as_str().unwrap_or("?"),
                payload["target"]["agent"].as_str().unwrap_or("?"),
                payload["version"],
                what_it_changes(&payload)
            );
        }
    }
    Ok(())
}
