// The first policy of a cluster (docs/architecture/credentials-policy.md, "The first policy").
//
// An identity may retrieve a secret it escrowed itself. The owner is the ledger's record of who stored the
// secret; no document can set it.
permit (principal, action == Action::"retrieve", resource)
when { resource.owner == principal };

// Incident freeze: a secret labelled frozen is retrievable by nobody, whatever else permits it.
forbid (principal, action, resource)
when { resource has frozen && resource.frozen };
