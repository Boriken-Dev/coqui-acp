//! The `coqui-acp-history` command: look at histories and move them between files.
//!
//! `export` and `import` need a harness's codec, which lives in that harness's crate; here they
//! say so and fail. `convert` rewrites a history in this format into another file of the same
//! format, unaltered unless redaction is asked for.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use coqui_acp_history::{
    Body, Bounds, Document, HistoryError, Reader, ReaderSettings, RedactionSettings, Redactor,
    non_defaults,
};

#[derive(Parser)]
#[command(
    name = "coqui-acp-history",
    version,
    about = "Look at and move Agent Client Protocol session histories"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The sizes a history is held to. Each is a setting with a default; a looser one is reported.
#[derive(Args)]
struct Limits {
    /// The longest line, in bytes.
    #[arg(long)]
    max_line_bytes: Option<u64>,
    /// The most lines.
    #[arg(long)]
    max_lines: Option<u64>,
    /// The most bytes in all.
    #[arg(long)]
    max_total_bytes: Option<u64>,
    /// The deepest nesting in a line.
    #[arg(long)]
    max_depth: Option<u64>,
}

impl Limits {
    fn settings(&self) -> ReaderSettings {
        let mut settings = ReaderSettings::default();
        let mut bounds = Bounds::default();
        bounds.max_line_bytes = self.max_line_bytes.unwrap_or(bounds.max_line_bytes);
        bounds.max_lines = self.max_lines.unwrap_or(bounds.max_lines);
        bounds.max_total_bytes = self.max_total_bytes.unwrap_or(bounds.max_total_bytes);
        bounds.max_depth = self.max_depth.unwrap_or(bounds.max_depth);
        settings.bounds = bounds;
        for setting in non_defaults(&settings) {
            eprintln!(
                "setting {} is {} (default {})",
                setting.setting, setting.value, setting.default
            );
        }
        settings
    }
}

#[derive(Subcommand)]
enum Command {
    /// List the histories in a directory (the `.jsonl` files), by their headers.
    List {
        /// The directory; the current one by default.
        dir: Option<PathBuf>,
        #[command(flatten)]
        limits: Limits,
    },
    /// Show a history: its header, what it holds, and its turns.
    Show {
        /// The history file.
        file: PathBuf,
        #[command(flatten)]
        limits: Limits,
    },
    /// Export a harness's session to a history (needs that harness's codec).
    Export {
        /// The harness: claude, codex, agy.
        #[arg(long)]
        harness: String,
        /// The session, in the harness's own id.
        session: String,
        /// Where to write the history.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import a history into a harness (needs that harness's codec).
    Import {
        /// The harness: claude, codex, agy.
        #[arg(long)]
        harness: String,
        /// The history file.
        file: PathBuf,
    },
    /// Write a history to another file of the same format: checked, and unaltered unless
    /// redaction is asked for.
    Convert {
        /// The history to read.
        input: PathBuf,
        /// The file to write; `-` for standard output.
        #[arg(short, long)]
        output: PathBuf,
        /// A regular expression whose matches are removed. Repeatable.
        #[arg(long = "redact-pattern")]
        redact_patterns: Vec<String>,
        /// A file of text to remove, one per line. A secret is read from a file, never from the
        /// command line.
        #[arg(long = "redact-values-file")]
        redact_values: Option<PathBuf>,
        /// What replaces each removal.
        #[arg(long)]
        replacement: Option<String>,
        /// Put a digest of the body in the header of what is written (a redacted history loses
        /// its digest otherwise).
        #[arg(long)]
        seal: bool,
        #[command(flatten)]
        limits: Limits,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, HistoryError> {
    match cli.command {
        Command::List { dir, limits } => {
            list(dir.as_deref().unwrap_or(Path::new(".")), &limits.settings())
        }
        Command::Show { file, limits } => show(&file, &limits.settings()),
        Command::Export { harness, .. } | Command::Import { harness, .. } => {
            eprintln!(
                "no codec for {harness} is built in: a harness's export and import live in its \
                 crate, behind that crate's `history` feature"
            );
            Ok(ExitCode::from(2))
        }
        Command::Convert {
            input,
            output,
            redact_patterns,
            redact_values,
            replacement,
            seal,
            limits,
        } => {
            let redaction = if redact_patterns.is_empty() && redact_values.is_none() {
                None
            } else {
                let mut settings = RedactionSettings::default();
                settings.patterns = redact_patterns;
                if let Some(path) = redact_values {
                    let text = fs::read_to_string(path)?;
                    settings.values = text
                        .lines()
                        .filter(|l| !l.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                if let Some(replacement) = replacement {
                    settings.replacement = replacement;
                }
                Some(settings)
            };
            convert(&input, &output, redaction, seal, &limits.settings())
        }
    }
}

fn open(path: &Path, settings: &ReaderSettings) -> Result<Reader<File>, HistoryError> {
    Reader::builder(File::open(path)?)
        .settings(settings.clone())
        .open()
}

fn list(dir: &Path, settings: &ReaderSettings) -> Result<ExitCode, HistoryError> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    paths.sort();
    for path in paths {
        match open(&path, settings) {
            Ok(reader) => {
                let header = reader.header();
                println!(
                    "{}  {}  {}  {}  {}",
                    header.id,
                    header.origin.harness,
                    header.model.as_deref().unwrap_or("-"),
                    header.cwd.as_deref().unwrap_or("-"),
                    path.display()
                );
            }
            Err(error) => println!(
                "-  not a history ({}: {error})  {}",
                error.code(),
                path.display()
            ),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn show(path: &Path, settings: &ReaderSettings) -> Result<ExitCode, HistoryError> {
    let document = Document::from_reader(open(path, settings)?)?;
    let header = &document.header;
    println!("session   {}", header.id);
    println!(
        "origin    {} {}",
        header.origin.harness,
        header.origin.version.as_deref().unwrap_or("")
    );
    for (name, value) in [
        ("origin id", &header.origin.session_id),
        ("model", &header.model),
        ("mode", &header.mode),
        ("effort", &header.effort),
        ("cwd", &header.cwd),
        ("created", &header.created_at),
    ] {
        if let Some(value) = value {
            println!("{name:<9} {value}");
        }
    }
    if let Some(redacted) = header.redacted {
        println!(
            "redacted  {} removals by {} rules",
            redacted.replaced, redacted.rules
        );
    }
    let summary = document.check()?;
    println!(
        "lines     {} ({} bytes), {} turns, {} updates, {} compactions{}",
        summary.lines,
        summary.bytes,
        summary.turns,
        summary.updates,
        summary.compactions,
        if summary.open_turn {
            ", ends inside a turn"
        } else {
            ""
        }
    );
    let seed = document.seed();
    if let Some(point) = &seed.point {
        println!(
            "seed      from line {} ({}), {} turns after it{}",
            point.line,
            if seed.summary.is_some() {
                "a compaction summary"
            } else {
                "the start"
            },
            seed.turns(),
            if seed.mid_turn {
                ", the first cut mid-turn"
            } else {
                ""
            }
        );
    }
    let mut turn = 0;
    for (at, line) in document.lines.iter().enumerate() {
        match &line.body {
            Body::Prompt { prompt } => {
                turn += 1;
                let text = prompt
                    .iter()
                    .find_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .unwrap_or("(no text)");
                let shown: String = text.chars().take(72).collect();
                println!(
                    "turn {turn:<4} line {:<6} {}",
                    at + 2,
                    shown.replace('\n', " ")
                );
            }
            Body::TurnEnd { stop, .. } => {
                println!("          line {:<6} ended: {}", at + 2, stop.name())
            }
            Body::Compaction { summary, .. } => println!(
                "          line {:<6} compaction{}",
                at + 2,
                if summary.is_some() {
                    " (with summary)"
                } else {
                    ""
                }
            ),
            _ => {}
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn convert(
    input: &Path,
    output: &Path,
    redaction: Option<RedactionSettings>,
    seal: bool,
    settings: &ReaderSettings,
) -> Result<ExitCode, HistoryError> {
    let reader = open(input, settings)?;
    let mut bytes = Vec::new();
    if redaction.is_none() && !seal {
        let summary = reader.copy_to(&mut bytes)?;
        eprintln!("copied {} lines unaltered", summary.lines);
    } else {
        let mut document = Document::from_reader(reader)?;
        if let Some(redaction) = redaction {
            let mut redactor = Redactor::try_new(&redaction)?;
            let report = redactor.document(&mut document)?;
            eprintln!(
                "redacted {} removals in {} lines",
                report.replaced, report.lines
            );
            for rule in &report.rules {
                eprintln!(
                    "  {:?} {}: {} removals in {} lines",
                    rule.kind, rule.index, rule.replaced, rule.lines
                );
            }
        }
        if seal {
            document.seal()?;
        }
        document.to_writer(&mut bytes)?;
    }
    write_whole(output, &bytes)?;
    Ok(ExitCode::SUCCESS)
}

/// Writes `bytes` to `output` whole or not at all: beside it first, then renamed over it.
fn write_whole(output: &Path, bytes: &[u8]) -> io::Result<()> {
    if output == Path::new("-") {
        return io::stdout().lock().write_all(bytes);
    }
    let mut temporary = output.as_os_str().to_owned();
    temporary.push(".part");
    let temporary = PathBuf::from(temporary);
    let written = File::create(&temporary).and_then(|file| {
        let mut file = BufWriter::new(file);
        file.write_all(bytes)?;
        file.flush()
    });
    match written.and_then(|()| fs::rename(&temporary, output)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}
