//! Publisher authentication independent of Apple signing and download mirrors.
use super::{Platform, UpdateRelease};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{self, Ed25519KeyPair};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedUpdate {
    pub version: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub signature: String,
}
impl SignedUpdate {
    pub fn message(&self) -> Result<String, String> {
        super::AppVersion::parse(&self.version)?;
        if ![Platform::MacosArm64, Platform::MacosX64]
            .iter()
            .any(|p| p.name(&self.version) == self.name)
            || self.size == 0
            || self.size > super::MAX_INSTALLER_BYTES
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("Invalid signed macOS package metadata".into());
        }
        Ok(format!(
            "TORTO-MACOS-UPDATE-V1\n{}\n{}\n{}\n{}\n",
            self.version, self.name, self.size, self.sha256
        ))
    }
    pub fn sign(&mut self, key: &Ed25519KeyPair) -> Result<(), String> {
        self.signature = STANDARD.encode(key.sign(self.message()?.as_bytes()).as_ref());
        Ok(())
    }
    pub fn verify(&self, key: &[u8]) -> Result<(), String> {
        let bytes = STANDARD
            .decode(&self.signature)
            .map_err(|_| "Invalid update signature encoding")?;
        signature::UnparsedPublicKey::new(&signature::ED25519, key)
            .verify(self.message()?.as_bytes(), &bytes)
            .map_err(|_| "The macOS update publisher signature is invalid".into())
    }
}
pub fn public_key() -> Result<Vec<u8>, String> {
    let text = include_str!("../../../../assets/macos/update-public-key.hex").trim();
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid embedded update public key".into());
    }
    (0..32)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
pub fn verify_release(release: &UpdateRelease) -> Result<(), String> {
    if !release.asset.name.ends_with("-macos-arm64.zip")
        && !release.asset.name.ends_with("-macos-x86_64.zip")
    {
        return Ok(());
    }
    let sig = release
        .macos_signature
        .as_ref()
        .ok_or("Missing signed macOS update manifest")?;
    if sig.version != release.version
        || sig.name != release.asset.name
        || sig.size != release.asset.size
        || sig.sha256 != release.asset.sha256
    {
        return Err("Signed macOS manifest does not match the release".into());
    }
    sig.verify(&public_key()?)
}
#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    #[test]
    fn pinned_key_accepts_the_golden_fixture_and_rejects_missing_or_mismatched_metadata() {
        let sig = SignedUpdate {
            version: "0.7.0".into(), name: Platform::MacosArm64.name("0.7.0"), size: 17,
            sha256: "b97d800ba8d0420dc6a0342045a15a97d785bd61d4cf9902d0c334ba32ec188b".into(),
            signature: "MBuJ82GTbhIxqMGGBGjjrctJQfjel5jdgKPe4d/QV8XQxzgbArO0X9I5T/a6u4ykClwgz6FSD3WdVODnYoG2CQ==".into(),
        };
        let mut release = UpdateRelease {
            version: sig.version.clone(),
            notes: String::new(),
            asset: super::super::UpdateAsset {
                name: sig.name.clone(),
                size: sig.size,
                sha256: sig.sha256.clone(),
                github_asset_id: None,
                url: String::new(),
            },
            macos_signature: Some(sig.clone()),
        };
        verify_release(&release).unwrap();
        release.asset.size += 1;
        assert!(verify_release(&release).is_err());
        release.asset.size = sig.size;
        release.macos_signature = None;
        assert!(verify_release(&release).is_err());
    }
    #[test]
    fn signature_binds_version_architecture_size_and_hash() {
        let key = Ed25519KeyPair::from_seed_unchecked(&[19; 32]).unwrap();
        let mut sig = SignedUpdate {
            version: "0.10.0".into(),
            name: Platform::MacosArm64.name("0.10.0"),
            size: 128,
            sha256: "a".repeat(64),
            signature: String::new(),
        };
        sig.sign(&key).unwrap();
        sig.verify(key.public_key().as_ref()).unwrap();
        for field in 0..4 {
            let mut bad = sig.clone();
            match field {
                0 => bad.version = "0.11.0".into(),
                1 => bad.name = Platform::MacosX64.name("0.10.0"),
                2 => bad.size += 1,
                _ => bad.sha256 = "b".repeat(64),
            };
            assert!(bad.verify(key.public_key().as_ref()).is_err());
        }
        assert!(
            sig.verify(
                Ed25519KeyPair::from_seed_unchecked(&[20; 32])
                    .unwrap()
                    .public_key()
                    .as_ref()
            )
            .is_err()
        );
    }
}
