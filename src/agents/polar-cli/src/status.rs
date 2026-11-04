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

//! `polar status`: every PolarAgent, and why the ones that are not running are not.

use crate::exec::explain_agent;
use crate::kube_cluster::KubeCluster;
use anyhow::{Context, Result};
use kube::api::{Api, ListParams};
use polar_operator_common::PolarAgent;

pub async fn status(cluster: &KubeCluster, out: &mut dyn FnMut(String)) -> Result<()> {
    let agents: Api<PolarAgent> = Api::all(cluster.client().clone());
    let list = agents
        .list(&ListParams::default())
        .await
        .context("cannot list PolarAgents (is the CRD installed? `polar up` on a plan with installCrd does it)")?;
    if list.items.is_empty() {
        out("no PolarAgents".to_string());
        return Ok(());
    }

    let mut rows: Vec<[String; 6]> = Vec::new();
    let mut notes: Vec<(String, Vec<String>)> = Vec::new();
    for agent in &list.items {
        let status = agent.status.clone().unwrap_or_default();
        let namespace = agent.metadata.namespace.clone().unwrap_or_default();
        let name = agent.metadata.name.clone().unwrap_or_default();
        let ending = status
            .last_ending
            .as_ref()
            .map(|e| format!("{:?} exit {} after {}s", e.kind, e.exit_code, e.ran_secs))
            .unwrap_or_else(|| "-".to_string());
        let phase = format!("{:?}", status.phase);
        rows.push([
            namespace.clone(),
            name.clone(),
            phase.clone(),
            status.pod.clone().unwrap_or_else(|| "-".to_string()),
            status.instances_started.to_string(),
            ending,
        ]);
        if phase != "Running" {
            let value = serde_json::to_value(agent)?;
            let lines = explain_agent(&value);
            if !lines.is_empty() {
                notes.push((format!("{namespace}/{name}"), lines));
            }
        }
    }

    let header = [
        "NAMESPACE",
        "AGENT",
        "PHASE",
        "INSTANCE",
        "STARTED",
        "LAST ENDING",
    ];
    let mut widths = header.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.len());
        }
    }
    let line = |cells: &[String; 6]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    out(line(&header.map(str::to_string)));
    for row in &rows {
        out(line(row));
    }
    for (agent, lines) in notes {
        out(format!("\n{agent}:"));
        for l in lines {
            out(format!("  {l}"));
        }
    }
    Ok(())
}
