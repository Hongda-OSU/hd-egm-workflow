//! HD EGM Workflow desktop shell (M4). Every command is a thin wrapper over
//! egm-core; the app never writes to Notion.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use egm_core::model::{Advice, Decision, Triage};
use egm_core::notion::{compact_id, Notion};
use egm_core::review::{self, RoundVerdicts};
use egm_core::settings::Settings;
use egm_core::store::{self, Lock, Step};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn settings() -> CmdResult<Settings> {
    Settings::load().map_err(err)
}

#[derive(Serialize)]
struct AppStatus {
    notion: bool,
    codex: bool,
    claude: bool,
}

/// Header indicators. Only exit codes / the `loggedIn` flag are read; no
/// account details leave this function.
#[tauri::command]
async fn app_status() -> AppStatus {
    let codex = settings().is_ok_and(|s| {
        Command::new("codex")
            .env("CODEX_HOME", s.codex_home())
            .args(["login", "status"])
            .output()
            .is_ok_and(|o| o.status.success())
    });
    let claude = Command::new("claude")
        .args(["auth", "status"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v["loggedIn"].as_bool())
        .unwrap_or(false);
    AppStatus { notion: Notion::from_keychain().is_ok(), codex, claude }
}

#[derive(Serialize)]
struct PrdItem {
    page_id: String,
    title: String,
    status: String,
    rounds: u32,
}

#[derive(Serialize)]
struct PrdList {
    items: Vec<PrdItem>,
    /// Notion could not be reached; only locally stored PRDs are listed.
    offline: bool,
    error: Option<String>,
}

fn local_metas(data_dir: &Path) -> Vec<store::PrdMeta> {
    fs::read_dir(data_dir)
        .map(|rd| rd.flatten().filter_map(|e| store::read_meta(&e.path())).collect())
        .unwrap_or_default()
}

/// PRDs under the project page, merged with PRDs already stored locally so
/// past rounds stay reachable offline (PRD §6.2).
#[tauri::command]
async fn list_prds() -> CmdResult<PrdList> {
    let settings = settings()?;
    let locals = local_metas(&settings.data_dir());
    let mut items = Vec::new();
    let mut error = None;
    let notion = Notion::from_keychain();
    let listed = match &notion {
        Ok(n) => store::list(n, &settings).await,
        Err(e) => Err(e.to_string().into()),
    };
    match listed {
        Ok(entries) => {
            for e in entries {
                let (status, rounds) = e.meta.map_or(("none".into(), 0), |m| (m.status, m.rounds));
                items.push(PrdItem { page_id: e.page_id, title: e.title, status, rounds });
            }
        }
        Err(e) => error = Some(e.to_string()),
    }
    for m in locals {
        if !items.iter().any(|i| compact_id(&i.page_id) == compact_id(&m.notion_page_id)) {
            items.push(PrdItem { page_id: m.notion_page_id, title: m.title, status: m.status, rounds: m.rounds });
        }
    }
    items.sort_by_key(|i| i.rounds == 0);
    Ok(PrdList { offline: error.is_some(), items, error })
}

#[derive(Serialize)]
struct RuleItem {
    name: String,
    who: &'static str,
    page_id: Option<String>,
}

#[tauri::command]
fn rule_pages() -> CmdResult<Vec<RuleItem>> {
    let who = |name: &str| match name {
        "hd-goldfish-rules" => "金鱼",
        "hd-elephant-rules" => "整理员",
        _ => "大象",
    };
    let order = ["hd-prd-rules", "hd-workflow-rules", "hd-goldfish-rules", "hd-elephant-rules"];
    let pages = settings()?.rule_pages;
    let mut out: Vec<RuleItem> = pages
        .into_iter()
        .map(|(name, page_id)| RuleItem { who: who(&name), name, page_id })
        .collect();
    out.sort_by_key(|r| order.iter().position(|o| *o == r.name).unwrap_or(order.len()));
    Ok(out)
}

#[derive(Serialize)]
struct RoundView {
    n: u32,
    latest: bool,
    stage: String,
    fish_steps: BTreeMap<String, Step>,
    triage_step: Step,
    warnings: Vec<String>,
    triage: Option<Triage>,
    decisions: Vec<Decision>,
    verdicts: Option<RoundVerdicts>,
    has_handoff: bool,
    /// Raw goldfish output by name, for showing an issue's `refs`.
    fish: BTreeMap<String, Value>,
}

#[derive(Serialize)]
struct PrdDetail {
    page_id: String,
    title: String,
    status: String,
    rounds: Vec<RoundView>,
}

fn round_dirs(prd_dir: &Path) -> Vec<(u32, PathBuf)> {
    let mut dirs: Vec<(u32, PathBuf)> = fs::read_dir(prd_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_str()?.strip_prefix("round-")?.parse().ok()?;
                    Some((n, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    dirs.sort_by_key(|(n, _)| *n);
    dirs
}

fn prd_dir(settings: &Settings, page_id: &str) -> CmdResult<PathBuf> {
    store::find_prd_dir(&settings.data_dir(), page_id).ok_or_else(|| "this PRD has no local rounds yet".into())
}

fn round_dir(page_id: &str, round: u32) -> CmdResult<PathBuf> {
    let dir = prd_dir(&settings()?, page_id)?.join(format!("round-{round}"));
    if dir.is_dir() { Ok(dir) } else { Err(format!("round {round} not found")) }
}

/// Everything stored locally for one PRD; `None` when it was never run.
#[tauri::command]
async fn get_prd(page_id: String) -> CmdResult<Option<PrdDetail>> {
    let settings = settings()?;
    let Some(dir) = store::find_prd_dir(&settings.data_dir(), &page_id) else { return Ok(None) };
    let meta = store::read_meta(&dir).ok_or("prd.json is missing")?;
    let dirs = round_dirs(&dir);
    let last = dirs.last().map(|(n, _)| *n);
    let mut rounds = Vec::new();
    for (n, rd) in dirs {
        let Ok(state) = store::read_state(&rd) else { continue };
        let fish = ["retell", "critic", "ready", "spec"]
            .iter()
            .filter_map(|f| {
                let text = fs::read_to_string(rd.join("fish").join(format!("{f}.json"))).ok()?;
                Some((f.to_string(), serde_json::from_str(&text).ok()?))
            })
            .collect();
        rounds.push(RoundView {
            n,
            latest: Some(n) == last,
            stage: state.stage,
            fish_steps: state.fish,
            triage_step: state.triage,
            warnings: state.warnings,
            triage: review::read_triage(&rd).ok(),
            decisions: review::read_decisions(&rd).unwrap_or_default(),
            verdicts: state.verdicts,
            has_handoff: rd.join("handoff.md").is_file(),
            fish,
        });
    }
    Ok(Some(PrdDetail { page_id: meta.notion_page_id, title: meta.title, status: meta.status, rounds }))
}

#[derive(Clone, Serialize)]
struct Progress {
    page_id: String,
    line: String,
}

fn progress(app: &AppHandle, page_id: &str, line: String) {
    let _ = app.emit("round-progress", Progress { page_id: page_id.to_string(), line });
}

async fn run_round(app: &AppHandle, settings: &Settings, page_id: &str, dir: &Path) -> CmdResult<()> {
    let prd_dir = dir.parent().ok_or("round dir has no parent")?;
    let _lock = Lock::acquire(prd_dir).map_err(err)?;
    let (app2, pid) = (app.clone(), page_id.to_string());
    egm_core::round::run(settings, dir, move |line| progress(&app2, &pid, line))
        .await
        .map_err(err)?;
    Ok(())
}

/// Pull a new snapshot and run 4 goldfish + the triager (PRD §6.3).
#[tauri::command]
async fn start_round(app: AppHandle, page_id: String) -> CmdResult<()> {
    let settings = settings()?;
    let notion = Notion::from_keychain().map_err(err)?;
    progress(&app, &page_id, "pull: fetching PRD and rule pages from Notion".into());
    let pulled = store::pull(&notion, &settings, &page_id).await.map_err(err)?;
    for w in &pulled.warnings {
        progress(&app, &page_id, format!("warning: {w}"));
    }
    run_round(&app, &settings, &page_id, &pulled.dir).await
}

/// Rerun whatever failed or never ran in the latest round.
#[tauri::command]
async fn resume_round(app: AppHandle, page_id: String) -> CmdResult<()> {
    let settings = settings()?;
    let dir = store::latest_round(&prd_dir(&settings, &page_id)?).ok_or("no rounds yet")?;
    run_round(&app, &settings, &page_id, &dir).await
}

#[tauri::command]
async fn decide(page_id: String, round: u32, issue_id: String, decision: String, note: Option<String>) -> CmdResult<RoundVerdicts> {
    let advice = match decision.as_str() {
        "accept" => Advice::Accept,
        "reject" => Advice::Reject,
        other => return Err(format!("unknown decision {other}")),
    };
    review::decide(&round_dir(&page_id, round)?, &issue_id, advice, note).map_err(err)
}

fn pbcopy(text: &str) -> CmdResult<()> {
    let mut child = Command::new("/usr/bin/pbcopy").stdin(Stdio::piped()).spawn().map_err(err)?;
    child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(err)?;
    child.wait().map_err(err)?;
    Ok(())
}

/// Writes handoff.md and puts it on the clipboard for pasting to the elephant.
#[tauri::command]
async fn make_handoff(page_id: String, round: u32) -> CmdResult<String> {
    let md = review::write_handoff(&round_dir(&page_id, round)?).map_err(err)?;
    pbcopy(&md)?;
    Ok(md)
}

#[tauri::command]
async fn copy_text(text: String) -> CmdResult<()> {
    pbcopy(&text)
}

/// Opens a Notion page in the browser; only hex page ids are accepted.
#[tauri::command]
async fn open_notion(page_id: String) -> CmdResult<()> {
    let id = compact_id(&page_id);
    if id.len() != 32 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("not a Notion page id".into());
    }
    Command::new("/usr/bin/open").arg(format!("https://www.notion.so/{id}")).spawn().map_err(err)?;
    Ok(())
}

fn main() {
    if let Ok(s) = Settings::load() {
        // A crash or quit mid-round must not block the PRD forever (PRD §6.3).
        let _ = store::recover_stale(&s.data_dir());
    }
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            app_status,
            list_prds,
            rule_pages,
            get_prd,
            start_round,
            resume_round,
            decide,
            make_handoff,
            copy_text,
            open_notion
        ])
        .run(tauri::generate_context!())
        .expect("error while running HD EGM Workflow");
}
