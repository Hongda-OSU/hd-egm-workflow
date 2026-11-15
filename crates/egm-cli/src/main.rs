//! `egm` — command-line front end for M1–M3, before the Tauri UI exists.

use std::path::Path;

use egm_core::markdown::render;
use egm_core::model::{Advice, Severity, Verdict};
use egm_core::notion::Notion;
use egm_core::round::{self, RoundResult};
use egm_core::settings::Settings;
use egm_core::store::{self, Lock};
use egm_core::Result;

const USAGE: &str = "usage:
  egm list                 list PRDs under the project page
  egm pull <page-id>       pull a new round snapshot into the data dir
  egm run <page-id>        pull a new round, run 4 goldfish + triager
  egm resume <page-id>     finish the latest round (failed or unfinished steps)
  egm convert <page-id>    print a page as Markdown (no stripping)";

async fn run_round(settings: &Settings, round_dir: &Path) -> Result<()> {
    let prd_dir = round_dir.parent().ok_or("round dir has no parent")?;
    let _lock = Lock::acquire(prd_dir)?;
    println!("running {} (goldfish take a few minutes)", round_dir.display());
    let result = round::run(settings, round_dir, |line| println!("  {line}")).await?;
    print_summary(&result);
    Ok(())
}

fn print_summary(r: &RoundResult) {
    let t = &r.triage;
    let v = |x: Verdict| if x == Verdict::Go { "GO" } else { "NO-GO" };
    let rv = &r.verdicts;
    let count = |f: &dyn Fn(&egm_core::model::Issue) -> bool| t.issues.iter().filter(|i| f(i)).count();
    println!("\nround {} -> {}", t.round, r.dir.join("triage.json").display());
    println!(
        "  retell {} (key decisions {}/{} = {:.0}%), critic {}, ready {}",
        v(rv.retell),
        t.key_decision_hits,
        t.key_decision_total,
        t.hit_rate() * 100.0,
        v(rv.critic),
        v(rv.ready)
    );
    println!(
        "  issues {} (high {}, medium {}, low {}); advice accept {}, reject {}; previously rejected {}",
        t.issues.len(),
        count(&|i| i.severity == Severity::High),
        count(&|i| i.severity == Severity::Medium),
        count(&|i| i.severity == Severity::Low),
        count(&|i| i.advice == Advice::Accept && i.rejected_as.is_none()),
        count(&|i| i.advice == Advice::Reject && i.rejected_as.is_none()),
        count(&|i| i.rejected_as.is_some()),
    );
    let passed = t.spec_checks.iter().filter(|c| c.pass).count();
    println!("  spec {passed}/{} pass", t.spec_checks.len());
    for w in &r.warnings {
        println!("  warning: {w}");
    }
    println!("  overall: {}", if r.overall_go() { "GO" } else { "NO-GO" });
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let page_arg = || args.get(1).cloned().ok_or_else(|| USAGE.to_string());
    match args.first().map(String::as_str) {
        Some("list") => {
            let settings = Settings::load()?;
            let notion = Notion::from_keychain()?;
            for e in store::list(&notion, &settings).await? {
                let status = match &e.meta {
                    Some(m) if m.status == "go" => format!("GO (round {})", m.rounds),
                    Some(m) => format!("round {}", m.rounds),
                    None => "not reviewed".into(),
                };
                println!("{}  {}  [{}]", e.page_id, e.title, status);
            }
        }
        Some("pull") => {
            let page = page_arg()?;
            let settings = Settings::load()?;
            let notion = Notion::from_keychain()?;
            let pulled = store::pull(&notion, &settings, &page).await?;
            println!("round {} -> {}", pulled.round, pulled.dir.display());
            for w in &pulled.warnings {
                println!("warning: {w}");
            }
            if !pulled.warnings.is_empty() {
                println!("this round cannot be judged GO until the warnings are fixed");
            }
        }
        Some("run") => {
            let page = page_arg()?;
            let settings = Settings::load()?;
            let notion = Notion::from_keychain()?;
            let pulled = store::pull(&notion, &settings, &page).await?;
            println!("pulled round {} -> {}", pulled.round, pulled.dir.display());
            for w in &pulled.warnings {
                println!("warning: {w}");
            }
            run_round(&settings, &pulled.dir).await?;
        }
        Some("resume") => {
            let page = page_arg()?;
            let settings = Settings::load()?;
            let prd_dir = store::find_prd_dir(&settings.data_dir(), &page)
                .ok_or("no local rounds for this PRD; use `egm run`")?;
            let round_dir = store::latest_round(&prd_dir).ok_or("no rounds yet; use `egm run`")?;
            run_round(&settings, &round_dir).await?;
        }
        Some("convert") => {
            let page = page_arg()?;
            let notion = Notion::from_keychain()?;
            let rendered = render(&notion.tree(&page).await?);
            print!("{}", rendered.markdown);
            for w in &rendered.warnings {
                eprintln!("warning: {w}");
            }
        }
        _ => println!("{USAGE}"),
    }
    Ok(())
}
