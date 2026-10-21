// ACE Tool
//
// Copyright 2026 Carnegie Mellon University.
//
// NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
// FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
// EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
// PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
// MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
// TRADEMARK, OR COPYRIGHT INFRINGEMENT.
//
// Licensed under a MIT (SEI)-style license, please see license.txt or contact
// permission@sei.cmu.edu for full terms.
//
// [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
// distribution.  Please see Copyright notice for non-US Government use and distribution.
//
// This Software includes and/or makes use of Third-Party Software each subject to its own license.
//
// DM26-0670

// The first policy of a cluster (docs/architecture/credentials-policy.md, "The first policy").
//
// An identity may retrieve a secret it escrowed itself. The owner is the ledger's record of who stored the
// secret; no document can set it.
permit (principal, action == Action::"retrieve", resource)
when { resource has owner && resource.owner == principal };

// Incident freeze: a secret labelled frozen is retrievable by nobody, whatever else permits it.
forbid (principal, action, resource)
when { resource has frozen && resource.frozen };
