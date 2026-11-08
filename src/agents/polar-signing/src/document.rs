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

//! The two-field document, and its signature block.
//!
//! ```text
//! { payload = { ... everything that has an effect ... }
//! , signature = { algorithm = "ecdsa-p256-sha256", value = "<base64>", certificate_chain = [ "<PEM>", ... ] }
//! }
//! ```
//!
//! Exactly these two fields, and exactly these three in the signature block: a field the signer did not
//! cover, and the verifier would ignore, is a way to carry something unsigned, so a document that has one
//! is refused.

use crate::canonical::{self, CanonicalError};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

/// Prepended to the canonical payload before signing, so a signature made for a configuration document
/// cannot be presented as a signature over anything else, nor the reverse.
pub const DOMAIN: &[u8] = Domain::CONFIGURATION.line();

/// What a signature is for: one line, prepended to the canonical payload before signing. Configuration documents
/// are signed under [`Domain::CONFIGURATION`]; anything else this system signs names its own, so a signature made
/// for one purpose does not verify for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Domain(&'static [u8]);

impl Domain {
    pub const CONFIGURATION: Domain = Domain::new(b"polar-config-document-v1\n");

    /// `line` is one line: something, then a newline, and no other newline. Then no domain's input is a prefix of
    /// another's, since a canonical payload starts with `{`. Checked when it is made, which for a constant is at
    /// compile time.
    pub const fn new(line: &'static [u8]) -> Self {
        let last = line.len().wrapping_sub(1);
        assert!(
            line.len() > 1 && line[last] == b'\n',
            "a domain is one line, ending in a newline"
        );
        let mut i = 0;
        while i < last {
            assert!(line[i] != b'\n', "a domain is one line, with one newline");
            i += 1;
        }
        Domain(line)
    }

    pub const fn line(self) -> &'static [u8] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Ed25519,
    EcdsaP256Sha256,
}

impl Algorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Ed25519 => "ed25519",
            Algorithm::EcdsaP256Sha256 => "ecdsa-p256-sha256",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "ed25519" => Some(Algorithm::Ed25519),
            "ecdsa-p256-sha256" => Some(Algorithm::EcdsaP256Sha256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureBlock {
    pub algorithm: Algorithm,
    pub value: Vec<u8>,
    /// PEM certificates, the signer's first, then whatever links it to the pinned CA.
    pub certificate_chain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub payload: Value,
    pub signature: SignatureBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("a configuration document is an object")]
    NotAnObject,
    #[error("a configuration document has a `{0}` field")]
    Missing(&'static str),
    #[error("a configuration document has exactly `payload` and `signature`, not `{0}`")]
    UnexpectedField(String),
    #[error("`payload` is an object")]
    PayloadNotAnObject,
    #[error("`signature` is an object")]
    SignatureNotAnObject,
    #[error("the signature has `{0}`")]
    SignatureMissing(&'static str),
    #[error("the signature has exactly `algorithm`, `value` and `certificate_chain`, not `{0}`")]
    SignatureUnexpectedField(String),
    #[error("`{0}` is not a signature algorithm this system accepts")]
    UnknownAlgorithm(String),
    #[error("the signature value is not base64")]
    ValueNotBase64,
    #[error("`certificate_chain` is a non-empty list of PEM strings")]
    BadChain,
}

impl Document {
    /// Reads a document from the JSON a rendered Dhall file evaluates to.
    pub fn from_value(value: &Value) -> Result<Self, LayoutError> {
        let object = value.as_object().ok_or(LayoutError::NotAnObject)?;
        exactly(object, &["payload", "signature"]).map_err(|e| match e {
            Mismatch::Missing(name) => LayoutError::Missing(name),
            Mismatch::Extra(name) => LayoutError::UnexpectedField(name),
        })?;
        let payload = object["payload"].clone();
        if !payload.is_object() {
            return Err(LayoutError::PayloadNotAnObject);
        }
        let block = object["signature"]
            .as_object()
            .ok_or(LayoutError::SignatureNotAnObject)?;
        exactly(block, &["algorithm", "value", "certificate_chain"]).map_err(|e| match e {
            Mismatch::Missing(name) => LayoutError::SignatureMissing(name),
            Mismatch::Extra(name) => LayoutError::SignatureUnexpectedField(name),
        })?;

        let name = block["algorithm"]
            .as_str()
            .ok_or_else(|| LayoutError::UnknownAlgorithm(block["algorithm"].to_string()))?;
        let algorithm = Algorithm::parse(name)
            .ok_or_else(|| LayoutError::UnknownAlgorithm(name.to_string()))?;
        let value = block["value"]
            .as_str()
            .and_then(|text| STANDARD.decode(text).ok())
            .ok_or(LayoutError::ValueNotBase64)?;
        let certificate_chain: Vec<String> = block["certificate_chain"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .ok_or(LayoutError::BadChain)?;
        if certificate_chain.is_empty() {
            return Err(LayoutError::BadChain);
        }
        Ok(Document {
            payload,
            signature: SignatureBlock {
                algorithm,
                value,
                certificate_chain,
            },
        })
    }

    /// The JSON form, which is what a Dhall file renders to and what travels.
    pub fn to_value(&self) -> Value {
        json!({
            "payload": self.payload,
            "signature": {
                "algorithm": self.signature.algorithm.as_str(),
                "value": STANDARD.encode(&self.signature.value),
                "certificate_chain": self.signature.certificate_chain,
            }
        })
    }

    /// The bytes the signature covers, as a configuration document.
    pub fn signing_input(&self) -> Result<Vec<u8>, CanonicalError> {
        signing_input(&self.payload)
    }
}

/// The bytes a configuration document's signature over `payload` covers.
pub fn signing_input(payload: &Value) -> Result<Vec<u8>, CanonicalError> {
    signing_input_in(Domain::CONFIGURATION, payload)
}

/// The bytes a signature over `payload` for `domain` covers: the domain line, then the canonical encoding.
pub fn signing_input_in(domain: Domain, payload: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut bytes = domain.line().to_vec();
    bytes.extend(canonical::encode(payload)?);
    Ok(bytes)
}

enum Mismatch {
    Missing(&'static str),
    Extra(String),
}

fn exactly(object: &Map<String, Value>, names: &[&'static str]) -> Result<(), Mismatch> {
    for name in names {
        if !object.contains_key(*name) {
            return Err(Mismatch::Missing(name));
        }
    }
    if let Some(extra) = object.keys().find(|key| !names.contains(&key.as_str())) {
        return Err(Mismatch::Extra(extra.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Value {
        json!({
            "payload": {"agent": "a", "version": 1},
            "signature": {
                "algorithm": "ed25519",
                "value": STANDARD.encode([1u8, 2, 3]),
                "certificate_chain": ["-----BEGIN CERTIFICATE-----"]
            }
        })
    }

    #[test]
    fn a_well_formed_document_reads_and_writes_back_the_same() {
        let document = Document::from_value(&good()).unwrap();
        assert_eq!(document.signature.algorithm, Algorithm::Ed25519);
        assert_eq!(document.signature.value, vec![1, 2, 3]);
        assert_eq!(document.to_value(), good());
    }

    #[test]
    fn a_third_field_is_refused_because_nothing_unsigned_may_ride_along() {
        let mut value = good();
        value["owner"] = json!("someone");
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::UnexpectedField("owner".into()))
        );
    }

    #[test]
    fn a_missing_field_is_refused() {
        let mut value = good();
        value.as_object_mut().unwrap().remove("signature");
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::Missing("signature"))
        );
        let mut value = good();
        value.as_object_mut().unwrap().remove("payload");
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::Missing("payload"))
        );
    }

    #[test]
    fn the_payload_and_the_document_are_objects() {
        let mut value = good();
        value["payload"] = json!([1]);
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::PayloadNotAnObject)
        );
        assert_eq!(
            Document::from_value(&json!([1])),
            Err(LayoutError::NotAnObject)
        );
    }

    #[test]
    fn the_signature_block_has_exactly_its_three_fields() {
        let mut value = good();
        value["signature"]["note"] = json!("x");
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::SignatureUnexpectedField("note".into()))
        );
        let mut value = good();
        value["signature"].as_object_mut().unwrap().remove("value");
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::SignatureMissing("value"))
        );
    }

    #[test]
    fn an_algorithm_this_system_does_not_accept_is_refused() {
        for name in ["rsa-sha256", "none", "ES256", ""] {
            let mut value = good();
            value["signature"]["algorithm"] = json!(name);
            assert_eq!(
                Document::from_value(&value),
                Err(LayoutError::UnknownAlgorithm(name.into())),
                "{name}"
            );
        }
    }

    #[test]
    fn a_value_that_is_not_base64_and_a_bad_chain_are_refused() {
        let mut value = good();
        value["signature"]["value"] = json!("not base64!!");
        assert_eq!(
            Document::from_value(&value),
            Err(LayoutError::ValueNotBase64)
        );
        for chain in [json!([]), json!("pem"), json!([1]), json!(null)] {
            let mut value = good();
            value["signature"]["certificate_chain"] = chain;
            assert_eq!(Document::from_value(&value), Err(LayoutError::BadChain));
        }
    }

    #[test]
    fn the_signing_input_starts_with_the_domain_line_and_ignores_the_signature() {
        let document = Document::from_value(&good()).unwrap();
        let input = document.signing_input().unwrap();
        assert!(input.starts_with(DOMAIN));
        assert_eq!(&input[DOMAIN.len()..], br#"{"agent":"a","version":1}"#);

        // Changing only the signature block does not change what is signed.
        let mut other = good();
        other["signature"]["value"] = json!(STANDARD.encode([9u8]));
        assert_eq!(
            Document::from_value(&other)
                .unwrap()
                .signing_input()
                .unwrap(),
            input
        );
    }
}
