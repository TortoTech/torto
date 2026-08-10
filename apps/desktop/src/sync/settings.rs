use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use keyring::Entry;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::persistence::write_json_atomic;

use super::SyncResult;

const SETTINGS_VERSION: u32 = 2;
const FIRST_SUPPORTED_SETTINGS_VERSION: u32 = 1;
const SETTINGS_FILE: &str = "webdav-sync.json";
const CREDENTIAL_SERVICE: &str = "Rebook WebDAV";
pub(crate) const SYNC_INTERVAL_OPTIONS: [u32; 4] = [10, 30, 60, 180];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CloudProviderKind {
    Jianguoyun,
    InfiniCloud,
    Koofr,
    HiDrive,
    YandexDisk,
    #[default]
    Custom,
}

impl CloudProviderKind {
    pub(crate) const ALL: [Self; 6] = [
        Self::Jianguoyun,
        Self::InfiniCloud,
        Self::Koofr,
        Self::HiDrive,
        Self::YandexDisk,
        Self::Custom,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Jianguoyun => "坚果云",
            Self::InfiniCloud => "InfiniCLOUD",
            Self::Koofr => "Koofr",
            Self::HiDrive => "STRATO HiDrive",
            Self::YandexDisk => "Yandex Disk",
            Self::Custom => "Custom",
        }
    }

    const fn base_url(self) -> Option<&'static str> {
        match self {
            Self::Jianguoyun => Some("https://dav.jianguoyun.com/dav"),
            Self::InfiniCloud => Some("https://webdav.infini-cloud.net"),
            Self::Koofr => Some("https://app.koofr.net/dav/Koofr"),
            Self::HiDrive => Some("https://webdav.hidrive.strato.com"),
            Self::YandexDisk => Some("https://webdav.yandex.com"),
            Self::Custom => None,
        }
    }

    pub(crate) const fn credential_url(self) -> &'static str {
        match self {
            Self::Jianguoyun => "https://www.jianguoyun.com/s/downloads",
            Self::InfiniCloud => "https://infini-cloud.net/en/",
            Self::Koofr => "https://app.koofr.net",
            Self::HiDrive => "https://www.strato.de/",
            Self::YandexDisk => "https://id.yandex.com/security/app-passwords",
            Self::Custom => "https://tortotech.github.io/guides/cloud-storage/",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SyncSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: CloudProviderKind,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub username: String,
    pub device_id: String,
    pub device_name: String,
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u32,
}

#[derive(Serialize, Deserialize)]
struct StoredSettings {
    version: u32,
    settings: SyncSettings,
}

impl SyncSettings {
    pub(crate) fn load_default() -> SyncResult<Self> {
        let path = settings_path()?;
        let mut settings = if path.exists() {
            let stored: StoredSettings = serde_json::from_slice(&fs::read(&path)?)?;
            if !(FIRST_SUPPORTED_SETTINGS_VERSION..=SETTINGS_VERSION).contains(&stored.version) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("不支持的 WebDAV 同步设置版本：{}", stored.version),
                )
                .into());
            }
            let mut settings = stored.settings;
            settings.migrate_from_version(stored.version);
            settings
        } else {
            Self::new_device()
        };
        settings.normalize();
        if !path.exists() {
            settings.save_default()?;
        }
        Ok(settings)
    }

    pub(crate) fn new_device() -> Self {
        Self {
            enabled: false,
            provider: CloudProviderKind::Jianguoyun,
            base_url: CloudProviderKind::Jianguoyun
                .base_url()
                .unwrap_or_default()
                .into(),
            username: String::new(),
            device_id: Uuid::new_v4().to_string(),
            device_name: std::env::var("COMPUTERNAME")
                .ok()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Torto Desktop".into()),
            interval_minutes: default_interval_minutes(),
        }
    }

    pub(crate) fn normalize(&mut self) {
        if let Some(base_url) = self.provider.base_url() {
            self.base_url = base_url.into();
        }
        self.base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        self.username = self.username.trim().to_owned();
        self.device_name = self.device_name.trim().to_owned();
        if self.device_name.is_empty() {
            self.device_name = "Torto Desktop".into();
        }
        self.interval_minutes = nearest_interval(self.interval_minutes);
    }

    pub(crate) fn select_provider(&mut self, provider: CloudProviderKind) {
        self.provider = provider;
        if let Some(base_url) = provider.base_url() {
            self.base_url = base_url.into();
        }
    }

    fn migrate_from_version(&mut self, version: u32) {
        if version < 2
            && self.provider == CloudProviderKind::Custom
            && self.base_url.trim().is_empty()
        {
            self.select_provider(CloudProviderKind::Jianguoyun);
        }
    }

    pub(crate) fn validate(&self) -> SyncResult<()> {
        if self.base_url.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "请输入 WebDAV 地址").into());
        }
        if self.username.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "请输入 WebDAV 用户名").into());
        }
        if Uuid::parse_str(&self.device_id).is_err() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "同步设备标识无效").into());
        }
        let url = Url::parse(&self.base_url)?;
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !local {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebDAV 同步默认只允许 HTTPS 地址",
            )
            .into());
        }
        Ok(())
    }

    pub(crate) fn save_default(&self) -> SyncResult<()> {
        let path = settings_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "同步设置路径没有父目录"))?;
        fs::create_dir_all(parent)?;
        let stored = StoredSettings {
            version: SETTINGS_VERSION,
            settings: self.clone(),
        };
        write_json_atomic(&path, &stored)?;
        Ok(())
    }

    pub(crate) fn load_password(&self) -> SyncResult<String> {
        match credential_entry(&self.device_id)?.get_password() {
            Ok(password) => Ok(password),
            Err(keyring::Error::NoEntry) => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn save_password(&self, password: &str) -> SyncResult<()> {
        let entry = credential_entry(&self.device_id)?;
        if password.is_empty() {
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error.into()),
            }
        } else {
            entry.set_password(password)?;
            Ok(())
        }
    }
}

fn default_interval_minutes() -> u32 {
    30
}

fn nearest_interval(value: u32) -> u32 {
    SYNC_INTERVAL_OPTIONS
        .into_iter()
        .min_by_key(|candidate| candidate.abs_diff(value))
        .unwrap_or_else(default_interval_minutes)
}

fn credential_entry(device_id: &str) -> SyncResult<Entry> {
    Ok(Entry::new(CREDENTIAL_SERVICE, device_id)?)
}

fn settings_path() -> io::Result<PathBuf> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定 WebDAV 同步设置目录"))?;
    Ok(project.config_dir().join(SETTINGS_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_keeps_device_identity_and_cleans_endpoint() {
        let mut settings = SyncSettings::new_device();
        settings.provider = CloudProviderKind::Custom;
        settings.base_url = " https://dav.example.test/books/// ".into();
        settings.username = " chris ".into();
        settings.device_name = " ".into();
        settings.interval_minutes = 0;
        let device_id = settings.device_id.clone();

        settings.normalize();

        assert_eq!(settings.base_url, "https://dav.example.test/books");
        assert_eq!(settings.username, "chris");
        assert_eq!(settings.device_name, "Torto Desktop");
        assert_eq!(settings.interval_minutes, 10);
        assert_eq!(settings.device_id, device_id);
        settings.validate().unwrap();
    }

    #[test]
    fn interval_is_normalized_to_a_supported_choice() {
        let mut settings = SyncSettings::new_device();
        assert_eq!(settings.interval_minutes, 30);

        settings.interval_minutes = 52;
        settings.normalize();

        assert_eq!(settings.interval_minutes, 60);
    }

    #[test]
    fn rejects_plain_http_for_remote_hosts() {
        let mut settings = SyncSettings::new_device();
        settings.provider = CloudProviderKind::Custom;
        settings.base_url = "http://dav.example.test".into();
        settings.username = "reader".into();
        assert!(settings.validate().is_err());

        settings.base_url = "http://127.0.0.1:9080".into();
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn new_devices_default_to_jianguoyun() {
        let settings = SyncSettings::new_device();

        assert_eq!(settings.provider, CloudProviderKind::Jianguoyun);
        assert_eq!(settings.base_url, "https://dav.jianguoyun.com/dav");
    }

    #[test]
    fn legacy_custom_endpoint_is_not_replaced_by_a_preset() {
        let json = r#"{
            "enabled": true,
            "base_url": "https://dav.example.test/books",
            "username": "reader",
            "device_id": "d6e21c7d-6f6b-40db-a87c-bef85c12fa47",
            "device_name": "Legacy device",
            "interval_minutes": 30
        }"#;
        let mut settings: SyncSettings = serde_json::from_str(json).unwrap();

        settings.normalize();

        assert_eq!(settings.provider, CloudProviderKind::Custom);
        assert_eq!(settings.base_url, "https://dav.example.test/books");
    }

    #[test]
    fn legacy_empty_endpoint_migrates_to_jianguoyun() {
        let json = r#"{
            "version": 1,
            "settings": {
                "enabled": false,
                "base_url": "",
                "username": "",
                "device_id": "d6e21c7d-6f6b-40db-a87c-bef85c12fa47",
                "device_name": "Legacy device",
                "interval_minutes": 30
            }
        }"#;
        let stored: StoredSettings = serde_json::from_str(json).unwrap();
        let mut settings = stored.settings;

        settings.migrate_from_version(stored.version);

        assert_eq!(settings.provider, CloudProviderKind::Jianguoyun);
        assert_eq!(settings.base_url, "https://dav.jianguoyun.com/dav");
    }
}
