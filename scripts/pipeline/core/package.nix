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

# The pipeline core's Nushell modules, gathered as one output (`corePipelineLibFiles`) so that another project's pipeline
# image can carry them and `use core/...` them, as Polar's own ci.nu does.
{ runCommand }:

runCommand "polar-core" {} ''
  mkdir -p $out

  cp ${./mod.nu} $out/mod.nu
  cp ${./cargo.nu} $out/cargo.nu
  cp ${./cassini.nu} $out/cassini.nu
  cp ${./dhall.nu} $out/dhall.nu
  cp ${./events.nu} $out/events.nu
  cp ${./hashing.nu} $out/hashing.nu
  cp ${./logging.nu} $out/logging.nu
  cp ${./oci.nu} $out/oci.nu
  cp ${./sbom.nu} $out/sbom.nu
  cp ${./scanning.nu} $out/scanning.nu
  cp ${./state.nu} $out/state.nu
''
