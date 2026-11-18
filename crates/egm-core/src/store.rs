//! Local run storage (PRD §7): `<data_dir>/<slug>/prd.json` and
//! `round-<N>/`. Earlier rounds are never overwritten.
//!
//! Reviewing a PRD again from scratch starts a new review: review 1 keeps
//! its rounds directly under `<slug>/` (where they always were), review K
//! (K >= 2) under `<slug>/review-K/`. Rounds are numbered from 1 in each
//! review, and rejections carry over only within a review.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::markdown::{render, Rendered};
use crate::notion::{compact_id, Notion};
use crate::prd::split_key_decisions;
use crate::settings::Settings;
use crate::Result;

pub const FISH: [&str; 4] = ["retell", "critic", "ready", "spec"];

/// `prd.json` status (PRD §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrdStatus {
    #[serde(rename = "none")]
    NotReviewed,
    #[serde(rename = "in_review")]
    InReview,
    #[serde(rename = "go")]
    Go,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrdMeta {
    pub notion_page_id: String,
    pub title: String,
    pub slug: String,
    /// Rounds in the current review.
    pub rounds: u32,
    pub status: PrdStatus,
    /// Number of reviews; the last one is current. Missing in prd.json
    /// files written before reviews existed, which have exactly one.
    #[serde(default = "one")]
    pub reviews: u32,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub status: StepStatus,
    pub retries: u32,
    pub error: Option<String>,
}

impl Step {
    fn queued() -> Self {
        Step { status: StepStatus::Queued, retries: 0, error: None }
    }
}

/// Where a round is in its lifecycle (`round.json` `stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Fish,
    Triage,
    Decide,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundState {
    pub stage: Stage,
    pub fish: BTreeMap<String, Step>,
    pub triage: Step,
    pub warnings: Vec<String>,
    /// Recomputed after triage and after every decision (PRD §6.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdicts: Option<crate::review::RoundVerdicts>,
    /// Model settings frozen when the round started, so a rerun or resume
    /// uses the same ones (PRD §6.3). None for rounds started before this
    /// existed; those fall back to the current settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::settings::Models>,
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

/// Finds the local directory of a PRD by the full page id stored in its
/// prd.json. Not by the id in the directory name: pages created around the
/// same time share their first 8 id characters.
pub fn find_prd_dir(data_dir: &Path, page_id: &str) -> Option<PathBuf> {
    let id = compact_id(page_id);
    fs::read_dir(data_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| read_meta(p).is_some_and(|m| compact_id(&m.notion_page_id) == id))
}

/// Directory for a PRD with no local data yet: `slug()`, or with the rest of
/// the page id appended when another PRD already owns that name.
fn new_prd_dir(data_dir: &Path, title: &str, page_id: &str) -> PathBuf {
    let base = slug(title, page_id);
    if read_meta(&data_dir.join(&base)).is_none() {
        return data_dir.join(base);
    }
    let id = compact_id(page_id);
    data_dir.join(format!("{base}-{}", id.get(8..).unwrap_or(&id)))
}

/// Directory holding the `round-N` dirs of review `k`.
pub fn review_dir(prd_dir: &Path, k: u32) -> PathBuf {
    if k <= 1 { prd_dir.to_path_buf() } else { prd_dir.join(format!("review-{k}")) }
}

/// The current review's directory (review 1 when prd.json is missing).
pub fn current_review_dir(prd_dir: &Path) -> PathBuf {
    review_dir(prd_dir, read_meta(prd_dir).map_or(1, |m| m.reviews))
}

/// The PRD directory a round dir belongs to: the nearest ancestor with a
/// prd.json, else the round's parent.
pub fn prd_dir_of(round_dir: &Path) -> Option<PathBuf> {
    round_dir
        .ancestors()
        .skip(1)
        .find(|p| p.join("prd.json").is_file())
        .or_else(|| round_dir.parent())
        .map(Path::to_path_buf)
}

/// True when `round_dir` is the latest round of its PRD's current review;
/// only that round can be decided.
pub fn is_latest(round_dir: &Path) -> bool {
    prd_dir_of(round_dir)
        .and_then(|p| latest_round(&current_review_dir(&p)))
        .is_some_and(|l| l == round_dir)
}

pub fn read_meta(prd_dir: &Path) -> Option<PrdMeta> {
    let text = fs::read_to_string(prd_dir.join("prd.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes via a temp file and a rename, so a crash never leaves a
/// half-written file behind.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path.file_name().ok_or("path has no file name")?.to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_atomic(path, (serde_json::to_string_pretty(value)? + "\n").as_bytes())
}

pub fn read_state(round_dir: &Path) -> Result<RoundState> {
    let path = round_dir.join("round.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn write_state(round_dir: &Path, state: &RoundState) -> Result<()> {
    write_json(&round_dir.join("round.json"), state)
}

/// Steps left running by a crash or quit become failed so the PRD is not
/// blocked forever; the author reruns them (PRD §6.3). Call at app start.
/// PRDs whose lock is held by a live process (e.g. `egm run` in a terminal)
/// are left alone.
pub fn recover_stale(data_dir: &Path) -> Result<usize> {
    let mut fixed = 0;
    let Ok(prds) = fs::read_dir(data_dir) else { return Ok(0) };
    for prd in prds.flatten().filter(|e| e.path().is_dir()) {
        if lock_holder(&prd.path().join(LOCK)).is_some_and(pid_alive) {
            continue;
        }
        let Some(round_dir) = latest_round(&current_review_dir(&prd.path())) else { continue };
        let Ok(mut state) = read_state(&round_dir) else { continue };
        let mut changed = false;
        for step in state.fish.values_mut().chain(std::iter::once(&mut state.triage)) {
            if step.status == StepStatus::Running {
                step.status = StepStatus::Failed;
                step.error = Some("interrupted: the app exited while this was running".into());
                changed = true;
            }
        }
        if changed {
            state.stage = Stage::Failed;
            write_state(&round_dir, &state)?;
            fixed += 1;
        }
    }
    Ok(fixed)
}

/// The `round-N` dirs of a PRD, oldest first.
pub fn rounds(prd_dir: &Path) -> Vec<(u32, PathBuf)> {
    let mut out: Vec<(u32, PathBuf)> = fs::read_dir(prd_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_str()?.strip_prefix("round-")?.parse().ok()?;
                    Some((n, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|(n, _)| *n);
    out
}

/// The highest-numbered `round-N` dir of a PRD.
pub fn latest_round(prd_dir: &Path) -> Option<PathBuf> {
    rounds(prd_dir).pop().map(|(_, p)| p)
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

const LOCK: &str = ".lock";

/// Held while a round is being written; one round per PRD at a time. The
/// file holds the owner's pid, so a lock left by a crash is taken over.
pub struct Lock(PathBuf);

impl Lock {
    pub fn acquire(prd_dir: &Path) -> Result<Self> {
        let path = prd_dir.join(LOCK);
        if create_lock(&path)? {
            return Ok(Lock(path));
        }
        if let Some(pid) = lock_holder(&path).filter(|p| pid_alive(*p)) {
            return Err(format!(
                "another round is running for this PRD (pid {pid}; delete {} if that is wrong)",
                path.display()
            )
            .into());
        }
        // The owner is gone (crash or force quit).
        let _ = fs::remove_file(&path);
        if create_lock(&path)? {
            return Ok(Lock(path));
        }
        Err("another round started for this PRD at the same moment".into())
    }
}

/// Creates the lock with our pid in one step: the pid goes into a temp file
/// that is then hard-linked into place, so the lock never exists empty.
fn create_lock(path: &Path) -> Result<bool> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let tmp = path.with_file_name(format!("{LOCK}.{}.{nanos}", std::process::id()));
    fs::write(&tmp, std::process::id().to_string())?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn lock_holder(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
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
/// so a round never mixes versions (PRD §5). `on_wait` gets the seconds it
/// waits for a recent edit to settle (see `settle_wait`).
async fn snapshot(notion: &Notion, settings: &Settings, page_id: &str, on_wait: &OnWait<'_>) -> Result<Snapshot> {
    let rule_ids: Vec<(String, String)> = settings
        .rule_pages
        .iter()
        .filter_map(|(name, id)| id.clone().map(|id| (name.clone(), id)))
        .collect();
    let mut ids = vec![page_id.to_string()];
    ids.extend(rule_ids.iter().map(|(_, id)| id.clone()));

    for _ in 0..4 {
        let before = edited_times(notion, &ids).await?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
        if let Some(wait) = settle_wait(&before, now) {
            on_wait(wait.as_secs());
            tokio::time::sleep(wait).await;
            continue;
        }
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

/// Notion's `last_edited_time` is truncated to the minute, so an edit made
/// during the pull in the same minute as the last one leaves it unchanged.
/// When any page was edited within the last minute (a minute of slack for
/// clock skew), returns how long to wait until that minute is safely over;
/// after that, any new edit lands in a later minute and changes the value.
fn settle_wait(times: &[String], now: i64) -> Option<std::time::Duration> {
    let cutoff = crate::review::rfc3339(now - 60);
    let recent = times.iter().any(|t| t.get(..16).is_some_and(|m| m >= &cutoff[..16]));
    recent.then(|| std::time::Duration::from_secs((121 - now.rem_euclid(60)) as u64))
}

async fn edited_times(notion: &Notion, ids: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for id in ids {
        out.push(notion.page(id).await?.last_edited_time);
    }
    Ok(out)
}

/// Called with the seconds `pull` will wait before reading Notion again.
pub type OnWait<'a> = dyn Fn(u64) + Send + Sync + 'a;

pub struct PulledRound {
    pub dir: PathBuf,
    pub round: u32,
    pub warnings: Vec<String>,
}

/// Fetch stage of a round: writes `round-<N>/` with prd.md, key-decisions.md,
/// rules/ and round.json. Everything goes to a temp dir first and is moved
/// into place only when complete. With `new_review`, the round is round 1
/// of a new review; the PRD must already have rounds.
pub async fn pull(
    notion: &Notion,
    settings: &Settings,
    page_id: &str,
    new_review: bool,
    on_wait: &OnWait<'_>,
) -> Result<PulledRound> {
    let page = notion.page(page_id).await?;
    let data_dir = settings.data_dir();
    let prd_dir = find_prd_dir(&data_dir, &page.id)
        .unwrap_or_else(|| new_prd_dir(&data_dir, &page.title, &page.id));
    fs::create_dir_all(&prd_dir)?;
    let _lock = Lock::acquire(&prd_dir)?;

    let mut meta = read_meta(&prd_dir).unwrap_or_else(|| PrdMeta {
        notion_page_id: page.id.clone(),
        title: page.title.clone(),
        slug: prd_dir.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        rounds: 0,
        status: PrdStatus::NotReviewed,
        reviews: 1,
    });
    if new_review && rounds(&review_dir(&prd_dir, meta.reviews)).is_empty() {
        return Err("this PRD has no rounds yet, so there is nothing to review again".into());
    }
    meta.title = page.title.clone();

    let snap = snapshot(notion, settings, &page.id, on_wait).await?;
    let mut warnings = snap.prd.warnings.clone();
    warnings.extend(snap.key_decisions.warnings.iter().map(|w| format!("key-decisions: {w}")));
    for (name, r) in &snap.rules {
        warnings.extend(r.warnings.iter().map(|w| format!("rules/{name}: {w}")));
    }

    let review = if new_review { meta.reviews + 1 } else { meta.reviews };
    let rdir = review_dir(&prd_dir, review);
    fs::create_dir_all(&rdir)?;
    let mut round = if new_review { 1 } else { meta.rounds + 1 };
    while rdir.join(format!("round-{round}")).exists() {
        round += 1;
    }
    let tmp = rdir.join(format!(".round-{round}.tmp"));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(tmp.join("rules"))?;
    fs::write(tmp.join("prd.md"), &snap.prd.markdown)?;
    fs::write(tmp.join("key-decisions.md"), &snap.key_decisions.markdown)?;
    for (name, r) in &snap.rules {
        fs::write(tmp.join("rules").join(format!("{name}.md")), &r.markdown)?;
    }
    // Freeze the prompt templates; reruns of this round read these.
    fs::create_dir_all(tmp.join("prompts"))?;
    for (name, text) in crate::round::PROMPT_TEMPLATES {
        fs::write(tmp.join("prompts").join(format!("{name}.md")), text)?;
    }
    let state = RoundState {
        stage: Stage::Fish,
        fish: FISH.iter().map(|f| (f.to_string(), Step::queued())).collect(),
        triage: Step::queued(),
        warnings: warnings.clone(),
        verdicts: None,
        config: Some(settings.models()),
    };
    write_json(&tmp.join("round.json"), &state)?;

    let dir = rdir.join(format!("round-{round}"));
    fs::rename(&tmp, &dir)?;
    meta.reviews = review;
    meta.rounds = round;
    meta.status = PrdStatus::InReview;
    write_json(&prd_dir.join("prd.json"), &meta)?;
    Ok(PulledRound { dir, round, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn edits_in_the_last_minute_wait_until_it_is_over() {
        // 2026-11-18T18:02:30Z
        let now = 1_795_024_950;
        assert_eq!(crate::review::rfc3339(now), "2026-11-18T18:02:30Z");
        let at = |t: &str| vec![t.to_string(), "2026-01-01T00:00:00.000Z".to_string()];
        let wait = settle_wait(&at("2026-11-18T18:02:00.000Z"), now).unwrap();
        assert_eq!(wait.as_secs(), 91);
        assert!(settle_wait(&at("2026-11-18T18:01:00.000Z"), now).is_some());
        // after waiting, the same timestamp no longer counts as recent
        assert!(settle_wait(&at("2026-11-18T18:02:00.000Z"), now + 91).is_none());
    }

    #[test]
    fn older_or_missing_edit_times_do_not_wait() {
        let now = 1_795_024_950;
        let at = |t: &str| vec![t.to_string()];
        assert!(settle_wait(&at("2026-11-18T18:00:00.000Z"), now).is_none());
        assert!(settle_wait(&at(""), now).is_none());
    }

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

    const EGM: &str = "40660d3c-f9f3-8108-8712-e6e662bb0f2c";
    const SAME_PREFIX: &str = "40660d3c-f9f3-8140-8d00-ed34fc94ea78";

    fn claim(data_dir: &Path, name: &str, page_id: &str) -> PathBuf {
        let dir = data_dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        let meta = PrdMeta {
            notion_page_id: page_id.into(),
            title: "t".into(),
            slug: name.into(),
            rounds: 1,
            status: PrdStatus::InReview,
            reviews: 1,
        };
        write_json(&dir.join("prd.json"), &meta).unwrap();
        dir
    }

    #[test]
    fn later_reviews_live_in_their_own_dir_and_only_their_last_round_is_latest() {
        let tmp = TempDir::new("reviews");
        let prd = claim(tmp.path(), "p", EGM);
        for d in ["round-1", "round-2", "review-2/round-1"] {
            fs::create_dir_all(prd.join(d)).unwrap();
        }
        assert_eq!(rounds(&prd).len(), 2, "review-2 is not a round of review 1");
        assert_eq!(current_review_dir(&prd), prd);
        assert!(is_latest(&prd.join("round-2")));

        let mut meta = read_meta(&prd).unwrap();
        meta.reviews = 2;
        write_json(&prd.join("prd.json"), &meta).unwrap();
        assert_eq!(current_review_dir(&prd), prd.join("review-2"));
        assert!(!is_latest(&prd.join("round-2")));
        assert!(is_latest(&prd.join("review-2/round-1")));
        assert_eq!(prd_dir_of(&prd.join("review-2/round-1")), Some(prd));
    }

    #[test]
    fn prd_json_without_reviews_means_one_review() {
        let text = r#"{"notion_page_id":"x","title":"t","slug":"s","rounds":7,"status":"go"}"#;
        assert_eq!(serde_json::from_str::<PrdMeta>(text).unwrap().reviews, 1);
    }

    #[test]
    fn find_prd_dir_matches_the_full_id_not_the_prefix() {
        let tmp = TempDir::new("find");
        let egm = claim(tmp.path(), "hd-egm-workflow-40660d3c", EGM);
        assert_eq!(find_prd_dir(tmp.path(), EGM), Some(egm.clone()));
        assert_eq!(find_prd_dir(tmp.path(), &compact_id(EGM)), Some(egm));
        assert_eq!(find_prd_dir(tmp.path(), SAME_PREFIX), None);
    }

    #[test]
    fn new_prd_dir_avoids_a_name_owned_by_another_prd() {
        let tmp = TempDir::new("newdir");
        assert_eq!(new_prd_dir(tmp.path(), "香港", EGM), tmp.path().join("40660d3c"));
        claim(tmp.path(), "40660d3c", EGM);
        assert_eq!(
            new_prd_dir(tmp.path(), "照片", SAME_PREFIX),
            tmp.path().join("40660d3c-f9f381408d00ed34fc94ea78")
        );
        // A leftover dir without prd.json (failed first pull) is reused.
        fs::create_dir_all(tmp.path().join("other-3e360d3c")).unwrap();
        assert_eq!(
            new_prd_dir(tmp.path(), "Other", "3e360d3c-f9f3-812f-9e04-e7bf96e28c20"),
            tmp.path().join("other-3e360d3c")
        );
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let tmp = TempDir::new("lock");
        let held = Lock::acquire(tmp.path()).unwrap();
        assert_eq!(lock_holder(&tmp.path().join(LOCK)), Some(std::process::id()));
        assert!(Lock::acquire(tmp.path()).is_err());
        drop(held);
        assert!(!tmp.path().join(LOCK).exists());
        assert!(Lock::acquire(tmp.path()).is_ok());
    }

    #[test]
    fn lock_left_by_a_dead_process_is_taken_over() {
        let tmp = TempDir::new("stale");
        // Above the macOS pid limit, so never a live process.
        fs::write(tmp.path().join(LOCK), "4000000").unwrap();
        let _held = Lock::acquire(tmp.path()).unwrap();
        assert_eq!(lock_holder(&tmp.path().join(LOCK)), Some(std::process::id()));
    }

    #[test]
    fn round_state_without_config_still_reads() {
        let old = r#"{"stage":"decide","fish":{},"triage":{"status":"done","retries":0,"error":null},"warnings":[]}"#;
        let state: RoundState = serde_json::from_str(old).unwrap();
        assert_eq!(state.config, None);
        let mut state = state;
        state.config = Some(crate::settings::Models::default());
        let back: RoundState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(back.config, Some(crate::settings::Models::default()));
    }

    #[test]
    fn write_json_leaves_no_temp_file() {
        let tmp = TempDir::new("atomic");
        let path = tmp.path().join("a.json");
        write_json(&path, &[1, 2]).unwrap();
        write_json(&path, &[3]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "[\n  3\n]\n");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
}
