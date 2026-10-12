//! `polar-sign configuration` makes a signed configuration document; `polar-sign verify` checks one.
//! Design: `docs/architecture/signed-configuration.md`.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use polar_env::Env;
use polar_sign::{
    configuration_payload, load_identity, parse_change, sign_to_dhall, split_pem, verify_source,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "polar-sign", about = "Sign and check Polar configuration documents")]
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
        /// Whose certificate and key to sign with: <dir>/<user>.crt and .key.
        #[arg(long, default_value = "djshepard")]
        user: String,
        /// Where they are; ~/.config/polar-admin by default.
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        out: Option<PathBuf>,
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

fn default_dir(given: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = given {
        return Ok(dir);
    }
    let home = polar_env::ProcessEnv
        .var("HOME")
        .context("HOME is not set: say where your certificate is with --dir")?;
    Ok(PathBuf::from(home).join(".config/polar-admin"))
}

fn main() -> Result<()> {
    match Cli::parse().command {
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
                    eprintln!("wrote {} ({} change(s) for {agent}, version {version})", path.display(), changes.len());
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
            let (signer, payload) = verify_source(&source, &trust, now).map_err(anyhow::Error::msg)?;
            println!(
                "signed by {} <{}> (identity {}, certificate {})",
                signer.username.as_deref().unwrap_or("?"),
                signer.email.as_deref().unwrap_or("?"),
                signer.identity.as_deref().unwrap_or("?"),
                signer.serial
            );
            let capabilities: Vec<String> = signer.capabilities.iter().map(|c| c.to_san_string()).collect();
            println!("capabilities: {}", capabilities.join(" "));
            println!(
                "type {}, for {}, version {}, changing: {}",
                payload["type"].as_str().unwrap_or("?"),
                payload["target"]["agent"].as_str().unwrap_or("?"),
                payload["version"],
                payload["changes"]
                    .as_array()
                    .map(|c| c.iter().filter_map(|c| c["name"].as_str()).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}
