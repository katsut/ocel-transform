use std::error::Error;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use ocel_transform::{apply, Recipe};

/// Recipe-driven OCEL 2.0 log transformation.
///
/// Reads an OCEL log, applies the recipe's steps in order, and writes a new
/// OCEL log. Follows the connector contract: human diagnostics on stderr,
/// NDJSON progress events on stdout, exit 0 on success.
#[derive(Debug, Parser)]
#[command(name = "ocel-transform", version, about)]
struct Cli {
    /// Input OCEL file (.json/.jsonocel, .sqlite/.db, .xml/.xmlocel).
    #[arg(long = "in", value_name = "FILE")]
    input: PathBuf,
    /// Recipe JSON file.
    #[arg(long)]
    recipe: PathBuf,
    /// Output OCEL file.
    #[arg(long)]
    out: PathBuf,
}

// --- connector contract v2: NDJSON progress events on stdout -----------------

fn emit(value: &serde_json::Value) {
    println!("{value}");
}

fn emit_progress(stage: &str, done: usize, total: usize) {
    emit(&serde_json::json!({"event": "progress", "stage": stage, "done": done, "total": total}));
}

fn emit_log(message: &str) {
    emit(&serde_json::json!({"event": "log", "level": "info", "message": message}));
}

fn emit_done(events: usize, objects: usize) {
    emit(&serde_json::json!({"event": "done", "events": events, "objects": objects}));
}

// -----------------------------------------------------------------------------

fn run(cli: &Cli) -> Result<(), Box<dyn Error>> {
    let raw = std::fs::read_to_string(&cli.recipe)
        .map_err(|e| format!("cannot read recipe {}: {e}", cli.recipe.display()))?;
    let recipe: Recipe = serde_json::from_str(&raw)
        .map_err(|e| format!("invalid recipe {}: {e}", cli.recipe.display()))?;

    let log = ocel::io::read_path(&cli.input)?;
    eprintln!(
        "recipe '{}': {} steps on {} events / {} objects",
        recipe.name,
        recipe.steps.len(),
        log.events.len(),
        log.objects.len()
    );

    // relative file references in steps like `union` resolve next to the input
    let base_dir = match cli.input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let total = recipe.steps.len();
    let (transformed, reports) = apply(&recipe, log, base_dir)?;
    for (index, report) in reports.iter().enumerate() {
        let mut line = format!(
            "{}: events {} -> {}, objects {} -> {}",
            report.step,
            report.events_before,
            report.events_after,
            report.objects_before,
            report.objects_after
        );
        if let Some(skipped) = report.duplicates_skipped {
            write!(line, ", {skipped} duplicates skipped").expect("writing to a String");
        }
        if let Some(lifted) = report.events_lifted {
            write!(line, ", lifted {lifted} events").expect("writing to a String");
        }
        eprintln!("  {line}");
        emit_log(&line);
        emit_progress(&report.step, index + 1, total);
    }

    ocel::io::write_path(&transformed, &cli.out)?;
    eprintln!(
        "wrote {} ({} events / {} objects)",
        cli.out.display(),
        transformed.events.len(),
        transformed.objects.len()
    );
    emit_done(transformed.events.len(), transformed.objects.len());
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
