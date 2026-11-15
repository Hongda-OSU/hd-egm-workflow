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
    /// Dedicated codex login for goldfish (agent_docs/codex-goldfish.md).
    #[serde(default = "default_codex_home")]
    pub codex_home: String,
    #[serde(default = "default_goldfish_model")]
    pub goldfish_model: String,
    #[serde(default = "default_goldfish_effort")]
    pub goldfish_effort: String,
    #[serde(default = "default_triager_model")]
    pub triager_model: String,
}

fn default_codex_home() -> String {
    "~/Library/Application Support/hd-egm-workflow/codex-home".into()
}

fn default_goldfish_model() -> String {
    "gpt-6.1-sol".into()
}

fn default_goldfish_effort() -> String {
    "medium".into()
}

fn default_triager_model() -> String {
    "claude-opus-5-5".into()
}

fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
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
        expand(&self.data_dir)
    }

    pub fn codex_home(&self) -> PathBuf {
        expand(&self.codex_home)
    }
}
