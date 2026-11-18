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
    /// CLI versions the isolation probes passed on (PRD §5); edited by hand
    /// after a re-probe, the app only compares.
    #[serde(default)]
    pub verified_cli_versions: CliVersions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliVersions {
    pub codex: String,
    pub claude: String,
}

impl Default for CliVersions {
    fn default() -> Self {
        Self { codex: "0.169.0".into(), claude: "2.1.292".into() }
    }
}

/// The three model fields the settings dialog edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Models {
    pub goldfish_model: String,
    pub goldfish_effort: String,
    pub triager_model: String,
}

impl Default for Models {
    fn default() -> Self {
        Self {
            goldfish_model: default_goldfish_model(),
            goldfish_effort: default_goldfish_effort(),
            triager_model: default_triager_model(),
        }
    }
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

    pub fn models(&self) -> Models {
        Models {
            goldfish_model: self.goldfish_model.clone(),
            goldfish_effort: self.goldfish_effort.clone(),
            triager_model: self.triager_model.clone(),
        }
    }
}

/// Writes the model fields into settings.json, leaving every other key as
/// it is. A value equal to its default is removed so the file keeps
/// following the default.
pub fn save_models(models: &Models) -> Result<()> {
    let path = settings_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read settings at {}: {e}", path.display()))?;
    let mut v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("invalid settings at {}: {e}", path.display()))?;
    apply_models(&mut v, models)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&v)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn apply_models(v: &mut serde_json::Value, models: &Models) -> Result<()> {
    let obj = v.as_object_mut().ok_or("settings.json is not a JSON object")?;
    let defaults = Models::default();
    for (key, value, default) in [
        ("goldfish_model", &models.goldfish_model, &defaults.goldfish_model),
        ("goldfish_effort", &models.goldfish_effort, &defaults.goldfish_effort),
        ("triager_model", &models.triager_model, &defaults.triager_model),
    ] {
        let value = value.trim();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(format!("{key} must be one word, got {value:?}").into());
        }
        if value == default {
            obj.remove(key);
        } else {
            obj.insert(key.into(), value.into());
        }
    }
    Ok(())
}

/// The version number in `codex --version` ("codex-cli 0.169.0") or
/// `claude --version` ("2.1.292 (Claude Code)") output.
pub fn parse_version(out: &str) -> Option<String> {
    out.split_whitespace()
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.'))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn saving_models_keeps_other_keys_and_drops_defaults() {
        let mut v = json!({"data_dir": "~/x", "rule_pages": {"a": null}, "goldfish_model": "old"});
        let m = Models { goldfish_model: " gpt-x ".into(), ..Models::default() };
        apply_models(&mut v, &m).unwrap();
        assert_eq!(v, json!({"data_dir": "~/x", "rule_pages": {"a": null}, "goldfish_model": "gpt-x"}));

        apply_models(&mut v, &Models::default()).unwrap();
        assert_eq!(v, json!({"data_dir": "~/x", "rule_pages": {"a": null}}));
    }

    #[test]
    fn saving_models_rejects_blank_or_spaced_values() {
        let mut v = json!({});
        let blank = Models { triager_model: " ".into(), ..Models::default() };
        assert!(apply_models(&mut v, &blank).is_err());
        let spaced = Models { goldfish_effort: "very high".into(), ..Models::default() };
        assert!(apply_models(&mut v, &spaced).is_err());
    }

    #[test]
    fn parses_cli_versions() {
        assert_eq!(parse_version("codex-cli 0.169.0\n").as_deref(), Some("0.169.0"));
        assert_eq!(parse_version("2.1.292 (Claude Code)\n").as_deref(), Some("2.1.292"));
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn missing_verified_versions_use_the_measured_ones() {
        let s: Settings = serde_json::from_value(json!({
            "data_dir": "~/x", "project_page_id": "p", "rule_pages": {}
        }))
        .unwrap();
        assert_eq!(s.verified_cli_versions, CliVersions::default());
        assert_eq!(s.models(), Models::default());
    }
}
