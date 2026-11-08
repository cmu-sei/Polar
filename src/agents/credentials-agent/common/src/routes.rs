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

//! The routes the credentials agent's observer serves over HTTP. Written once, so the observer's router, its clients and
//! its topic declarations (each publish is caused by one: docs/architecture/authority-model.md, "Decisions") name the
//! same route, and an invariant that names one cannot drift from it.

/// A certificate request, a person's or a tool's.
pub const ISSUE: &str = "/issue";
/// An identity to normalize.
pub const IDENTITY: &str = "/identity";
/// An account claim, both phases.
pub const ACCOUNT: &str = "/account";
/// A signed administrator action.
pub const ADMIN_ACTION: &str = "/admin-action";
/// An agent's public escrow certificate, for someone outside the cluster to encrypt to.
pub const AGENT_ESCROW_KEY: &str = "/agent-escrow-key";
/// A workload opens a session, before it has a certificate.
pub const WORKLOAD_SESSION: &str = "/workload/session";
/// A workload asks for its certificate, its token sealed to the session.
pub const WORKLOAD_ISSUE: &str = "/workload/issue";
