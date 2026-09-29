mod book;
mod config;
mod device;
mod discover;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use indicatif::{HumanBytes, ProgressBar, ProgressStyle};

use device::{Device, Entry, Firmware, join, mkdir_p, resolve, split};

pub type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[derive(Parser)]
#[command(
    name = "x4-cli",
    version,
    about = "Put books on an Xteink X4 running CrossPoint firmware, over Wi-Fi"
)]
struct Cli {
    /// Device address, e.g. 192.168.3.3 or crosspoint.local [default: config file, then discovery]
    #[arg(long, global = true, env = "X4_HOST")]
    host: Option<String>,
    /// Print raw device requests and responses to stderr
    #[arg(long, global = true)]
    trace: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show firmware, IP and free heap (re-detects the firmware)
    Status,
    /// List a folder on the X4 [default: the books folder from the config, /Books]
    Ls {
        dir: Option<String>,
        /// Show sizes
        #[arg(short, long)]
        long: bool,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Upload .epub .txt .xtc .xtch .bmp files; EPUBs get renamed to "Author - Title.epub"
    Add {
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Folder on the X4 [default: the books folder]
        #[arg(long)]
        to: Option<String>,
        #[command(flatten)]
        naming: Naming,
    },
    /// Upload every book in a local folder that the X4 is missing; subfolders become folders on the X4
    Sync {
        /// Local folder with your books [default: `library` from the config]
        dir: Option<PathBuf>,
        /// Remember DIR as the default folder
        #[arg(long)]
        save: bool,
        /// Folder on the X4 [default: the books folder]
        #[arg(long)]
        to: Option<String>,
        /// Also delete books on the X4 that are not in the local folder
        #[arg(long)]
        delete: bool,
        /// Don't ask before deleting
        #[arg(short, long)]
        yes: bool,
        #[command(flatten)]
        naming: Naming,
    },
    /// Delete files or folders; names and globs (* ?) match case-insensitively
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
        /// Delete folders and everything in them
        #[arg(short, long)]
        recursive: bool,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Download a file
    Pull {
        name: String,
        /// Local file or folder [default: current folder]
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Rename a file or move it into a folder
    Mv { name: String, dest: String },
    /// Look for an X4: UDP broadcast, mDNS, then the CrossPoint hotspot IP
    Find {
        /// Save the first device found as the default host
        #[arg(long)]
        save: bool,
    },
}

#[derive(Args)]
struct Naming {
    /// Keep the local file name instead of the EPUB metadata
    #[arg(long)]
    raw_name: bool,
    /// Replace a file of the same name but a different size
    #[arg(long)]
    force: bool,
    /// Use `_` for spaces and `-` between author and title (config: no_spaces = true)
    #[arg(long)]
    no_spaces: bool,
}

const NOT_FOUND: &str = "no X4 found. Open File Transfer on the X4 (it only joins 2.4 GHz Wi-Fi), or pass --host / set X4_HOST.";

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("x4-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut cli: Cli) -> Result<()> {
    let cfg = config::load()?;
    if let Cmd::Find { save } = cli.cmd {
        return find(save, cli.trace);
    }
    // Settle the local folder before touching the network, so mistakes fail fast.
    if let Cmd::Sync { dir, save, .. } = &mut cli.cmd {
        *dir = Some(library(dir.take(), *save, cfg.library.as_deref())?);
    }
    if let Cmd::Add { naming, .. } | Cmd::Sync { naming, .. } = &mut cli.cmd {
        naming.no_spaces |= cfg.no_spaces;
    }
    let host = match cli.host.or(cfg.host) {
        Some(h) => h,
        None => {
            let found = discover::find(false, cli.trace)
                .into_iter()
                .next()
                .ok_or(NOT_FOUND)?;
            eprintln!(
                "x4-cli: using the X4 at {} (found via {}; `x4-cli find --save` remembers it)",
                found.host, found.via
            );
            found.host
        }
    };
    let dev = device::connect(&host, cli.trace, matches!(cli.cmd, Cmd::Status))?;
    let dev = dev.as_ref();
    let base = cfg.dir.as_str();
    match cli.cmd {
        Cmd::Status => status(dev),
        Cmd::Ls { dir, long, json } => ls(
            dev,
            &resolve(base, dir.as_deref().unwrap_or("")),
            long,
            json,
        ),
        Cmd::Add { files, to, naming } => add(
            dev,
            &files,
            &resolve(base, to.as_deref().unwrap_or("")),
            &naming,
        ),
        Cmd::Sync {
            dir,
            to,
            delete,
            yes,
            naming,
            ..
        } => sync(
            dev,
            &dir.expect("resolved above"),
            &resolve(base, to.as_deref().unwrap_or("")),
            &naming,
            delete,
            yes,
        ),
        Cmd::Rm {
            names,
            recursive,
            yes,
        } => rm(dev, base, &names, recursive, yes),
        Cmd::Pull { name, output } => pull(dev, &resolve(base, &name), output),
        Cmd::Mv { name, dest } => mv(dev, base, &name, &dest),
        Cmd::Find { .. } => unreachable!("handled above"),
    }
}

/// The local folder `sync` reads: the argument, else `library` from the config.
fn library(arg: Option<PathBuf>, save: bool, configured: Option<&str>) -> Result<PathBuf> {
    let dir = match (arg, configured) {
        (Some(d), _) => d,
        (None, Some(c)) => match (c.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => Path::new(&home).join(rest),
            _ => PathBuf::from(c),
        },
        (None, None) => {
            return Err(
                "which folder holds your books? Run `x4-cli sync <folder> --save` once and x4-cli remembers it."
                    .into(),
            );
        }
    };
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if save {
        let p = config::save("library", &dir.to_string_lossy())?;
        println!("saved library = \"{}\" in {}", dir.display(), p.display());
    }
    Ok(dir)
}

fn status(dev: &dyn Device) -> Result<()> {
    let firmware = match dev.firmware() {
        Firmware::CrossPoint => "CrossPoint",
        Firmware::Stock => "stock",
    };
    println!("{:<10} {firmware}", "firmware");
    println!("{:<10} {}", "host", dev.host());
    println!("{:<10} yes", "reachable");
    for (k, v) in dev.status()? {
        println!("{k:<10} {v}");
    }
    Ok(())
}

fn ls(dev: &dyn Device, dir: &str, long: bool, json: bool) -> Result<()> {
    let mut entries = dev.list(dir)?;
    entries.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    for e in &entries {
        let name = if e.is_dir {
            format!("{}/", e.name)
        } else {
            e.name.clone()
        };
        if long {
            let size = if e.is_dir {
                "-".to_string()
            } else {
                HumanBytes(e.size).to_string()
            };
            println!("{size:>10}  {name}");
        } else {
            println!("{name}");
        }
    }
    Ok(())
}

enum Action {
    Upload,
    Replace(String),
    Skip,
}

#[derive(Default)]
struct Tally {
    added: usize,
    skipped: usize,
    failed: Vec<String>,
}

// ponytail: fixed budget; the X4 came back within seconds each time it dropped.
const ATTEMPTS: usize = 3;
const COMEBACK: Duration = Duration::from_secs(60);

fn add(dev: &dyn Device, files: &[PathBuf], dir: &str, naming: &Naming) -> Result<()> {
    if let Some(f) = files.iter().find(|f| !book::is_book(f)) {
        return Err(format!(
            "{}: unsupported format. x4-cli takes .{}",
            f.display(),
            book::FORMATS.join(" .")
        )
        .into());
    }
    let mut tally = Tally::default();
    put_files(dev, files, dir, naming, false, &mut tally)?;
    match tally.failed.len() {
        0 => Ok(()),
        n => Err(format!(
            "{n} of {} files failed: {}",
            files.len(),
            tally.failed.join(", ")
        )
        .into()),
    }
}

/// Uploads `files` into `dir`. A file the X4 refuses is reported and skipped;
/// losing the X4 for good stops everything, since each later file would wait too.
fn put_files(
    dev: &dyn Device,
    files: &[PathBuf],
    dir: &str,
    naming: &Naming,
    quiet_skips: bool,
    tally: &mut Tally,
) -> Result<()> {
    let mut plan = Vec::new();
    for f in files {
        let size = std::fs::metadata(f)
            .map_err(|e| format!("{}: {e}", f.display()))?
            .len();
        plan.push((
            f,
            book::target_name(f, naming.raw_name, naming.no_spaces),
            size,
        ));
    }

    mkdir_p(dev, dir)?;
    let mut remote = dev.list(dir)?;
    let mut actions = Vec::new();
    let mut conflicts = Vec::new();
    for (f, name, size) in plan {
        let same = |e: &Entry| !e.is_dir && e.name.to_lowercase() == name.to_lowercase();
        let action = match remote.iter().find(|e| same(e)) {
            None => Action::Upload,
            Some(e) if e.size == size => Action::Skip,
            Some(e) if naming.force => Action::Replace(e.name.clone()),
            Some(e) => {
                conflicts.push(format!(
                    "{} is already on the X4 with a different size ({size} bytes here, {} there)",
                    join(dir, &name),
                    e.size
                ));
                continue;
            }
        };
        if !matches!(action, Action::Skip) {
            remote.retain(|e| !same(e));
            remote.push(Entry {
                name: name.clone(),
                size,
                is_dir: false,
            });
        }
        actions.push((f, name, size, action));
    }
    if !conflicts.is_empty() {
        for c in &conflicts {
            eprintln!("x4-cli: {c}");
        }
        return Err("nothing uploaded. Pass --force to replace those files.".into());
    }

    for (f, name, size, action) in actions {
        let path = join(dir, &name);
        let old = match &action {
            Action::Skip => {
                tally.skipped += 1;
                if !quiet_skips {
                    println!("skip   {path} (same name and size already on the X4)");
                }
                continue;
            }
            Action::Replace(old) => Some(old.as_str()),
            Action::Upload => None,
        };
        let data = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let bar = ProgressBar::new(size)
            .with_style(bar_style()?)
            .with_message(name.clone());
        let res = upload_with_retry(dev, dir, &name, &data, &bar, old);
        bar.finish_and_clear();
        let note = if old.is_some() {
            "\n  The old copy was already deleted."
        } else {
            ""
        };
        match res {
            Ok(()) => {
                println!("added  {path}");
                tally.added += 1;
            }
            Err(e) if device::is_transient(e.as_ref()) => {
                return Err(format!(
                    "{e}{note}\n  The X4 did not come back. Run the same command again to upload the rest."
                )
                .into());
            }
            Err(e) => {
                eprintln!("x4-cli: {path}: {e}{note}");
                tally.failed.push(path);
            }
        }
    }
    Ok(())
}

/// Rides out Wi-Fi drops: waits for the X4 to come back, clears what the broken
/// attempt left on the card, and tries again.
fn upload_with_retry(
    dev: &dyn Device,
    dir: &str,
    name: &str,
    data: &[u8],
    bar: &ProgressBar,
    replace: Option<&str>,
) -> Result<()> {
    if let Some(old) = replace {
        dev.delete(&[join(dir, old)])?;
    }
    let mut attempt = 1;
    loop {
        let err = match dev.upload(dir, name, data, bar) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if !device::is_transient(err.as_ref()) || attempt == ATTEMPTS {
            return Err(err);
        }
        attempt += 1;
        bar.suspend(|| {
            eprintln!(
                "x4-cli: {name}: {err}\n  waiting up to {} s for the X4 to come back (attempt {attempt} of {ATTEMPTS})",
                COMEBACK.as_secs()
            )
        });
        if !device::wait_for(dev.host(), COMEBACK) {
            return Err(err);
        }
        // A broken upload can leave a partial file, or a complete one whose DONE got lost.
        let left = dev
            .list(dir)?
            .into_iter()
            .find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name));
        if let Some(e) = left {
            if e.size == data.len() as u64 {
                return Ok(());
            }
            dev.delete(&[join(dir, &e.name)])?;
        }
        bar.set_position(0);
    }
}

/// Uploads every book under `local` that the X4 lacks, mirroring subfolders under `to`.
fn sync(
    dev: &dyn Device,
    local: &Path,
    to: &str,
    naming: &Naming,
    delete: bool,
    yes: bool,
) -> Result<()> {
    let mut folders = Vec::new();
    let mut not_books = Vec::new();
    collect(local, to, naming.no_spaces, &mut folders, &mut not_books)?;
    let mut tally = Tally::default();
    for (dir, files) in &folders {
        put_files(dev, files, dir, naming, true, &mut tally)?;
    }
    let deleted = if delete {
        prune(dev, &folders, naming, yes)?
    } else {
        0
    };

    let mut summary = format!(
        "sync: {} added, {} already on the X4",
        tally.added, tally.skipped
    );
    if delete {
        summary += &format!(", {deleted} deleted");
    }
    if !tally.failed.is_empty() {
        summary += &format!(", {} failed", tally.failed.len());
    }
    println!("{summary}");
    if !not_books.is_empty() {
        let names: Vec<String> = not_books
            .iter()
            .map(|p| p.strip_prefix(local).unwrap_or(p).display().to_string())
            .collect();
        println!("left alone, not a book format: {}", names.join(", "));
    }
    match tally.failed.len() {
        0 => Ok(()),
        n => Err(format!("{n} files failed: {}", tally.failed.join(", ")).into()),
    }
}

/// Lists book files per folder, depth first; hidden entries are skipped.
fn collect(
    local: &Path,
    dir: &str,
    no_spaces: bool,
    folders: &mut Vec<(String, Vec<PathBuf>)>,
    not_books: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries = std::fs::read_dir(local)
        .map_err(|e| format!("{}: {e}", local.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    let mut files = Vec::new();
    let mut subdirs = Vec::new();
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        if name.starts_with('.') {
            continue;
        }
        // Symlinked folders are not followed, so a link loop can't recurse forever.
        if e.file_type()?.is_dir() {
            subdirs.push((path, join(dir, &book::folder_name(&name, no_spaces))));
        } else if !path.is_file() {
            continue;
        } else if book::is_book(&path) {
            files.push(path);
        } else {
            not_books.push(path);
        }
    }
    folders.push((dir.to_string(), files));
    for (path, sub) in subdirs {
        collect(&path, &sub, no_spaces, folders, not_books)?;
    }
    Ok(())
}

/// Deletes books on the X4 that have no local counterpart. Only book formats are
/// touched, so firmware files like update.bin survive.
fn prune(
    dev: &dyn Device,
    folders: &[(String, Vec<PathBuf>)],
    naming: &Naming,
    yes: bool,
) -> Result<usize> {
    let mut extra = Vec::new();
    for (dir, files) in folders {
        let wanted: HashSet<String> = files
            .iter()
            .map(|f| book::target_name(f, naming.raw_name, naming.no_spaces).to_lowercase())
            .collect();
        for e in dev.list(dir)? {
            if !e.is_dir
                && book::is_book(Path::new(&e.name))
                && !wanted.contains(&e.name.to_lowercase())
            {
                extra.push(join(dir, &e.name));
            }
        }
    }
    if extra.is_empty() {
        return Ok(0);
    }
    for p in &extra {
        println!("delete {p} (not in the local folder)");
    }
    if !yes && !confirm(&format!("Delete {} book(s) from the X4?", extra.len()))? {
        println!("Nothing deleted.");
        return Ok(0);
    }
    dev.delete(&extra)?;
    Ok(extra.len())
}

fn bar_style() -> Result<ProgressStyle> {
    Ok(ProgressStyle::with_template(
        "{msg} [{bar:30}] {bytes}/{total_bytes} {binary_bytes_per_sec}",
    )?
    .progress_chars("=> "))
}

fn rm(dev: &dyn Device, base: &str, names: &[String], recursive: bool, yes: bool) -> Result<()> {
    let mut listings: HashMap<String, Vec<Entry>> = HashMap::new();
    let mut targets: Vec<(String, bool)> = Vec::new();
    for arg in names {
        let (parent, pattern) = split(&resolve(base, arg));
        if pattern.is_empty() {
            return Err("refusing to delete /".into());
        }
        if !listings.contains_key(&parent) {
            listings.insert(parent.clone(), dev.list(&parent)?);
        }
        let pattern = pattern.to_lowercase();
        let hits: Vec<&Entry> = listings[&parent]
            .iter()
            .filter(|e| glob(&pattern, &e.name.to_lowercase()))
            .collect();
        if hits.is_empty() {
            return Err(format!("no match for {arg} in {parent}").into());
        }
        for e in hits {
            let path = join(&parent, &e.name);
            if e.is_dir && !recursive {
                return Err(format!(
                    "{path} is a folder. Pass -r to delete it and everything in it."
                )
                .into());
            }
            if !targets.iter().any(|(p, _)| *p == path) {
                targets.push((path, e.is_dir));
            }
        }
    }

    for (p, is_dir) in &targets {
        println!(
            "{p}{}",
            if *is_dir {
                "/ (and everything in it)"
            } else {
                ""
            }
        );
    }
    if !yes && !confirm(&format!("Delete {} item(s)?", targets.len()))? {
        println!("Nothing deleted.");
        return Ok(());
    }
    // CrossPoint only removes empty folders, so contents go first.
    let mut ordered = Vec::new();
    for (p, is_dir) in targets {
        if is_dir {
            walk(dev, &p, &mut ordered)?
        } else {
            ordered.push(p)
        }
    }
    dev.delete(&ordered)?;
    println!("deleted {} item(s)", ordered.len());
    Ok(())
}

fn walk(dev: &dyn Device, dir: &str, out: &mut Vec<String>) -> Result<()> {
    for e in dev.list(dir)? {
        let p = join(dir, &e.name);
        if e.is_dir {
            walk(dev, &p, out)?
        } else {
            out.push(p)
        }
    }
    out.push(dir.to_string());
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// `*` and `?` wildcards, iterative with single-star backtracking.
fn glob(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn pull(dev: &dyn Device, path: &str, output: Option<PathBuf>) -> Result<()> {
    if !dev.caps().download {
        return Err(device::unsupported("pull"));
    }
    let (_, name) = split(path);
    let out = match output {
        Some(o) if o.is_dir() => o.join(&name),
        Some(o) => o,
        None => PathBuf::from(&name),
    };
    let mut file = std::fs::File::create_new(&out).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!(
            "{} already exists. Pick another path with -o.",
            out.display()
        ),
        _ => format!("{}: {e}", out.display()),
    })?;
    let bar = ProgressBar::new(0)
        .with_style(bar_style()?)
        .with_message(name);
    let res = dev.download(path, &mut file, &bar);
    bar.finish_and_clear();
    match res {
        Ok(n) => {
            println!("saved  {} ({})", out.display(), HumanBytes(n));
            Ok(())
        }
        Err(e) => {
            drop(file);
            let _ = std::fs::remove_file(&out);
            Err(e)
        }
    }
}

/// `dest` is a folder when it ends in '/' or already exists as one; otherwise it is the new path.
/// A bare name renames in place.
fn mv(dev: &dyn Device, base: &str, name: &str, dest: &str) -> Result<()> {
    let caps = dev.caps();
    if !caps.rename || !caps.move_file {
        return Err(device::unsupported("mv"));
    }
    let src = resolve(base, name);
    let (src_dir, src_name) = split(&src);
    let target = if dest.contains('/') {
        resolve(base, dest)
    } else {
        join(&src_dir, dest)
    };
    let into_dir = dest.ends_with('/') || is_dir(dev, &target)?;
    let (dir, new_name) = if into_dir {
        (target, src_name.clone())
    } else {
        split(&target)
    };
    if dir != src_dir {
        mkdir_p(dev, &dir)?;
        dev.move_to(&src, &dir)?;
    }
    if new_name != src_name {
        dev.rename(&join(&dir, &src_name), &new_name)?;
    }
    println!("{src} -> {}", join(&dir, &new_name));
    Ok(())
}

fn is_dir(dev: &dyn Device, path: &str) -> Result<bool> {
    let (parent, name) = split(path);
    Ok(!name.is_empty()
        && dev
            .list(&parent)?
            .iter()
            .any(|e| e.is_dir && e.name.eq_ignore_ascii_case(&name)))
}

fn find(save: bool, trace: bool) -> Result<()> {
    let found = discover::find(true, trace);
    let first = found.first().ok_or(NOT_FOUND)?;
    for f in &found {
        println!("{:<16} {:<11} via {}", f.host, f.firmware.as_str(), f.via);
    }
    if save {
        let p = config::save("host", &first.host)?;
        println!("saved host = \"{}\" in {}", first.host, p.display());
    }
    Ok(())
}
