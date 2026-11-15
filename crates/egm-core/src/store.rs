//! Local run storage (PRD §7): `<data_dir>/<slug>/prd.json` and
//! `round-<N>/`. Earlier rounds are never overwritten.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::markdown::{render, Rendered};
use crate::notion::{compact_id, Notion};
use crate::prd::split_key_decisions;
use crate::settings::Settings;
use crate::Result;

pub const FISH: [&str; 4] = ["retell", "critic", "ready", "spec"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrdMeta {
    pub notion_page_id: String,
    pub title: String,
    pub slug: String,
    pub rounds: u32,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub status: String,
    pub retries: u32,
    pub error: Option<String>,
}

impl Step {
    fn queued() -> Self {
        Step { status: "queued".into(), retries: 0, error: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundState {
    pub stage: String,
    pub fish: BTreeMap<String, Step>,
    pub triage: Step,
    pub warnings: Vec<String>,
}

/// Directory name: ASCII slug of the title + first 8 chars of the page id.
pub fn slug(title: &str, page_id: &str) -> String {
    let mut s = String::new();
    let mut pending_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !s.is_empty() {
                s.push('-');
            }
            s.push(c.to_ascii_lowercase());
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    let id: String = compact_id(page_id).chars().take(8).collect();
    if s.is_empty() { id } else { format!("{s}-{id}") }
}

/// Finds the local directory of a PRD by its page id (the slug may predate
/// a rename, so match on the id suffix).
pub fn find_prd_dir(data_dir: &Path, page_id: &str) -> Option<PathBuf> {
    let suffix: String = compact_id(page_id).chars().take(8).collect();
    fs::read_dir(data_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&suffix)))
}

pub fn read_meta(prd_dir: &Path) -> Option<PrdMeta> {
    let text = fs::read_to_string(prd_dir.join("prd.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    fs::write(path, serde_json::to_string_pretty(value)? + "\n")?;
    Ok(())
}

pub struct PrdEntry {
    pub page_id: String,
    pub title: String,
    pub meta: Option<PrdMeta>,
}

/// Child pages of the project page, with their local review status.
pub async fn list(notion: &Notion, settings: &Settings) -> Result<Vec<PrdEntry>> {
    let data_dir = settings.data_dir();
    Ok(notion
        .children(&settings.project_page_id)
        .await?
        .into_iter()
        .filter(|b| b["type"] == "child_page")
        .map(|b| {
            let page_id = b["id"].as_str().unwrap_or("").to_string();
            let meta = find_prd_dir(&data_dir, &page_id).and_then(|d| read_meta(&d));
            PrdEntry {
                title: b["child_page"]["title"].as_str().unwrap_or("").to_string(),
                page_id,
                meta,
            }
        })
        .collect())
}

/// Held while a round is being written; one round per PRD at a time.
struct Lock(PathBuf);

impl Lock {
    fn acquire(prd_dir: &Path) -> Result<Self> {
        let path = prd_dir.join(".lock");
        fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "another round is running for this PRD (delete {} if it crashed)",
                    path.display()
                )
            } else {
                e.to_string()
            }
        })?;
        Ok(Lock(path))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct Snapshot {
    prd: Rendered,
    key_decisions: Rendered,
    rules: Vec<(String, Rendered)>,
}

/// Pulls the PRD and rule pages; retries when any page was edited mid-pull
/// so a round never mixes versions (PRD §5).
async fn snapshot(notion: &Notion, settings: &Settings, page_id: &str) -> Result<Snapshot> {
    let rule_ids: Vec<(String, String)> = settings
        .rule_pages
        .iter()
        .filter_map(|(name, id)| id.clone().map(|id| (name.clone(), id)))
        .collect();
    let mut ids = vec![page_id.to_string()];
    ids.extend(rule_ids.iter().map(|(_, id)| id.clone()));

    for _ in 0..3 {
        let before = edited_times(notion, &ids).await?;
        let prd_blocks = notion.tree(page_id).await?;
        let mut rules = Vec::new();
        for (name, id) in &rule_ids {
            rules.push((name.clone(), render(&notion.tree(id).await?)));
        }
        if edited_times(notion, &ids).await? == before {
            let (body, key) = split_key_decisions(prd_blocks)?;
            return Ok(Snapshot { prd: render(&body), key_decisions: render(&key), rules });
        }
    }
    Err("Notion pages kept changing while pulling; try again in a minute".into())
}

async fn edited_times(notion: &Notion, ids: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        out.push(notion.page(id).await?.last_edited_time);
    }
    Ok(out)
}

pub struct PulledRound {
    pub dir: PathBuf,
    pub round: u32,
    pub warnings: Vec<String>,
}

/// Fetch stage of a round: writes `round-<N>/` with prd.md, key-decisions.md,
/// rules/ and round.json. Everything goes to a temp dir first and is moved
/// into place only when complete.
pub async fn pull(notion: &Notion, settings: &Settings, page_id: &str) -> Result<PulledRound> {
    let page = notion.page(page_id).await?;
    let data_dir = settings.data_dir();
    let prd_dir = find_prd_dir(&data_dir, &page.id)
        .unwrap_or_else(|| data_dir.join(slug(&page.title, &page.id)));
    fs::create_dir_all(&prd_dir)?;
    let _lock = Lock::acquire(&prd_dir)?;

    let mut meta = read_meta(&prd_dir).unwrap_or_else(|| PrdMeta {
        notion_page_id: page.id.clone(),
        title: page.title.clone(),
        slug: prd_dir.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        rounds: 0,
        status: "none".into(),
    });
    meta.title = page.title.clone();

    let snap = snapshot(notion, settings, &page.id).await?;
    let mut warnings = snap.prd.warnings.clone();
    warnings.extend(snap.key_decisions.warnings.iter().map(|w| format!("key-decisions: {w}")));
    for (name, r) in &snap.rules {
        warnings.extend(r.warnings.iter().map(|w| format!("rules/{name}: {w}")));
    }

    let mut round = meta.rounds + 1;
    while prd_dir.join(format!("round-{round}")).exists() {
        round += 1;
    }
    let tmp = prd_dir.join(format!(".round-{round}.tmp"));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(tmp.join("rules"))?;
    fs::write(tmp.join("prd.md"), &snap.prd.markdown)?;
    fs::write(tmp.join("key-decisions.md"), &snap.key_decisions.markdown)?;
    for (name, r) in &snap.rules {
        fs::write(tmp.join("rules").join(format!("{name}.md")), &r.markdown)?;
    }
    let state = RoundState {
        stage: "fish".into(),
        fish: FISH.iter().map(|f| (f.to_string(), Step::queued())).collect(),
        triage: Step::queued(),
        warnings: warnings.clone(),
    };
    write_json(&tmp.join("round.json"), &state)?;

    let dir = prd_dir.join(format!("round-{round}"));
    fs::rename(&tmp, &dir)?;
    meta.rounds = round;
    meta.status = "in_review".into();
    write_json(&prd_dir.join("prd.json"), &meta)?;
    Ok(PulledRound { dir, round, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_prd_example() {
        assert_eq!(
            slug("HD EGM Workflow · 产品文档", "40660d3c-f9f3-8108-8712-e6e662bb0f2c"),
            "hd-egm-workflow-40660d3c"
        );
    }

    #[test]
    fn slug_of_non_ascii_title_is_the_id_prefix() {
        assert_eq!(slug("香港求职自动化", "36660d3cf9f381ff"), "36660d3c");
    }
}
