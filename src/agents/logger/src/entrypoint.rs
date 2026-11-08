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

use logger::Durability;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    polar::init_logging("events.logging.supervisor".to_string());
    let _db_path =
        PathBuf::from(std::env::var("LOG_DB_PATH").unwrap_or_else(|_| "./rocksdb".to_string()));
    let _durability = match std::env::var("LOG_DURABILITY").as_deref() {
        Ok("dev") => Durability::DevLoose,
        Ok("audit") => Durability::AuditStrict,
        _ => Durability::AuditStrict,
    };

    // ----------------------
    // Spawn the logging consumer actor
    // ----------------------
    todo!("Spawn logger");
    // ----------------------
    // Block main until shutdown (optional)
    // ----------------------
}

// Every Polar binary allocates with Scudo (docs/architecture/broker-baseline.md, "Decided").
#[global_allocator]
static GLOBAL: polar_scudo::Scudo = polar_scudo::Scudo;
