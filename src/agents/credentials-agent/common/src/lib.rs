/*
   Polar (OSS)

   Copyright 2024 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND,
   EITHER EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS
   FOR PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL.
   CARNEGIE MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM
   PATENT, TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT-style license, please see license.txt or contact permission@sei.cmu.edu for
   full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM24-0470
*/

pub mod events;
pub mod issuance;
pub mod observer;
pub mod identity;
pub mod graph;
pub mod certificate;
pub mod escrow_key;
pub mod escrow_secret;

pub const CREDENTIALS_GRANTS_TOPIC: &str = "credentials-grants";
pub const CREDENTIALS_REVOCATIONS_TOPIC: &str = "credentials-revocations";

pub const CREDENTIALS_ISSUANCE_REQUESTS_TOPIC: &str = "credentials-issuance-requests";
pub const CREDENTIALS_ISSUANCE_REPLIES_TOPIC: &str = "credentials-issuance-replies";

pub const CREDENTIALS_IDENTITY_NORMALIZATION_REQUESTS_TOPIC: &str = "credentials-identity-normalization-requests";
pub const CREDENTIALS_IDENTITY_NORMALIZATION_REPLIES_TOPIC: &str = "credentials-identity-normalization-replies";

pub const CREDENTIALS_ESCROW_KEY_REQUESTS_TOPIC: &str = "credentials-escrow-key-requests";
pub const CREDENTIALS_ESCROW_KEY_REPLIES_TOPIC: &str = "credentials-escrow-key-replies";

pub const CREDENTIALS_CERTIFICATE_REQUESTS_TOPIC: &str = "credentials-certificate-requests";
pub const CREDENTIALS_CERTIFICATE_REPLIES_TOPIC: &str = "credentials-certificate-replies";
