//! `egm` — command-line front end for M1–M3, before the Tauri UI exists.

use egm_core::markdown::render;
use egm_core::notion::Notion;
use egm_core::settings::Settings;
use egm_core::{store, Result};

const USAGE: &str = "usage:
  egm list                 list PRDs under the project page
  egm pull <page-id>       pull a new round snapshot into the data dir
  egm convert <page-id>    print a page as Markdown (no stripping)";

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
