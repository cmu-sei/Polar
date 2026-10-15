//! Certificates for tests, here and in crates that verify signed documents, minted with rcgen: a CA,
//! optional intermediates, and signers of either key type with the subject and SAN entries cert-issuer
//! puts in a user's certificate. Behind the `testing` feature so nothing ships with it.

use rcgen::{
    BasicConstraints, CertificateParams, DnType, DnValue, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, SanType, SubjectPublicKeyInfo, date_time_ymd,
};

/// The moment the tests check at.
pub fn now() -> i64 {
    date_time_ymd(2026, 10, 11).unix_timestamp()
}

pub struct Authority {
    pub issuer: Issuer<'static, KeyPair>,
    pub pem: String,
}

pub fn root(name: &str) -> Authority {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.not_before = date_time_ymd(2026, 1, 1);
    params.not_after = date_time_ymd(2100, 1, 1);
    let key = KeyPair::generate().unwrap();
    let pem = params.self_signed(&key).unwrap().pem();
    Authority {
        issuer: Issuer::new(params, key),
        pem,
    }
}

pub fn intermediate(parent: &Authority, name: &str, not_after_year: i32) -> Authority {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.not_before = date_time_ymd(2026, 1, 1);
    params.not_after = date_time_ymd(not_after_year, 1, 1);
    let key = KeyPair::generate().unwrap();
    let pem = params.signed_by(&key, &parent.issuer).unwrap().pem();
    Authority {
        issuer: Issuer::new(params, key),
        pem,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    P256,
    Ed25519,
}

pub struct Spec {
    pub kind: Kind,
    pub username: &'static str,
    pub email: &'static str,
    pub identity: &'static str,
    pub capabilities: Vec<&'static str>,
    pub key_usages: Vec<KeyUsagePurpose>,
    pub not_before_year: i32,
    pub not_after_year: i32,
    pub is_ca: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            kind: Kind::P256,
            username: "alice",
            email: "alice@example.org",
            identity: "d983a3d2-9b61-41c5-88a0-d123fb50ba33",
            capabilities: vec!["c:update-configuration"],
            key_usages: vec![KeyUsagePurpose::DigitalSignature],
            not_before_year: 2026,
            not_after_year: 2030,
            is_ca: false,
        }
    }
}

pub struct Signer {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn signer(issuer: &Authority, spec: Spec) -> Signer {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(
        DnType::CustomDnType(vec![0, 9, 2342, 19200300, 100, 1, 1]),
        DnValue::Utf8String(spec.username.into()),
    );
    params.distinguished_name.push(
        DnType::CustomDnType(vec![0, 9, 2342, 19200300, 100, 1, 3]),
        DnValue::Ia5String(spec.email.try_into().unwrap()),
    );
    params.subject_alt_names = std::iter::once(SanType::DnsName(spec.identity.try_into().unwrap()))
        .chain(
            spec.capabilities
                .iter()
                .map(|c| SanType::URI((*c).try_into().unwrap())),
        )
        .collect();
    params.key_usages = spec.key_usages;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.not_before = date_time_ymd(spec.not_before_year, 1, 1);
    params.not_after = date_time_ymd(spec.not_after_year, 1, 1);
    if spec.is_ca {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    }
    let key = match spec.kind {
        Kind::P256 => KeyPair::generate().unwrap(),
        Kind::Ed25519 => KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap(),
    };
    let cert_pem = params.signed_by(&key, &issuer.issuer).unwrap().pem();
    Signer {
        cert_pem,
        key_pem: key.serialize_pem(),
    }
}

/// An RSA public key (2048 bits, made once for these tests; its private half was thrown away), which is what an
/// agent's escrow certificate holds.
pub const ESCROW_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAmLcTDbxBxdnisvVTSES2\nal7BZH7yBefiUNjdF8rBv7maHUD+9lQXft9aBi98hx+d9hexbdOOM4irqh43kDSF\nxl0kLNTjV5295UbB9fCGsfPY1yQmk4WwT+ZTv4C1yuuGSBssd9ZA7ih2/+oEDT7o\nqn9AeLXRajJkK8y+Dg+5KrQ0uicZ0LhgvRA6VFcfuO0t1ylBzt0UwLM+KCllPmNS\n9jJgZD2/EuewfyqKkqmos99LPxdP1/dOIqN3yZdALd2sE7wM3+ns1/zmFqnUpm6H\nkouqNpfq5Rr22guZyz4ULuxVJ0+KZNGqcz7Lfc8FCR3tGETJfd3a+4dsK35Y55Ic\nRwIDAQAB\n-----END PUBLIC KEY-----\n";

pub struct EscrowSpec {
    /// The DNS name the certificate carries: the agent's normalized identity.
    pub identity: &'static str,
    pub key_usages: Vec<KeyUsagePurpose>,
    pub not_before_year: i32,
    pub not_after_year: i32,
    pub is_ca: bool,
}

impl Default for EscrowSpec {
    fn default() -> Self {
        EscrowSpec {
            identity: "git-repo-observer.polar.serviceaccount.cluster.local",
            key_usages: vec![KeyUsagePurpose::KeyEncipherment],
            not_before_year: 2026,
            not_after_year: 2030,
            is_ca: false,
        }
    }
}

/// An agent's escrow certificate as cert-issuer makes one (`CertType::Service`): the agent's identity as the DNS
/// name, `keyEncipherment` and nothing else, over an RSA key, signed by `issuer`.
pub fn escrow_certificate(issuer: &Authority, spec: EscrowSpec) -> String {
    escrow_certificate_for(issuer, spec, ESCROW_PUBLIC_KEY_PEM)
}

/// The same, over an RSA public key of the caller's, for a test that has to open what was encrypted to it.
pub fn escrow_certificate_for(
    issuer: &Authority,
    spec: EscrowSpec,
    public_key_pem: &str,
) -> String {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.subject_alt_names = vec![SanType::DnsName(spec.identity.try_into().unwrap())];
    params.key_usages = spec.key_usages;
    params.not_before = date_time_ymd(spec.not_before_year, 1, 1);
    params.not_after = date_time_ymd(spec.not_after_year, 1, 1);
    if spec.is_ca {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    }
    let subject = SubjectPublicKeyInfo::from_pem(public_key_pem).unwrap();
    params.signed_by(&subject, &issuer.issuer).unwrap().pem()
}
