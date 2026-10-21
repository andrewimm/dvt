mod exclude;
mod filetype;
mod fmt;
mod glob;
mod layout;
mod scan;
mod search;
mod tree;
mod ui;
mod watch;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, bail};
use clap::Parser;

use crate::exclude::Excludes;
use crate::scan::Msg;
use crate::tree::{ROOT, Tree, flags};

/// dvt - disk view tree. A read-only terminal disk usage visualizer.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Directory to scan
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Stay on one filesystem (always on when scanning `/`)
    #[arg(short = 'x', long)]
    one_file_system: bool,

    /// Skip entries matching PATTERN (repeatable). Without a `/` it matches
    /// names at any depth (`node_modules`, `*.iso`); with one, full paths
    /// (`/proc`, `/home/*/.cache`). A trailing `/` matches only directories.
    #[arg(short, long, value_name = "PATTERN")]
    exclude: Vec<String>,

    /// Ignore the exclude file (~/.config/dvt/exclude, one pattern per line)
    #[arg(long)]
    no_exclude_file: bool,

    /// Don't watch for filesystem changes (toggle with `w` in the UI)
    #[arg(long)]
    no_watch: bool,

    /// Print a du-style summary of the top level instead of starting the UI
    #[arg(long)]
    print: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    scan::init();
    let root = scan::canonicalize(&cli.path)
        .with_context(|| format!("cannot open {}", cli.path.display()))?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let mut patterns = Vec::new();
    if !cli.no_exclude_file
        && let Some(file) = exclude::config_file()
    {
        patterns = exclude::read_file(&file)
            .with_context(|| format!("cannot read exclude file {}", file.display()))?;
    }
    patterns.extend(cli.exclude);
    let opts = scan::Options {
        one_file_system: cli.one_file_system || root.parent().is_none(),
        excludes: Arc::new(Excludes::new(&patterns)),
    };

    if cli.print {
        print_summary(root, opts);
        Ok(())
    } else {
        ui::run(root, opts, !cli.no_watch)
    }
}

fn print_summary(root: PathBuf, opts: scan::Options) {
    let start = Instant::now();
    let mut tree = Tree::new(root.clone());
    let (tx, rx) = crossbeam_channel::unbounded();
    let req = scan::Request {
        path: root,
        id: ROOT,
        skip: None,
        send_root: true,
    };
    scan::spawn(req, tree.ids.clone(), opts, &scan::pool(), tx);
    for msg in rx {
        let done = matches!(msg, Msg::Done);
        tree.apply(msg);
        if done {
            break;
        }
    }

    for id in tree.sorted_children(ROOT) {
        let n = tree.get(id);
        let slash = if n.is_dir() { "/" } else { "" };
        let note = if n.has(flags::EXCLUDED) {
            "  (excluded)"
        } else {
            ""
        };
        println!(
            "{:>8}  {}{slash}{note}",
            fmt::size(n.disk),
            tree.name(id).to_string_lossy()
        );
    }
    let r = tree.get(ROOT);
    println!(
        "{:>8}  {} ({} bytes)",
        fmt::size(r.disk),
        tree.root_path().display(),
        r.disk
    );
    eprintln!(
        "{} items in {:.2?}, {} errors",
        fmt::count(r.items as u64),
        start.elapsed(),
        tree.errors
    );
}
