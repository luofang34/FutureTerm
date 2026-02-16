use rcgen::{CertificateParams, DistinguishedName, DnType};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Get the path to the certificate directory
fn cert_dir() -> Result<PathBuf, String> {
    let home =
        std::env::var("HOME").map_err(|_| "HOME environment variable not set".to_string())?;
    let mut path = PathBuf::from(home);
    path.push("Library");
    path.push("Application Support");
    path.push("FutureTerm");
    path.push("cert");
    Ok(path)
}

/// Ensure TLS certificate exists, generating if needed
/// Returns (cert_pem, key_pem)
pub fn ensure_tls_cert() -> Result<(Vec<u8>, Vec<u8>), String> {
    let dir = cert_dir()?;
    let cert_path = dir.join("localhost.crt");
    let key_path = dir.join("localhost.key");

    // Check if certificate already exists
    if cert_path.exists() && key_path.exists() {
        let cert_pem =
            fs::read(&cert_path).map_err(|e| format!("Failed to read certificate: {}", e))?;
        let key_pem =
            fs::read(&key_path).map_err(|e| format!("Failed to read private key: {}", e))?;
        return Ok((cert_pem, key_pem));
    }

    // Generate new self-signed certificate
    eprintln!("Generating self-signed TLS certificate...");
    let (cert_pem, key_pem) = generate_tls_cert()?;

    // Create directory if it doesn't exist
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create cert directory: {}", e))?;

    // Write certificate and key to disk
    let mut cert_file = fs::File::create(&cert_path)
        .map_err(|e| format!("Failed to create certificate file: {}", e))?;
    cert_file
        .write_all(&cert_pem)
        .map_err(|e| format!("Failed to write certificate: {}", e))?;

    let mut key_file =
        fs::File::create(&key_path).map_err(|e| format!("Failed to create key file: {}", e))?;
    key_file
        .write_all(&key_pem)
        .map_err(|e| format!("Failed to write private key: {}", e))?;

    eprintln!("Certificate generated at: {:?}", cert_path);

    Ok((cert_pem, key_pem))
}

/// Generate a self-signed TLS certificate for localhost
fn generate_tls_cert() -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut params = CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .map_err(|e| format!("Failed to create certificate params: {}", e))?;

    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "FutureTerm Bridge");
    dn.push(DnType::OrganizationName, "FutureTerm");
    params.distinguished_name = dn;

    let key_pair =
        rcgen::KeyPair::generate().map_err(|e| format!("Failed to generate key pair: {}", e))?;
    let key_pem = key_pair.serialize_pem();

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| format!("Failed to generate certificate: {}", e))?;
    let cert_pem = cert.pem();

    Ok((cert_pem.into_bytes(), key_pem.into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_tls_cert() {
        let result = generate_tls_cert();
        assert!(result.is_ok());

        let (cert_pem, key_pem) = result.unwrap();
        assert!(!cert_pem.is_empty());
        assert!(!key_pem.is_empty());

        // Verify it's PEM format
        let cert_str = String::from_utf8_lossy(&cert_pem);
        assert!(cert_str.contains("-----BEGIN CERTIFICATE-----"));
        assert!(cert_str.contains("-----END CERTIFICATE-----"));

        let key_str = String::from_utf8_lossy(&key_pem);
        assert!(key_str.contains("-----BEGIN PRIVATE KEY-----"));
        assert!(key_str.contains("-----END PRIVATE KEY-----"));
    }

    #[test]
    fn test_cert_dir() {
        let result = cert_dir();
        if std::env::var("HOME").is_ok() {
            assert!(result.is_ok());
            let path = result.unwrap();
            assert!(path
                .to_string_lossy()
                .contains("Library/Application Support/FutureTerm/cert"));
        }
    }
}
