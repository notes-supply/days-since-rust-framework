use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Result, ensure};
use clap::Parser;
use days_since_rust_framework::{
    discover,
    history::{self, BuildMode, History, ResetLedger, utc_date},
    site,
};
use jiff::{Timestamp, civil::Date};

#[derive(Parser)]
#[command(version, about = "days since the last rust framework")]
enum Command {
    Import {
        catalog: PathBuf,
    },
    Build {
        #[arg(long)]
        production: bool,
        #[arg(long)]
        today: Option<Date>,
        #[arg(long, default_value = "dist")]
        out: PathBuf,
    },
    Discover {
        #[arg(long)]
        since: Option<Date>,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Command::parse() {
        Command::Import { catalog } => {
            let history = history::import(&catalog)?;
            fs::create_dir_all("data")?;
            let mut json = serde_json::to_string_pretty(&history)?;
            json.push('\n');
            fs::write("data/history.json", json)?;
            println!(
                "Imported {} frameworks and {} not included into data/history.json",
                history.frameworks.len(),
                history.not_included.len()
            );
        }
        Command::Build {
            production,
            today,
            out,
        } => {
            let history = History::load("data/history.json".as_ref())?;
            let ledger = ResetLedger::load("data/resets.toml".as_ref())?;
            let mode = if production {
                BuildMode::Production
            } else {
                BuildMode::Preview
            };
            let today = today.unwrap_or_else(|| utc_date(Timestamp::now()));
            let sizes = site::build(&history, &ledger, mode, today, &out)?;
            println!("file                    raw  gzip(-9)  brotli(q11)");
            for size in sizes {
                println!(
                    "{:<22} {:>5} {:>9} {:>12}",
                    size.path, size.raw, size.gzip, size.brotli
                );
            }
        }
        Command::Discover { since, out } => {
            let history = History::load("data/history.json".as_ref())?;
            let since = match since {
                Some(date) => date,
                None => ResetLedger::load("data/resets.toml".as_ref())?.checked_through,
            };
            let (report, failed) = discover::run(&history, since);
            let mut json = serde_json::to_vec_pretty(&report)?;
            json.push(b'\n');
            match out {
                Some(path) => fs::write(path, json)?,
                None => io::stdout().lock().write_all(&json)?,
            }
            ensure!(!failed, "discovery had incomplete sources");
        }
    }
    Ok(())
}
