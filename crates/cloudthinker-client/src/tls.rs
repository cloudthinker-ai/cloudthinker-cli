use std::sync::OnceLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls_native_certs::CertificateResult;
use sha2::{Digest, Sha256};

use crate::error::{CtError, CtResult};

const DISTRUSTED_ROOTS_SHA256: [[u8; 32]; 4] = [
    [
        0x86, 0xa1, 0xec, 0xba, 0x08, 0x9c, 0x4a, 0x8d, 0x3b, 0xbe, 0x27, 0x34, 0xc6, 0x12, 0xba,
        0x34, 0x1d, 0x81, 0x3e, 0x04, 0x3c, 0xf9, 0xe8, 0xa8, 0x62, 0xcd, 0x5c, 0x57, 0xa3, 0x6b,
        0xbe, 0x6b,
    ],
    [
        0x12, 0x56, 0x09, 0xaa, 0x30, 0x1d, 0xa0, 0xa2, 0x49, 0xb9, 0x7a, 0x82, 0x39, 0xcb, 0x6a,
        0x34, 0x21, 0x6f, 0x44, 0xdc, 0xac, 0x9f, 0x39, 0x54, 0xb1, 0x42, 0x92, 0xf2, 0xe8, 0xc8,
        0x60, 0x8f,
    ],
    [
        0xbc, 0x4d, 0x80, 0x9b, 0x15, 0x18, 0x9d, 0x78, 0xdb, 0x3e, 0x1d, 0x8c, 0xf4, 0xf9, 0x72,
        0x6a, 0x79, 0x5d, 0xa1, 0x64, 0x3c, 0xa5, 0xf1, 0x35, 0x8e, 0x1d, 0xdb, 0x0e, 0xdc, 0x0d,
        0x7e, 0xb3,
    ],
    [
        0xd7, 0xa7, 0xa0, 0xfb, 0x5d, 0x7e, 0x27, 0x31, 0xd7, 0x71, 0xe9, 0x48, 0x4e, 0xbc, 0xde,
        0xf7, 0x1d, 0x5f, 0x0c, 0x3e, 0x0a, 0x29, 0x48, 0x78, 0x2b, 0xc8, 0x3e, 0xe0, 0xea, 0x69,
        0x9e, 0xf4,
    ],
];

static TRUSTED_ROOTS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();

fn roots() -> &'static [Vec<u8>] {
    TRUSTED_ROOTS.get_or_init(|| trusted_roots(rustls_native_certs::load_native_certs()))
}

pub(crate) fn http_client_builder() -> reqwest::ClientBuilder {
    let certificates = roots()
        .iter()
        .filter_map(|der| reqwest::Certificate::from_der(der).ok());
    reqwest::Client::builder().tls_certs_only(certificates)
}

pub fn https_client() -> CtResult<reqwest::Client> {
    http_client_builder()
        .build()
        .map_err(|e| CtError::Transport(format!("http client build: {e}")))
}

pub fn trusted_roots_pem() -> String {
    to_pem(roots())
}

fn to_pem(roots: &[Vec<u8>]) -> String {
    let mut pem = String::new();
    for der in roots {
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        let encoded = STANDARD.encode(der);
        for line in encoded.as_bytes().chunks(64) {
            pem.push_str(&String::from_utf8_lossy(line));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
    }
    pem
}

fn trusted_roots(loaded: CertificateResult) -> Vec<Vec<u8>> {
    loaded
        .certs
        .into_iter()
        .filter(|cert| {
            let fingerprint: [u8; 32] = Sha256::digest(cert.as_ref()).into();
            !DISTRUSTED_ROOTS_SHA256.contains(&fingerprint)
        })
        .filter(|cert| webpki::anchor_from_trusted_cert(cert).is_ok())
        .map(|cert| cert.as_ref().to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_roots_drop_the_leaked_emudhra_chain_and_keep_the_rest() {
        let bundle = [
            include_str!("../tests/fixtures/tls/distrusted-emsign-g3.pem"),
            include_str!("../tests/fixtures/tls/distrusted-emsign-c1.pem"),
            include_str!("../tests/fixtures/tls/distrusted-emsign-c3.pem"),
            include_str!("../tests/fixtures/tls/distrusted-aaa.pem"),
            include_str!("../tests/fixtures/tls/isrg-root-x1.pem"),
        ]
        .concat();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca-certificates.crt");
        std::fs::write(&path, bundle).unwrap();

        let loaded = rustls_native_certs::load_certs_from_paths(Some(&path), None);
        assert_eq!(loaded.certs.len(), 5);
        assert!(
            loaded
                .certs
                .iter()
                .all(|cert| webpki::anchor_from_trusted_cert(cert).is_ok())
        );
        let trusted = trusted_roots(loaded);
        assert_eq!(trusted.len(), 1);

        let exported = dir.path().join("trusted-roots.pem");
        std::fs::write(&exported, to_pem(&trusted)).unwrap();
        let reloaded = rustls_native_certs::load_certs_from_paths(Some(&exported), None);
        assert_eq!(trusted_roots(reloaded), trusted);
    }
}
