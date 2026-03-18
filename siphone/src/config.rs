//! Configuration file support for sipr.
//!
//! Loads settings from `~/.config/sipr/config.json` (XDG) or `~/.sipr.json` (fallback).
//! CLI flags always override config file values.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Persistent configuration for sipr.
///
/// All fields are optional — the config file can be sparse.
/// Missing fields use the application's built-in defaults.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SiprConfig {
    /// Default SIP username
    pub user: Option<String>,
    /// Default SIP server address
    pub server: Option<String>,
    /// Default SIP password (stored in plaintext)
    pub password: Option<String>,
    /// Default audio codec: "pcmu", "pcma", or "opus"
    pub codec: Option<rtp_core::CodecType>,
    /// Default audio input device name or index
    pub input_device: Option<String>,
    /// Default audio output device name or index
    pub output_device: Option<String>,
    /// Default local SIP port (0 = OS-assigned)
    pub port: Option<u16>,
    /// Disable colored output
    pub no_color: Option<bool>,
    /// Enable SIP sniffing by default during calls
    pub sniff: Option<bool>,
    /// Default path for call recordings
    pub record_path: Option<String>,
    /// Maximum number of command history entries to keep in ~/.sipr.history
    pub max_history: Option<usize>,
    /// Speed dial slots (0-9) mapped to SIP URIs
    pub speed_dials: Option<BTreeMap<String, String>>,
}

impl SiprConfig {
    /// Load config from the first file found, or return defaults.
    pub fn load() -> Self {
        for path in Self::config_paths() {
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(config) => return config,
                        Err(e) => {
                            eprintln!("Warning: invalid config at {}: {}", path.display(), e);
                            return Self::default();
                        }
                    },
                    Err(e) => {
                        eprintln!("Warning: could not read {}: {}", path.display(), e);
                    }
                }
            }
        }
        Self::default()
    }

    /// Return candidate config file paths in priority order.
    pub fn config_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(config_dir) = dirs::config_dir() {
            paths.push(config_dir.join("sipr").join("config.json"));
        }
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".sipr.json"));
        }
        paths
    }

    /// Return the preferred path for creating a new config file.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|d| d.join("sipr").join("config.json"))
            .unwrap_or_else(|| PathBuf::from(".sipr.json"))
    }

    /// Generate a template config as a pretty-printed JSON string.
    pub fn template() -> String {
        let example = SiprConfig {
            user: None,
            server: None,
            password: None,
            codec: Some(rtp_core::CodecType::Pcmu),
            input_device: None,
            output_device: None,
            port: Some(0),
            no_color: Some(false),
            sniff: Some(false),
            record_path: None,
            max_history: Some(1000),
            speed_dials: Some(BTreeMap::from([
                ("1".to_string(), "sip:alice@example.com".to_string()),
                ("2".to_string(), "sip:bob@example.com".to_string()),
            ])),
        };
        serde_json::to_string_pretty(&example).unwrap()
    }

    /// Find which config file is currently active (first that exists).
    pub fn active_path() -> Option<PathBuf> {
        Self::config_paths().into_iter().find(|p| p.exists())
    }

    /// Save config to active path if present, otherwise default path.
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = Self::active_path().unwrap_or_else(Self::default_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        std::fs::write(&path, json)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let cfg = SiprConfig::default();
        assert!(cfg.user.is_none());
        assert!(cfg.server.is_none());
        assert!(cfg.codec.is_none());
        assert!(cfg.port.is_none());
    }

    #[test]
    fn test_parse_minimal_json() {
        let json = r#"{"user": "alice"}"#;
        let cfg: SiprConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.user, Some("alice".into()));
        assert!(cfg.server.is_none());
        assert!(cfg.codec.is_none());
    }

    #[test]
    fn test_parse_full_json() {
        let json = r#"{
            "user": "bob",
            "server": "sip.example.com",
            "password": "secret",
            "codec": "pcma",
            "input_device": "USB Mic",
            "output_device": "default",
            "port": 5061,
            "no_color": true,
            "sniff": true,
            "record_path": "/tmp/calls",
            "max_history": 500,
            "speed_dials": {
                "1": "sip:alice@example.com",
                "2": "sip:bob@example.com"
            }
        }"#;
        let cfg: SiprConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.user, Some("bob".into()));
        assert_eq!(cfg.server, Some("sip.example.com".into()));
        assert_eq!(cfg.codec, Some(rtp_core::CodecType::Pcma));
        assert_eq!(cfg.port, Some(5061));
        assert_eq!(cfg.no_color, Some(true));
        assert_eq!(cfg.sniff, Some(true));
        assert_eq!(cfg.max_history, Some(500));
        assert_eq!(
            cfg.speed_dials
                .as_ref()
                .and_then(|m| m.get("1"))
                .map(|s| s.as_str()),
            Some("sip:alice@example.com")
        );
    }

    #[test]
    fn test_parse_empty_json() {
        let json = "{}";
        let cfg: SiprConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.user.is_none());
        assert!(cfg.codec.is_none());
    }

    #[test]
    fn test_template_is_valid_json() {
        let template = SiprConfig::template();
        let _: SiprConfig = serde_json::from_str(&template).unwrap();
        assert!(template.contains("\"speed_dials\""));
    }

    #[test]
    fn test_codec_serialization() {
        let json = r#"{"codec": "opus"}"#;
        let cfg: SiprConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.codec, Some(rtp_core::CodecType::Opus));

        let serialized = serde_json::to_string(&cfg).unwrap();
        assert!(serialized.contains("\"opus\""));
    }
}
