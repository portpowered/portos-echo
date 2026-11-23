//! Reversible Athena settings and package state, from echo-dot-assist (MIT).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Result;

pub const PACKAGES: &[&str] = &[
    "amazon.speech.sim",
    "com.amazon.bishop",
    "com.amazon.device.echoaudioservice",
    "com.amazon.afe.app",
    "com.amazon.alexa.awaservice",
    "com.amazon.ambienthome",
    "com.amazon.alexa.ms.kinex",
    "com.amazon.alexadirectivebrokerservice",
    "com.amazon.alexa.datastore.app",
    "com.amazon.NativeAccessorProxyServices",
    "com.amazon.paladin",
    "com.amazon.whad",
    "com.amazon.d3",
    "com.amazon.a4b.mcc",
    "com.amazon.csm.htmlruntime",
    "com.amazon.device.software.ota",
    "com.amazon.device.software.ota.override",
    "com.amazon.amasetup.service",
    "com.amazon.kindle.otter.oobe",
];

pub const SETTINGS: &[(&str, &str, &str)] = &[
    ("global", "device_provisioned", "1"),
    ("secure", "user_setup_complete", "1"),
    ("system", "screen_off_timeout", "300000"),
    ("global", "bluetooth_on", "0"),
    ("system", "smartconn_status", "enabled"),
];

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageState {
    Default,
    Enabled,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SavedSetting {
    pub namespace: String,
    pub key: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub previous: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    version: u8,
    pub packages: BTreeMap<String, PackageState>,
    pub settings: Vec<SavedSetting>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            version: 1,
            packages: BTreeMap::new(),
            settings: Vec::new(),
        }
    }
}

impl Journal {
    /// # Errors
    /// Rejects invalid, unsupported or non-Athena restore entries.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let journal: Self = serde_json::from_slice(bytes).map_err(|_| "invalid Alexa journal")?;
        journal.validate()?;
        Ok(journal)
    }

    fn validate(&self) -> Result<()> {
        let mut keys = Vec::new();
        if self.version != 1
            || self.packages.keys().any(|name| !PACKAGES.contains(&name.as_str()))
            || self.settings.iter().any(|entry| {
                let key = (entry.namespace.as_str(), entry.key.as_str());
                let invalid = keys.contains(&key)
                    || !SETTINGS.iter().any(|&(ns, name, _)| key == (ns, name))
                    || entry.previous.as_ref().is_some_and(|value| value.contains('\0'));
                keys.push(key);
                invalid
            })
        {
            return Err("invalid Alexa journal");
        }
        Ok(())
    }

    /// # Errors
    /// Rejects invalid entries or an encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| "encode Alexa journal")
    }

    pub fn remember_setting(&mut self, namespace: &str, key: &str, previous: Option<String>) {
        if !self
            .settings
            .iter()
            .any(|entry| entry.namespace == namespace && entry.key == key)
        {
            self.settings.push(SavedSetting {
                namespace: namespace.into(),
                key: key.into(),
                previous,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_preserves_absence_and_rejects_unrelated_settings() {
        let mut journal = Journal::default();
        journal.remember_setting("system", "smartconn_status", None);
        journal.remember_setting("system", "smartconn_status", Some("later".into()));
        let decoded = Journal::decode(&journal.encode().unwrap()).unwrap();
        assert_eq!(decoded.settings[0].previous, None);
        let unrelated =
            br#"{"version":1,"packages":{},"settings":[{"namespace":"global","key":"wifi_on","previous":"1"}]}"#;
        assert!(Journal::decode(unrelated).is_err());
        let missing = br#"{"version":1,"packages":{},"settings":[{"namespace":"system","key":"smartconn_status"}]}"#;
        assert!(Journal::decode(missing).is_err());
    }
}
