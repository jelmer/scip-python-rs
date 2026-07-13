use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use scip_python::{Options, index_project, project_metadata};

/// SCIP indexer for Python.
#[derive(Parser)]
#[command(name = "scip-python", version)]
struct Cli {
    /// Project root directory.
    #[arg(default_value = ".")]
    dir: PathBuf,
    /// Package name recorded in emitted symbols; defaults to the name
    /// from pyproject.toml or the directory name.
    #[arg(long)]
    project_name: Option<String>,
    /// Package version recorded in emitted symbols; defaults to the
    /// version from pyproject.toml.
    #[arg(long)]
    project_version: Option<String>,
    /// Output path for the index.
    #[arg(short, long, default_value = "index.scip")]
    output: PathBuf,
    /// Disable type-inference-based reference resolution.
    #[arg(long)]
    no_infer: bool,
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let metadata = if cli.project_name.is_some() && cli.project_version.is_some() {
        Default::default()
    } else {
        project_metadata(&cli.dir)?
    };
    let project_name = cli
        .project_name
        .or(metadata.name)
        .or_else(|| {
            cli.dir
                .canonicalize()
                .ok()?
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let project_version = cli
        .project_version
        .or(metadata.version)
        .unwrap_or_else(|| "0.0".to_string());
    let result = index_project(&Options {
        project_root: cli.dir,
        project_name,
        project_version,
        infer: !cli.no_infer,
    })?;
    for failure in &result.errors {
        eprintln!(
            "warning: failed to parse {}: {}",
            failure.path.display(),
            failure.message
        );
    }
    let documents = result.index.documents.len();
    scip::write_message_to_file(&cli.output, result.index)
        .map_err(|e| anyhow::anyhow!("cannot write {}: {}", cli.output.display(), e))?;
    eprintln!(
        "indexed {} file{} ({} parse failure{}) -> {}",
        documents,
        if documents == 1 { "" } else { "s" },
        result.errors.len(),
        if result.errors.len() == 1 { "" } else { "s" },
        cli.output.display()
    );
    Ok(if result.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
