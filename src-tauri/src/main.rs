//! HD EGM Workflow desktop shell. Every command is a thin wrapper over
//! egm-core; the app never writes to Notion.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use egm_core::diff;
use egm_core::model::{Advice, Decision, Triage, Verdict};
use egm_core::notion::{compact_id, Block, Notion};
use egm_core::prd;
use egm_core::review::{self, RoundVerdicts};
use egm_core::settings::{self as cfg, CliVersions, Models, Settings};
use egm_core::store::{self, Lock, PrdStatus, Stage, Step};
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
    /// The goldfish's dedicated CODEX_HOME, so the UI can show the exact
    /// login command when `codex` is false.
    codex_home: Option<String>,
    /// Installed CLI versions (None when the CLI is missing) and the ones
    /// the isolation probes passed on; a mismatch blocks runs (PRD §5).
    codex_version: Option<String>,
    claude_version: Option<String>,
    verified: CliVersions,
}

fn cli_version(bin: &str) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    cfg::parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// Preflight for a round. Only exit codes / the `loggedIn` flag are read; no
/// account details leave this function.
#[tauri::command]
async fn app_status() -> AppStatus {
    let loaded = settings().ok();
    let home = loaded.as_ref().map(|s| s.codex_home());
    let codex = home.as_ref().is_some_and(|h| {
        Command::new("codex")
            .env("CODEX_HOME", h)
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
    AppStatus {
        notion: Notion::from_keychain().is_ok(),
        codex,
        claude,
        codex_home: home.map(|h| h.display().to_string()),
        codex_version: cli_version("codex"),
        claude_version: cli_version("claude"),
        verified: loaded.map(|s| s.verified_cli_versions).unwrap_or_default(),
    }
}

#[derive(Serialize)]
struct PrdItem {
    page_id: String,
    title: String,
    status: PrdStatus,
    /// Rounds in the current review.
    rounds: u32,
    reviews: u32,
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
                let (status, rounds, reviews) =
                    e.meta.map_or((PrdStatus::NotReviewed, 0, 1), |m| (m.status, m.rounds, m.reviews));
                items.push(PrdItem { page_id: e.page_id, title: e.title, status, rounds, reviews });
            }
        }
        Err(e) => error = Some(e.to_string()),
    }
    for m in locals {
        if !items.iter().any(|i| compact_id(&i.page_id) == compact_id(&m.notion_page_id)) {
            items.push(PrdItem {
                page_id: m.notion_page_id,
                title: m.title,
                status: m.status,
                rounds: m.rounds,
                reviews: m.reviews,
            });
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
    stage: Stage,
    fish_steps: BTreeMap<String, Step>,
    triage_step: Step,
    warnings: Vec<String>,
    triage: Option<Triage>,
    decisions: Vec<Decision>,
    verdicts: Option<RoundVerdicts>,
    has_handoff: bool,
    /// A decision or note changed after handoff.md was written.
    handoff_stale: bool,
    /// Raw goldfish output by name, for showing an issue's `refs`.
    fish: BTreeMap<String, Value>,
}

/// One review of a PRD, for the review switcher.
#[derive(Serialize)]
struct ReviewSummary {
    n: u32,
    rounds: u32,
    /// The PRD verdict of the review's last round.
    verdict: Option<Verdict>,
}

#[derive(Serialize)]
struct PrdDetail {
    page_id: String,
    title: String,
    status: PrdStatus,
    /// The review `rounds` belong to, and the current (newest) review.
    review: u32,
    current_review: u32,
    reviews: Vec<ReviewSummary>,
    rounds: Vec<RoundView>,
}

fn prd_dir(settings: &Settings, page_id: &str) -> CmdResult<PathBuf> {
    store::find_prd_dir(&settings.data_dir(), page_id).ok_or_else(|| "this PRD has no local rounds yet".into())
}

fn round_dir(page_id: &str, review: u32, round: u32) -> CmdResult<PathBuf> {
    let dir = store::review_dir(&prd_dir(&settings()?, page_id)?, review).join(format!("round-{round}"));
    if dir.is_dir() { Ok(dir) } else { Err(format!("round {round} of review {review} not found")) }
}

fn round_verdicts(state: &store::RoundState, triage: Option<&Triage>, decisions: &[Decision]) -> Option<RoundVerdicts> {
    // Verdicts are only stored once a decision is made, so a round decided
    // before that, or not decided yet, has none on disk. Show what the
    // current decisions give instead of nothing; nothing is written here.
    state.verdicts.or_else(|| triage.map(|t| review::compute_verdicts(t, decisions, &state.warnings)))
}

fn review_summaries(prd: &Path, count: u32) -> Vec<ReviewSummary> {
    (1..=count)
        .map(|n| {
            let dirs = store::rounds(&store::review_dir(prd, n));
            let verdict = dirs.last().and_then(|(_, rd)| {
                let state = store::read_state(rd).ok()?;
                let triage = review::read_triage(rd).ok();
                let decisions = review::read_decisions(rd).unwrap_or_default();
                round_verdicts(&state, triage.as_ref(), &decisions).map(|v| v.prd)
            });
            ReviewSummary { n, rounds: dirs.len() as u32, verdict }
        })
        .collect()
}

/// Everything stored locally for one review of a PRD (the current review
/// when `review` is None); `None` when the PRD was never run.
#[tauri::command]
async fn get_prd(page_id: String, review: Option<u32>) -> CmdResult<Option<PrdDetail>> {
    let settings = settings()?;
    let Some(dir) = store::find_prd_dir(&settings.data_dir(), &page_id) else { return Ok(None) };
    let meta = store::read_meta(&dir).ok_or("prd.json is missing")?;
    let review = review.unwrap_or(meta.reviews).clamp(1, meta.reviews);
    let dirs = store::rounds(&store::review_dir(&dir, review));
    // Only the current review's last round can be decided.
    let last = dirs.last().map(|(n, _)| *n).filter(|_| review == meta.reviews);
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
        let triage = review::read_triage(&rd).ok();
        let decisions = review::read_decisions(&rd).unwrap_or_default();
        let verdicts = round_verdicts(&state, triage.as_ref(), &decisions);
        rounds.push(RoundView {
            n,
            latest: Some(n) == last,
            stage: state.stage,
            fish_steps: state.fish,
            triage_step: state.triage,
            warnings: state.warnings,
            triage,
            decisions,
            verdicts,
            has_handoff: rd.join("handoff.md").is_file(),
            handoff_stale: review::handoff_stale(&rd),
            fish,
        });
    }
    Ok(Some(PrdDetail {
        reviews: review_summaries(&dir, meta.reviews),
        page_id: meta.notion_page_id,
        title: meta.title,
        status: meta.status,
        review,
        current_review: meta.reviews,
        rounds,
    }))
}

#[derive(Serialize)]
struct RoundRef {
    review: u32,
    round: u32,
}

/// The text a snapshot diff compares: prd.md plus the key-decision list,
/// which goldfish never see but the author does change.
fn snapshot_text(rd: &Path) -> CmdResult<String> {
    let prd = fs::read_to_string(rd.join("prd.md")).map_err(err)?;
    let key = fs::read_to_string(rd.join("key-decisions.md")).unwrap_or_default();
    Ok(format!("{}\n\n{key}", prd.trim_end()))
}

/// The round after (review, round): the next round of the same review, or
/// round 1 of the next review.
fn next_round(prd: &Path, reviews: u32, review: u32, round: u32) -> Option<(RoundRef, PathBuf)> {
    let rd = store::review_dir(prd, review).join(format!("round-{}", round + 1));
    if rd.is_dir() {
        return Some((RoundRef { review, round: round + 1 }, rd));
    }
    let rd = store::review_dir(prd, review + 1).join("round-1");
    (review < reviews && rd.is_dir()).then_some((RoundRef { review: review + 1, round: 1 }, rd))
}

#[derive(Serialize)]
struct IssueDiff {
    next: RoundRef,
    sections: Vec<diff::SectionDiff>,
}

/// How the sections an issue names changed in the round after it (PRD
/// §6.4); `None` while there is no next round yet.
#[tauri::command]
async fn issue_diff(page_id: String, review: u32, round: u32, section: String) -> CmdResult<Option<IssueDiff>> {
    let settings = settings()?;
    let prd = prd_dir(&settings, &page_id)?;
    let reviews = store::read_meta(&prd).map_or(1, |m| m.reviews);
    let rd = round_dir(&page_id, review, round)?;
    let Some((next, next_dir)) = next_round(&prd, reviews, review, round) else { return Ok(None) };
    let sections = diff::issue_sections(&section, &snapshot_text(&rd)?, &snapshot_text(&next_dir)?);
    Ok(Some(IssueDiff { next, sections }))
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
    let prd_dir = store::prd_dir_of(dir).ok_or("round dir has no parent")?;
    let _lock = Lock::acquire(&prd_dir).map_err(err)?;
    let (app2, pid) = (app.clone(), page_id.to_string());
    egm_core::round::run(settings, dir, move |line| progress(&app2, &pid, line))
        .await
        .map_err(err)?;
    Ok(())
}

/// Pull a new snapshot and run 4 goldfish + the triager (PRD §6.3).
#[tauri::command]
async fn start_round(app: AppHandle, page_id: String) -> CmdResult<()> {
    pull_and_run(&app, &page_id, false).await
}

/// Like start_round, but as round 1 of a new review: earlier rejections no
/// longer carry over, and earlier reviews stay as they are.
#[tauri::command]
async fn start_review(app: AppHandle, page_id: String) -> CmdResult<()> {
    pull_and_run(&app, &page_id, true).await
}

async fn pull_and_run(app: &AppHandle, page_id: &str, new_review: bool) -> CmdResult<()> {
    let (app, page_id) = (app.clone(), page_id.to_string());
    let settings = settings()?;
    let notion = Notion::from_keychain().map_err(err)?;
    progress(&app, &page_id, "pull: fetching PRD and rule pages from Notion".into());
    let on_wait = |secs: u64| progress(&app, &page_id, format!("pull: waiting {secs}s for recent Notion edits to settle"));
    let pulled = store::pull(&notion, &settings, &page_id, new_review, &on_wait).await.map_err(err)?;
    for w in &pulled.warnings {
        progress(&app, &page_id, format!("warning: {w}"));
    }
    run_round(&app, &settings, &page_id, &pulled.dir).await
}

/// Rerun whatever failed or never ran in the latest round.
#[tauri::command]
async fn resume_round(app: AppHandle, page_id: String) -> CmdResult<()> {
    let settings = settings()?;
    let dir = store::latest_round(&store::current_review_dir(&prd_dir(&settings, &page_id)?)).ok_or("no rounds yet")?;
    run_round(&app, &settings, &page_id, &dir).await
}

#[tauri::command]
async fn decide(
    page_id: String,
    review: u32,
    round: u32,
    issue_id: String,
    decision: String,
    note: Option<String>,
) -> CmdResult<RoundVerdicts> {
    let advice = match decision.as_str() {
        "accept" => Advice::Accept,
        "reject" => Advice::Reject,
        other => return Err(format!("unknown decision {other}")),
    };
    review::decide(&round_dir(&page_id, review, round)?, &issue_id, advice, note).map_err(err)
}

fn pbcopy(text: &str) -> CmdResult<()> {
    let mut child = Command::new("/usr/bin/pbcopy").stdin(Stdio::piped()).spawn().map_err(err)?;
    child.stdin.take().ok_or("no stdin")?.write_all(text.as_bytes()).map_err(err)?;
    child.wait().map_err(err)?;
    Ok(())
}

/// Writes handoff.md and puts it on the clipboard for pasting to the elephant.
#[tauri::command]
async fn make_handoff(page_id: String, review: u32, round: u32) -> CmdResult<String> {
    let md = review::write_handoff(&round_dir(&page_id, review, round)?).map_err(err)?;
    pbcopy(&md)?;
    Ok(md)
}

/// Copies the saved handoff.md again without regenerating it.
#[tauri::command]
async fn copy_handoff(page_id: String, review: u32, round: u32) -> CmdResult<()> {
    let md = fs::read_to_string(round_dir(&page_id, review, round)?.join("handoff.md")).map_err(err)?;
    pbcopy(&md)
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

/// Section headings and the key-decision check for a PRD not reviewed yet,
/// so problems show before a run is started. Read-only, top-level blocks.
#[tauri::command]
async fn prd_outline(page_id: String) -> CmdResult<prd::Outline> {
    let notion = Notion::from_keychain().map_err(err)?;
    let blocks: Vec<Block> = notion.children(&page_id).await.map_err(err)?.into_iter().map(Block::from_json).collect();
    Ok(prd::outline(&blocks))
}

/// Shows a path with the home folder as `~`, as settings.json writes it.
fn tilde(p: &Path) -> String {
    match p.strip_prefix(cfg::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[derive(Serialize)]
struct SettingsView {
    models: Models,
    defaults: Models,
    data_dir: String,
    settings_path: String,
}

#[tauri::command]
async fn get_settings() -> CmdResult<SettingsView> {
    let s = settings()?;
    Ok(SettingsView {
        models: s.models(),
        defaults: Models::default(),
        data_dir: tilde(&s.data_dir()),
        settings_path: tilde(&cfg::settings_path()),
    })
}

/// Saves only the model fields; they take effect from the next round.
#[tauri::command]
async fn save_models(models: Models) -> CmdResult<()> {
    cfg::save_models(&models).map_err(err)
}

/// Reveals the data folder or settings.json in Finder. Takes a name, not a
/// path, so the UI cannot point Finder anywhere else.
#[tauri::command]
async fn reveal(which: String) -> CmdResult<()> {
    let path = match which.as_str() {
        "data" => settings()?.data_dir(),
        "settings" => cfg::settings_path(),
        other => return Err(format!("unknown location {other}")),
    };
    Command::new("/usr/bin/open").arg("-R").arg(path).spawn().map_err(err)?;
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
            start_review,
            resume_round,
            issue_diff,
            decide,
            make_handoff,
            copy_handoff,
            copy_text,
            open_notion,
            get_settings,
            save_models,
            reveal,
            prd_outline
        ])
        .run(tauri::generate_context!())
        .expect("error while running HD EGM Workflow");
}
