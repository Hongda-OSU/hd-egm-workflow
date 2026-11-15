//! App settings (PRD §4): data dir, project page and rule page ids.
//! Never holds the Notion token; that lives in the keychain.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub data_dir: String,
    pub project_page_id: String,
    /// Rule page name -> Notion page id; `null` for pages not written yet.
    pub rule_pages: BTreeMap<String, Option<String>>,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

pub fn settings_path() -> PathBuf {
    home().join("Library/Application Support/hd-egm-workflow/settings.json")
}

impl Settings {
    pub fn load() -> Result<Self> {
        let path = settings_path();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read settings at {}: {e}", path.display()))?;
        Ok(serde_json::from_str(&text)
            .map_err(|e| format!("invalid settings at {}: {e}", path.display()))?)
    }

    pub fn data_dir(&self) -> PathBuf {
        match self.data_dir.strip_prefix("~/") {
            Some(rest) => home().join(rest),
            None => PathBuf::from(&self.data_dir),
        }
    }
}
