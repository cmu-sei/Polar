# ACE Tool
#
# Copyright 2026 Carnegie Mellon University.
#
# NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
# FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
# EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
# PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
# MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
# TRADEMARK, OR COPYRIGHT INFRINGEMENT.
#
# Licensed under a MIT (SEI)-style license, please see license.txt or contact permission@sei.cmu.edu
# for full terms.
#
# [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
# distribution.  Please see Copyright notice for non-US Government use and distribution.
#
# This Software includes and/or makes use of Third-Party Software each subject to its own license.
#
# DM26-0670

# utils/neo4j_client.py
#
# Single source of truth for constructing an mTLS-enabled Neo4j driver.
# Every module that talks to Neo4j should import get_driver() from here
# rather than calling neo4j.GraphDatabase.driver() directly -- Polar's
# Neo4j requires mutual TLS (client presents its own certificate, issued
# by polar-init/cert-issuer, in addition to username/password auth), and
# that configuration should exist in exactly one place.
#
# Environment variables match Polar's existing Rust-agent convention
# (see infra/schema/functions.dhall's makeGraphEnv) rather than
# inventing ACE-specific names, so every Neo4j-touching container in
# the fleet -- Rust or Python -- uses the same vocabulary.
#
#   GRAPH_ENDPOINT     bolt+s://host:port  (the +s is required -- trusted_certificates
#                      and client_certificate have no effect on a bare bolt:// URI)
#   GRAPH_DB           database name (informational; not passed to the driver here --
#                      callers select the database per-session if needed)
#   GRAPH_USER         Neo4j username
#   GRAPH_PASSWORD     Neo4j password
#   GRAPH_CLIENT_CERT  path to this pod's mTLS client certificate (from polar-init)
#   GRAPH_CLIENT_KEY   path to this pod's mTLS client private key (from polar-init)
#   GRAPH_CA_CERT      path to the CA certificate to trust (Polar's private CA)

from __future__ import annotations

import os

from neo4j import GraphDatabase
from neo4j.auth_management import ClientCertificate, ClientCertificateProviders
from neo4j import TrustCustomCAs


def _build_driver():
    uri = os.getenv("GRAPH_ENDPOINT")
    user = os.getenv("GRAPH_USER")
    password = os.getenv("GRAPH_PASSWORD")
    client_cert_path = os.getenv("GRAPH_CLIENT_CERT")
    client_key_path = os.getenv("GRAPH_CLIENT_KEY")
    ca_cert_path = os.getenv("GRAPH_CA_CERT")

    missing = [
        name
        for name, val in [
            ("GRAPH_ENDPOINT", uri),
            ("GRAPH_USER", user),
            ("GRAPH_PASSWORD", password),
        ]
        if not val
    ]
    if missing:
        raise RuntimeError(
            f"Missing required Neo4j environment variable(s): {', '.join(missing)}"
        )

    driver_kwargs = {"auth": (user, password)}

    # The driver rejects "encrypted"/"trusted_certificates"/"ssl_context"
    # as explicit kwargs when the URI itself already carries a +s/+ssc
    # encryption suffix -- those are two different ways of expressing the
    # same setting and the driver refuses to accept both at once. Strip
    # the suffix to the bare scheme and set encrypted explicitly instead,
    # which is what actually lets us also supply our own CA/client cert.
    scheme, _, rest = uri.partition("://")
    bare_scheme = scheme.split("+", 1)[0]
    if scheme != bare_scheme:
        uri = f"{bare_scheme}://{rest}"
        driver_kwargs["encrypted"] = True

    if ca_cert_path:
        driver_kwargs["trusted_certificates"] = TrustCustomCAs(ca_cert_path)

    if client_cert_path and client_key_path:
        cert_provider = ClientCertificateProviders.static(
            ClientCertificate(client_cert_path, client_key_path)
        )
        driver_kwargs["client_certificate"] = cert_provider

    return GraphDatabase.driver(uri, **driver_kwargs)


_driver = None


def get_driver():
    """Return a process-wide singleton Neo4j driver, constructed with mTLS
    on first use. The driver manages its own connection pool internally,
    so sharing one instance across the app is the intended usage pattern
    (not a driver-per-query)."""
    global _driver
    if _driver is None:
        _driver = _build_driver()
    return _driver
