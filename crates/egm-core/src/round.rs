//! Fish and triage stages of a round (PRD §6.3): 4 goldfish in parallel,
//! then the triager once all 4 succeeded. Progress lives in round.json, so
//! `resume` only reruns what is not done yet.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::task::JoinSet;

use crate::agents::{self, ClaudeConfig, CodexConfig};
use crate::model::{Advice, Critic, Decision, Issue, Ready, Rejection, Retell, Role, Spec, Triage, TriageOutput, Verdict};
use crate::settings::Settings;
use crate::store::{read_state, write_json, write_state, Stage, StepStatus};
use crate::Result;

const FISH: [(&str, &str); 4] = [
    ("retell", include_str!("../../../schemas/retell.schema.json")),
    ("critic", include_str!("../../../schemas/critic.schema.json")),
    ("ready", include_str!("../../../schemas/ready.schema.json")),
    ("spec", include_str!("../../../schemas/spec.schema.json")),
];
/// Prompt templates built into this binary. `store::pull` copies them into
/// `round-N/prompts/` when a round starts, and runs read that copy, so a
/// rebuilt app does not change a round already under way (PRD §6.3).
pub const PROMPT_TEMPLATES: [(&str, &str); 5] = [
    ("retell", include_str!("../../../prompts/retell.md")),
    ("critic", include_str!("../../../prompts/critic.md")),
    ("ready", include_str!("../../../prompts/ready.md")),
    ("spec", include_str!("../../../prompts/spec.md")),
    ("triage", include_str!("../../../prompts/triage.md")),
];
const TRIAGE_SCHEMA: &str = include_str!("../../../schemas/triage.schema.json");
const TRIAGE_SYSTEM: &str = "You are the triager in a cross-model PRD review workflow. \
Follow the triage rules in the user's message and return only the structured result.";

pub struct RoundResult {
    pub dir: PathBuf,
    pub triage: Triage,
    /// App-computed (PRD §6.6), not the triager's own pre-decision guess.
    pub verdicts: crate::review::RoundVerdicts,
    pub warnings: Vec<String>,
}

impl RoundResult {
    pub fn overall_go(&self) -> bool {
        self.verdicts.prd == Verdict::Go
    }
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()).into())
}

/// The round's frozen template; rounds started before templates were
/// frozen have none and use the built-in one.
fn template(round_dir: &Path, name: &str) -> Result<String> {
    let frozen = round_dir.join("prompts").join(format!("{name}.md"));
    if frozen.exists() {
        return read(&frozen);
    }
    PROMPT_TEMPLATES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| t.to_string())
        .ok_or_else(|| format!("no prompt template named {name}").into())
}

/// The 交付前检查清单 items of hd-prd-rules, minus those a goldfish cannot
/// judge: marking the title [已完成] (not done while under review), the
/// 关键决策清单 item (stripped from what goldfish see; `pull` already
/// refuses a PRD without it) and child-page items (goldfish only see a
/// `📄 子页面：title` line, never the child page's body).
pub fn spec_checklist(prd_rules: &str) -> Result<String> {
    let mut in_section = false;
    let mut items = Vec::new();
    for line in prd_rules.lines() {
        if line.starts_with("## ") {
            in_section = line.contains("交付前检查清单");
            continue;
        }
        let t = line.trim_start();
        let item = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] "));
        if let (true, Some(item)) = (in_section, item)
            && !item.contains("[已完成]")
            && !item.contains(crate::prd::KEY_DECISIONS_HEADING)
            && !item.contains("子页面")
        {
            items.push(format!("- {item}"));
        }
    }
    if items.is_empty() {
        return Err("hd-prd-rules has no 交付前检查清单 items".into());
    }
    Ok(items.join("\n"))
}

fn parse<T: DeserializeOwned>(v: &Value) -> Result<T> {
    Ok(serde_json::from_value(v.clone())?)
}

/// Parses a goldfish's output; the spec goldfish must answer every one of
/// the `spec_items` checklist items and explain each failure.
fn validate_fish(name: &str, text: &str, spec_items: usize) -> Result<Value> {
    let v: Value = serde_json::from_str(text)?;
    match name {
        "retell" => drop(parse::<Retell>(&v)?),
        "critic" => drop(parse::<Critic>(&v)?),
        "ready" => drop(parse::<Ready>(&v)?),
        _ => {
            let spec: Spec = parse(&v)?;
            if spec.checks.len() != spec_items {
                return Err(format!("spec: answered {} checks, checklist has {spec_items}", spec.checks.len()).into());
            }
            if spec.checks.iter().any(|c| !c.pass && c.note.trim().is_empty()) {
                return Err("spec: a failed check has no note".into());
            }
        }
    }
    Ok(v)
}

/// One goldfish. Invalid output is retried once (PRD §9); a process failure
/// (quota, login, timeout) is not, so it does not burn quota twice.
async fn run_fish(
    name: &'static str,
    prompt: String,
    schema: &'static str,
    spec_items: usize,
    cfg: CodexConfig,
) -> (&'static str, u32, Result<Value>) {
    let mut retries = 0;
    loop {
        let text = match agents::codex(&cfg, prompt.clone(), schema).await {
            Ok(text) => text,
            Err(e) => return (name, retries, Err(e)),
        };
        match validate_fish(name, &text, spec_items) {
            Ok(v) => return (name, retries, Ok(v)),
            Err(_) if retries == 0 => retries += 1,
            Err(e) => return (name, retries, Err(format!("invalid output after a retry: {e}").into())),
        }
    }
}

/// Rejected issues of all earlier rounds (PRD §6.6), from each round's
/// triage.json + decisions.json. A rejection drops out again when a later
/// round matched an issue to it (`rejected_as`) and the author accepted that
/// issue: the author changed their mind.
pub fn collect_rejections(prd_dir: &Path, current: u32) -> Result<Vec<Rejection>> {
    let mut out = Vec::new();
    let mut overturned = Vec::new();
    for k in 1..current {
        let dir = prd_dir.join(format!("round-{k}"));
        let (Ok(t), Ok(d)) = (fs::read_to_string(dir.join("triage.json")), fs::read_to_string(dir.join("decisions.json"))) else {
            continue;
        };
        let triage: Triage = serde_json::from_str(&t)?;
        let decisions: Vec<Decision> = serde_json::from_str(&d)?;
        for dec in &decisions {
            let Some(i) = triage.issues.iter().find(|i| i.id == dec.issue_id) else { continue };
            match (dec.decision, &i.rejected_as) {
                (Advice::Reject, _) => out.push(Rejection {
                    id: i.id.clone(),
                    section: i.section.clone(),
                    title: i.title.clone(),
                    problem: i.problem.clone(),
                    note: dec.note.clone(),
                }),
                (Advice::Accept, Some(prev)) => overturned.push(prev.clone()),
                (Advice::Accept, None) => {}
            }
        }
    }
    out.retain(|r| !overturned.contains(&r.id));
    Ok(out)
}

/// What the triager's output is checked against beyond its schema.
struct TriageInputs<'a> {
    round: u32,
    key_items: usize,
    spec_items: usize,
    /// Entry counts of retell `guesses`, critic `issues`, ready `missing`.
    fish_len: [(&'static str, usize); 3],
    rejected: &'a [Rejection],
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Retell => "retell",
        Role::Critic => "critic",
        Role::Ready => "ready",
    }
}

/// Every issue must trace back to real goldfish entries, and the goldfish
/// in its `refs` must be exactly its `source` + `also_from`, since those
/// decide which GO lines it counts toward.
fn check_refs(i: &Issue, fish_len: &[(&'static str, usize)]) -> Result<()> {
    if i.refs.is_empty() {
        return Err(format!("{}: refs is empty", i.id).into());
    }
    let mut named = Vec::new();
    for r in &i.refs {
        let bad = || format!("{}: ref {r} is not <goldfish>#<index>", i.id);
        let (name, idx) = r.split_once('#').ok_or_else(bad)?;
        let idx: usize = idx.parse().map_err(|_| bad())?;
        let (name, len) = *fish_len.iter().find(|(n, _)| *n == name).ok_or_else(bad)?;
        if idx >= len {
            return Err(format!("{}: ref {r} is out of range ({name} has {len} entries)", i.id).into());
        }
        named.push(name);
    }
    let mut lines: Vec<&str> = std::iter::once(i.source).chain(i.also_from.iter().copied()).map(role_name).collect();
    for v in [&mut named, &mut lines] {
        v.sort_unstable();
        v.dedup();
    }
    if named != lines {
        return Err(format!("{}: refs come from {named:?} but source + also_from is {lines:?}", i.id).into());
    }
    Ok(())
}

fn validate_triage(v: &Value, ctx: &TriageInputs) -> Result<TriageOutput> {
    let out: TriageOutput = parse(v)?;
    let prefix = format!("r{}-", ctx.round);
    for i in &out.issues {
        let num = i.id.strip_prefix(&prefix).unwrap_or("");
        if num.len() < 2 || !num.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("issue id {} is not {prefix}NN", i.id).into());
        }
        check_refs(i, &ctx.fish_len)?;
        // A made-up id would silently drop the issue from GO and from the
        // author's to-do list.
        if let Some(prev) = &i.rejected_as
            && !ctx.rejected.iter().any(|r| &r.id == prev)
        {
            return Err(format!("{}: rejected_as {prev} is not an earlier rejection", i.id).into());
        }
    }
    let mut ids: Vec<&str> = out.issues.iter().map(|i| i.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != out.issues.len() {
        return Err("duplicate issue ids".into());
    }
    if out.key_decisions.len() != ctx.key_items {
        return Err(format!("scored {} key decisions, list has {}", out.key_decisions.len(), ctx.key_items).into());
    }
    if out.spec_checks.len() != ctx.spec_items {
        return Err(format!("copied {} spec checks, checklist has {}", out.spec_checks.len(), ctx.spec_items).into());
    }
    Ok(out)
}

fn round_number(round_dir: &Path) -> Result<u32> {
    round_dir
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("round-"))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("not a round dir: {}", round_dir.display()).into())
}

/// Runs whatever is not done yet in `round_dir`. `log` gets progress lines.
pub async fn run(settings: &Settings, round_dir: &Path, mut log: impl FnMut(String)) -> Result<RoundResult> {
    let round = round_number(round_dir)?;
    let prd_dir = round_dir.parent().ok_or("round dir has no parent")?;
    let rules = round_dir.join("rules");
    let prd = read(&round_dir.join("prd.md"))?;
    let key_decisions = read(&round_dir.join("key-decisions.md"))?;
    let goldfish_rules = read(&rules.join("hd-goldfish-rules.md"))?;
    let elephant_rules = read(&rules.join("hd-elephant-rules.md"))?;
    let checklist = spec_checklist(&read(&rules.join("hd-prd-rules.md"))?)?;
    let spec_items = checklist.lines().count();
    let mut state = read_state(round_dir)?;
    let fish_dir = round_dir.join("fish");
    fs::create_dir_all(&fish_dir)?;

    // Models frozen at round start; older rounds use the current settings.
    let models = state.config.clone().unwrap_or_else(|| settings.models());

    // Fish stage.
    let cfg = CodexConfig {
        home: settings.codex_home(),
        model: models.goldfish_model.clone(),
        effort: models.goldfish_effort.clone(),
    };
    let mut set = JoinSet::new();
    for (name, schema) in FISH {
        if state.fish.get(name).is_some_and(|s| s.status == StepStatus::Done) {
            continue;
        }
        let prompt = template(round_dir, name)?
            .replace("{{goldfish_rules}}", goldfish_rules.trim())
            .replace("{{checklist}}", &checklist)
            .replace("{{prd}}", prd.trim());
        if let Some(step) = state.fish.get_mut(name) {
            step.status = StepStatus::Running;
            step.error = None;
        }
        set.spawn(run_fish(name, prompt, schema, spec_items, cfg.clone()));
        log(format!("{name}: running"));
    }
    if !set.is_empty() {
        state.stage = Stage::Fish;
        write_state(round_dir, &state)?;
    }
    while let Some(joined) = set.join_next().await {
        let (name, retries, result) = joined?;
        let step = state.fish.get_mut(name).ok_or("unknown goldfish")?;
        step.retries += retries;
        match result {
            Ok(v) => {
                write_json(&fish_dir.join(format!("{name}.json")), &v)?;
                step.status = StepStatus::Done;
                log(format!("{name}: done"));
            }
            Err(e) => {
                step.status = StepStatus::Failed;
                step.error = Some(e.to_string());
                log(format!("{name}: failed: {e}"));
            }
        }
        write_state(round_dir, &state)?;
    }
    let failed: Vec<&String> = state.fish.iter().filter(|(_, s)| s.status != StepStatus::Done).map(|(n, _)| n).collect();
    if !failed.is_empty() {
        state.stage = Stage::Failed;
        write_state(round_dir, &state)?;
        return Err(format!("goldfish failed: {failed:?}; fix the cause, then run `egm resume`").into());
    }

    // Triage stage.
    let triage_path = round_dir.join("triage.json");
    if state.triage.status != StepStatus::Done {
        let rejections = collect_rejections(prd_dir, round)?;
        write_json(&round_dir.join("rejected-history.json"), &rejections)?;
        let rejected = if rejections.is_empty() { "无".to_string() } else { serde_json::to_string_pretty(&rejections)? };
        let fish_text = |n: &str| read(&fish_dir.join(format!("{n}.json")));
        let prompt = template(round_dir, "triage")?
            .replace("{{round}}", &round.to_string())
            .replace("{{elephant_rules}}", elephant_rules.trim())
            .replace("{{key_decisions}}", key_decisions.trim())
            .replace("{{rejected}}", &rejected)
            .replace("{{retell}}", fish_text("retell")?.trim())
            .replace("{{critic}}", fish_text("critic")?.trim())
            .replace("{{ready}}", fish_text("ready")?.trim())
            .replace("{{spec}}", fish_text("spec")?.trim())
            .replace("{{prd}}", prd.trim());
        let fish_len = [
            ("retell", serde_json::from_str::<Retell>(&fish_text("retell")?)?.guesses.len()),
            ("critic", serde_json::from_str::<Critic>(&fish_text("critic")?)?.issues.len()),
            ("ready", serde_json::from_str::<Ready>(&fish_text("ready")?)?.missing.len()),
        ];
        let inputs = TriageInputs {
            round,
            key_items: key_decisions.lines().filter(|l| l.starts_with("- ")).count(),
            spec_items,
            fish_len,
            rejected: &rejections,
        };
        let claude = ClaudeConfig { model: models.triager_model.clone() };

        state.stage = Stage::Triage;
        state.triage.status = StepStatus::Running;
        state.triage.error = None;
        write_state(round_dir, &state)?;
        log("triage: running".into());

        let mut result = Err::<TriageOutput, crate::Error>("not run".into());
        for attempt in 0..2 {
            result = match agents::claude(&claude, TRIAGE_SYSTEM, prompt.clone(), TRIAGE_SCHEMA).await {
                Ok(v) => validate_triage(&v, &inputs),
                Err(e) => Err(e),
            };
            let retryable = matches!(&result, Err(e) if e.downcast_ref::<agents::ProcessError>().is_none());
            if result.is_ok() || !retryable {
                break;
            }
            state.triage.retries = attempt + 1;
        }
        match result {
            Ok(out) => {
                write_json(&triage_path, &Triage::from_output(round, out))?;
                state.triage.status = StepStatus::Done;
                state.stage = Stage::Decide;
                write_state(round_dir, &state)?;
                // First verdicts right away, so a round needing no decision
                // still reaches a conclusion (PRD §6.6).
                state.verdicts = Some(crate::review::refresh_verdicts(round_dir)?);
                log("triage: done".into());
            }
            Err(e) => {
                state.triage.status = StepStatus::Failed;
                state.triage.error = Some(e.to_string());
                state.stage = Stage::Failed;
                write_state(round_dir, &state)?;
                return Err(format!("triage failed: {e}; run `egm resume` to retry").into());
            }
        }
    }

    let triage: Triage = serde_json::from_str(&read(&triage_path)?)?;
    let verdicts = match state.verdicts {
        Some(v) => v,
        None => crate::review::refresh_verdicts(round_dir)?,
    };
    Ok(RoundResult { dir: round_dir.to_path_buf(), triage, verdicts, warnings: state.warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Role, Severity};
    use crate::testutil::{dec, issue, triage_in, TempDir};

    fn save_round(prd_dir: &Path, n: u32, issues: Vec<crate::model::Issue>, decisions: Option<Vec<Decision>>) {
        let dir = prd_dir.join(format!("round-{n}"));
        fs::create_dir_all(&dir).unwrap();
        write_json(&dir.join("triage.json"), &triage_in(n, issues, 1, 1)).unwrap();
        if let Some(d) = decisions {
            write_json(&dir.join("decisions.json"), &d).unwrap();
        }
    }

    fn ids(r: &[Rejection]) -> Vec<&str> {
        r.iter().map(|x| x.id.as_str()).collect()
    }

    #[test]
    fn a_round_uses_its_frozen_template_else_the_built_in_one() {
        let tmp = TempDir::new("template");
        let p = tmp.path();
        assert_eq!(template(p, "critic").unwrap(), PROMPT_TEMPLATES[1].1);
        fs::create_dir_all(p.join("prompts")).unwrap();
        fs::write(p.join("prompts/critic.md"), "frozen {{prd}}").unwrap();
        assert_eq!(template(p, "critic").unwrap(), "frozen {{prd}}");
        assert_eq!(template(p, "triage").unwrap(), PROMPT_TEMPLATES[4].1);
        assert!(template(p, "nope").is_err());
    }

    #[test]
    fn rejections_carry_over_until_the_author_accepts_a_match() {
        let tmp = TempDir::new("reject");
        let p = tmp.path();
        let i = |id: &str, prev: Option<&str>| issue(id, Role::Critic, &[], Severity::High, prev);
        let mut noted = dec("r1-01", Advice::Reject);
        noted.note = Some("设计如此".into());
        save_round(
            p,
            1,
            vec![i("r1-01", None), i("r1-02", None), i("r1-03", None)],
            Some(vec![noted, dec("r1-02", Advice::Accept), dec("r1-03", Advice::Reject)]),
        );
        let r = collect_rejections(p, 2).unwrap();
        assert_eq!(ids(&r), vec!["r1-01", "r1-03"]);
        assert_eq!(r[0].note.as_deref(), Some("设计如此"));

        // Round 2: r2-01 matched r1-01 and the author accepted it this time;
        // r2-02 matched r1-03 and was left alone (auto-rejected, no decision).
        save_round(
            p,
            2,
            vec![i("r2-01", Some("r1-01")), i("r2-02", Some("r1-03")), i("r2-03", None)],
            Some(vec![dec("r2-01", Advice::Accept), dec("r2-03", Advice::Reject)]),
        );
        assert_eq!(ids(&collect_rejections(p, 3).unwrap()), vec!["r1-03", "r2-03"]);
    }

    #[test]
    fn rounds_without_decisions_are_skipped() {
        let tmp = TempDir::new("nodec");
        let i = issue("r1-01", Role::Ready, &[], Severity::Low, None);
        save_round(tmp.path(), 1, vec![i], None);
        assert!(collect_rejections(tmp.path(), 2).unwrap().is_empty());
    }

    #[test]
    fn checklist_takes_its_section_and_drops_unjudgeable_items() {
        let rules = "## 参考\n\n- [ ] not this\n\n## 交付前检查清单\n\n- [ ] 有里程碑表\n- [ ] 末尾有关键决策清单\n- [ ] 子页面（若有）不重复主 PRD\n- [ ] 项目做完后，标题加了 `[已完成]`\n\n## 参考\n- [ ] nor this\n";
        assert_eq!(spec_checklist(rules).unwrap(), "- 有里程碑表");
    }

    #[test]
    fn fish_validation_rejects_extra_fields_and_noteless_failures() {
        assert!(validate_fish("critic", r#"{"issues":[]}"#, 1).is_ok());
        assert!(validate_fish("critic", r#"{"issues":[],"extra":1}"#, 1).is_err());
        assert!(validate_fish("spec", r#"{"checks":[{"item":"a","pass":false,"note":""}]}"#, 1).is_err());
        assert!(validate_fish("spec", r#"{"checks":[{"item":"a","pass":false,"note":"n"}]}"#, 1).is_ok());
        assert!(validate_fish("spec", r#"{"checks":[{"item":"a","pass":true,"note":""}]}"#, 2).is_err());
        assert!(validate_fish("ready", r#"{"verdict":"GO","reason":"ok","missing":[]}"#, 1).is_ok());
    }

    fn triage_json(issue: Issue) -> Value {
        let mut t = serde_json::to_value(triage_in(3, vec![issue], 1, 1)).unwrap();
        let o = t.as_object_mut().unwrap();
        for k in ["round", "key_decision_hits", "key_decision_total"] {
            o.remove(k);
        }
        o.insert("spec_checks".into(), serde_json::json!([{ "item": "a", "pass": true }]));
        t
    }

    #[test]
    fn triage_must_trace_issues_to_goldfish_and_real_rejections() {
        let rejected = [Rejection {
            id: "r2-09".into(),
            section: "§1".into(),
            title: "t".into(),
            problem: "p".into(),
            note: None,
        }];
        let ctx = TriageInputs {
            round: 3,
            key_items: 1,
            spec_items: 1,
            fish_len: [("retell", 1), ("critic", 2), ("ready", 0)],
            rejected: &rejected,
        };
        let check = |edit: &dyn Fn(&mut Issue)| {
            let mut i = issue("r3-01", Role::Critic, &[Role::Retell], Severity::High, None);
            i.refs = vec!["critic#1".into(), "retell#0".into()];
            edit(&mut i);
            validate_triage(&triage_json(i), &ctx)
        };
        assert!(check(&|_| {}).is_ok());
        assert!(check(&|i| i.rejected_as = Some("r2-09".into())).is_ok());
        assert!(check(&|i| i.rejected_as = Some("r2-15".into())).is_err(), "made-up rejection");
        assert!(check(&|i| i.refs.clear()).is_err(), "no refs");
        assert!(check(&|i| i.refs[0] = "critic#2".into()).is_err(), "index out of range");
        assert!(check(&|i| i.refs[0] = "critic[1]".into()).is_err(), "bad ref format");
        assert!(check(&|i| i.also_from.clear()).is_err(), "retell ref but retell not in also_from");
        assert!(check(&|i| i.id = "r3-1".into()).is_err(), "id is not two digits");
        assert!(check(&|i| i.id = "r2-01".into()).is_err(), "wrong round");

        let mut spec_short = triage_json(issue("r3-01", Role::Critic, &[], Severity::Low, None));
        spec_short["issues"][0]["refs"] = serde_json::json!(["critic#0"]);
        assert!(validate_triage(&spec_short, &ctx).is_ok());
        spec_short["spec_checks"] = serde_json::json!([]);
        assert!(validate_triage(&spec_short, &ctx).is_err(), "spec checks missing");
    }
}
