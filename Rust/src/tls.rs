//! Encrypted phone transfer: Swiftlane's own small certificate authority (CA), the same as the
//! Python version (tls.py), and it reuses the CA the Python version already made, so phones
//! that trust it keep working.
//! - The CA is limited (name constraints) to private and Tailscale IP addresses and one made-up
//!   name, so even if it were stolen it could never vouch for a real website.
//! - Its private key is stored encrypted with Windows DPAPI (tied to this Windows account).
//! - The server certificate is made fresh for this PC's current addresses; its key only ever
//!   exists in memory.

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree,
    IsCa, KeyPair, KeyUsagePurpose, NameConstraints, SanType,
};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;

const ONLY_NAME: &str = "swiftlane.invalid"; // the only DNS name the CA may vouch for (it doesn't exist)
const ALLOWED: &[([u8; 4], [u8; 4])] = &[
    ([10, 0, 0, 0], [255, 0, 0, 0]),
    ([172, 16, 0, 0], [255, 240, 0, 0]),
    ([192, 168, 0, 0], [255, 255, 0, 0]),
    ([100, 64, 0, 0], [255, 192, 0, 0]),
    ([127, 0, 0, 0], [255, 0, 0, 0]),
];

fn allowed(ip: Ipv4Addr) -> bool {
    let o = u32::from(ip);
    ALLOWED.iter().any(|(net, mask)| o & u32::from_be_bytes(*mask) == u32::from_be_bytes(*net))
}

pub struct Authority {
    pub der: Vec<u8>,
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn folder() -> PathBuf {
    crate::config::config_dir().join("tls")
}

impl Authority {
    /// Loads Swiftlane's CA, or makes a new one if there is none (or it runs out within 30 days).
    pub fn load_or_create() -> Result<Self, String> {
        Self::load().or_else(|_| Self::create())
    }

    fn load() -> Result<Self, String> {
        let pem = std::fs::read_to_string(folder().join("swiftlane-ca.pem")).map_err(|e| e.to_string())?;
        let sealed = std::fs::read(folder().join("swiftlane-ca.key.dpapi")).map_err(|e| e.to_string())?;
        Self::from_parts(&pem, &dpapi::unprotect(&sealed)?)
    }

    /// The CA from its certificate (PEM) and private key (PKCS#8 DER).
    fn from_parts(pem: &str, der_key: &[u8]) -> Result<Self, String> {
        let key = KeyPair::try_from(der_key).map_err(|e| e.to_string())?;
        let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).map_err(|e| e.to_string())?;
        let der = parsed.contents.clone();
        let (_, x509) = x509_parser::parse_x509_certificate(&der).map_err(|e| e.to_string())?;
        let ends = x509.validity().not_after.timestamp();
        if ends < time::OffsetDateTime::now_utc().unix_timestamp() + 30 * 86400 {
            return Err("CA runs out soon".into());
        }
        // The same CA (name, key, constraints), so certificates signed now chain to the one phones trust.
        let params = CertificateParams::from_ca_cert_der(&der.clone().into()).map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        Ok(Authority { der, cert, key })
    }

    fn create() -> Result<Self, String> {
        let ca = Self::generate()?;
        std::fs::create_dir_all(folder()).map_err(|e| e.to_string())?;
        std::fs::write(folder().join("swiftlane-ca.key.dpapi"), dpapi::protect(&ca.key.serialize_der())?).map_err(|e| e.to_string())?;
        std::fs::write(folder().join("swiftlane-ca.pem"), ca.cert.pem()).map_err(|e| e.to_string())?;
        Ok(ca)
    }

    /// A new CA in memory (create() saves it).
    fn generate() -> Result<Self, String> {
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
        let mut params = CertificateParams::default();
        let mut name = DistinguishedName::new();
        let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this PC".into());
        name.push(DnType::CommonName, format!("Swiftlane on {pc}").chars().take(64).collect::<String>());
        name.push(DnType::OrganizationName, "Swiftlane (this PC only)");
        params.distinguished_name = name;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let mut permitted: Vec<GeneralSubtree> =
            ALLOWED.iter().map(|(n, m)| GeneralSubtree::IpAddress(CidrSubnet::V4(*n, *m))).collect();
        permitted.push(GeneralSubtree::DnsName(ONLY_NAME.into()));
        params.name_constraints = Some(NameConstraints { permitted_subtrees: permitted, excluded_subtrees: vec![] });
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::minutes(5);
        params.not_after = now + time::Duration::days(3650);
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        Ok(Authority { der: cert.der().to_vec(), cert, key })
    }

    /// First 96 bits of the CA's SHA-256, grouped, to compare by eye.
    pub fn fingerprint(&self) -> String {
        let h = hex::encode_upper(Sha256::digest(&self.der));
        h.as_bytes()[..24].chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
    }

    /// iPhone profile that installs the CA (the user then turns on full trust in Settings).
    pub fn mobileconfig(&self) -> Vec<u8> {
        use base64::Engine;
        let ident = format!("local.swiftlane.ca.{}", &hex::encode(Sha256::digest(&self.der))[..16]);
        let uid = |s: &str| uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, s.as_bytes()).to_string();
        format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>PayloadContent</key><array><dict>
<key>PayloadCertificateFileName</key><string>Swiftlane.cer</string>
<key>PayloadContent</key><data>{data}</data>
<key>PayloadDescription</key><string>Lets this phone talk to Swiftlane on your PC over an encrypted connection. It can only vouch for home-network addresses, never for websites.</string>
<key>PayloadDisplayName</key><string>Swiftlane (your PC)</string>
<key>PayloadIdentifier</key><string>{ident}.cert</string>
<key>PayloadType</key><string>com.apple.security.root</string>
<key>PayloadUUID</key><string>{u1}</string>
<key>PayloadVersion</key><integer>1</integer>
</dict></array>
<key>PayloadDisplayName</key><string>Swiftlane: encrypted connection to your PC</string>
<key>PayloadIdentifier</key><string>{ident}</string>
<key>PayloadRemovalDisallowed</key><false/>
<key>PayloadType</key><string>Configuration</string>
<key>PayloadUUID</key><string>{u2}</string>
<key>PayloadVersion</key><integer>1</integer>
</dict></plist>"#,
            data = base64::engine::general_purpose::STANDARD.encode(&self.der),
            u1 = uid(&format!("{ident}.cert")),
            u2 = uid(&ident),
        ).into_bytes()
    }

    /// A TLS setup with a fresh certificate for these addresses (private/Tailscale ones only).
    pub fn acceptor(&self, addresses: &[String]) -> Result<tokio_rustls::TlsAcceptor, String> {
        let mut ips: Vec<Ipv4Addr> = addresses.iter().filter_map(|a| a.parse().ok()).filter(|ip| allowed(*ip)).collect();
        ips.push(Ipv4Addr::LOCALHOST);
        ips.sort();
        ips.dedup();
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
        let mut params = CertificateParams::new(vec![ONLY_NAME.to_string()]).map_err(|e| e.to_string())?;
        params.subject_alt_names.extend(ips.iter().map(|ip| SanType::IpAddress(IpAddr::V4(*ip))));
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, ONLY_NAME);
        params.distinguished_name = name;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::minutes(5);
        params.not_after = now + time::Duration::days(397);
        let leaf = params.signed_by(&key, &self.cert, &self.key).map_err(|e| e.to_string())?;
        let chain = vec![leaf.der().clone(), rustls::pki_types::CertificateDer::from(self.der.clone())];
        let private = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(chain, private)
            .map_err(|e| e.to_string())?;
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    }
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN};

    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let desc: Vec<u16> = "Swiftlane".encode_utf16().chain(std::iter::once(0)).collect();
        let ok = unsafe {
            if protect {
                CryptProtectData(&input, desc.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 || out.pbData.is_null() {
            return Err("Windows couldn't unlock Swiftlane's certificate key".into());
        }
        let result = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Ok(result)
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, true)
    }
    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        run(data, false)
    }
}

#[cfg(not(windows))]
mod dpapi {
    pub fn protect(_data: &[u8]) -> Result<Vec<u8>, String> {
        Err("Encrypted phone transfer needs Windows".into())
    }
    pub fn unprotect(_data: &[u8]) -> Result<Vec<u8>, String> {
        Err("Encrypted phone transfer needs Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phone that trusts the CA can connect to this PC's home address over TLS,
    /// and the CA's limits are really in the certificate.
    #[tokio::test]
    async fn phone_trusts_the_server() {
        let ca = Authority::generate().unwrap();
        let acceptor = ca.acceptor(&["192.168.1.24".to_string()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(ca.der.clone())).unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let client = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions().unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let (a, b) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move { acceptor.accept(b).await.map(|_| ()) });
        let name = rustls::pki_types::ServerName::IpAddress("192.168.1.24".parse::<std::net::IpAddr>().unwrap().into());
        let _phone = connector.connect(name, a).await.expect("phone should trust the PC");
        server.await.unwrap().unwrap();

        let (_, x509) = x509_parser::parse_x509_certificate(&ca.der).unwrap();
        let nc = x509.name_constraints().unwrap().expect("CA must be limited to home addresses");
        assert!(nc.critical);
        assert!(x509.is_ca());
    }

    /// The CA the Python version made (and phones already trust) keeps working.
    #[tokio::test]
    async fn reuses_the_python_ca() {
        let pem = include_str!("../tests/fixtures/python-ca.pem");
        let key = include_bytes!("../tests/fixtures/python-ca.key.der");
        let ca = Authority::from_parts(pem, key).expect("Python's CA should load");
        let (_, original) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        assert_eq!(ca.der, original.contents, "the phone must keep seeing the same CA");
        let acceptor = ca.acceptor(&["10.0.0.7".to_string()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(original.contents.clone())).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let (a, b) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move { acceptor.accept(b).await.map(|_| ()) });
        let name = rustls::pki_types::ServerName::IpAddress("10.0.0.7".parse::<std::net::IpAddr>().unwrap().into());
        let _phone = tokio_rustls::TlsConnector::from(Arc::new(client)).connect(name, a).await.expect("phone should trust the PC");
        server.await.unwrap().unwrap();
    }

    #[test]
    fn only_home_addresses() {
        assert!(super::allowed("192.168.1.4".parse().unwrap()));
        assert!(super::allowed("100.100.1.1".parse().unwrap()));
        assert!(!super::allowed("8.8.8.8".parse().unwrap()));
        assert!(!super::allowed("172.32.0.1".parse().unwrap()));
    }
}
