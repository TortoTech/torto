//! Generate a private signing key or sign a bounded update archive; never logs secrets.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rebook_app_core::updates::{MAX_INSTALLER_BYTES, macos_auth::SignedUpdate};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("generate") {
        use std::io::Write;
        let path = args
            .get(2)
            .ok_or("Pass a private key path outside the repository")?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| "Could not generate signing key")?;
        options.open(path)?.write_all(key.as_ref())?;
        let pair = Ed25519KeyPair::from_pkcs8(key.as_ref()).map_err(|_| "Invalid generated key")?;
        println!(
            "{}",
            pair.public_key()
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        return Ok(());
    }
    let archive = Path::new(args.get(1).ok_or("Pass archive and version")?);
    let version = args.get(2).ok_or("Pass version")?;
    if fs::metadata(archive)?.len() > MAX_INSTALLER_BYTES {
        return Err("Archive too large".into());
    }
    let bytes = fs::read(archive)?;
    let secret = std::env::var("TORTO_MACOS_UPDATE_SIGNING_KEY")
        .map_err(|_| "Configure TORTO_MACOS_UPDATE_SIGNING_KEY")?;
    let key = STANDARD.decode(secret.trim())?;
    let key = Ed25519KeyPair::from_pkcs8(&key).map_err(|_| "Invalid update signing key")?;
    if key.public_key().as_ref()
        != rebook_app_core::updates::macos_auth::public_key().map_err(std::io::Error::other)?
    {
        return Err("Signing key does not match the pinned public key".into());
    }
    let mut sig = SignedUpdate {
        version: version.clone(),
        name: archive
            .file_name()
            .ok_or("Missing archive name")?
            .to_string_lossy()
            .into_owned(),
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        signature: String::new(),
    };
    sig.sign(&key).map_err(std::io::Error::other)?;
    fs::write(
        archive.with_extension("zip.sig.json"),
        serde_json::to_vec(&sig)?,
    )?;
    Ok(())
}
