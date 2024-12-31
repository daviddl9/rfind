use clap::Parser;
use colored::*;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use glob::Pattern;
use log::debug;
use parking_lot::Mutex;
use pathdiff::diff_paths;
use std::error::Error;
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::{collections::HashSet, path::PathBuf};

#[derive(Default, Debug, Clone, Copy)]
enum SymlinkMode {
    #[default]
    Never, // -P: Never follow symlinks
    Command, // -H: Follow symlinks on command line only
    Always,  // -L: Follow all symlinks
}

/// Enum to filter results by type.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum TypeFilter {
    #[default]
    Any,
    File,
    Dir,
    Symlink,
}

impl std::str::FromStr for TypeFilter {
    type Err = String;

    /// Converts user input to a `TypeFilter`.
    /// Example: "-t f" => `TypeFilter::File`, "-t d" => `TypeFilter::Dir`, "-t l" => `TypeFilter::Symlink`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "f" | "file" => Ok(TypeFilter::File),
            "d" | "dir" => Ok(TypeFilter::Dir),
            "l" | "link" | "symlink" => Ok(TypeFilter::Symlink),
            "any" => Ok(TypeFilter::Any),
            other => Err(format!("Invalid type filter '{}'. Use f|d|l|any.", other)),
        }
    }
}

/// Pattern matcher that supports both glob and fuzzy matching
enum PatternMatcher {
    Glob(Pattern),
    Substring { pattern_lower: String },
}

impl PatternMatcher {
    fn matches(&self, filename: &str) -> bool {
        match self {
            PatternMatcher::Glob(pattern) => pattern.matches(filename),
            PatternMatcher::Substring { pattern_lower } => {
                filename.to_lowercase().contains(pattern_lower)
            }
        }
    }
}

fn create_pattern_matcher(pattern: &str) -> PatternMatcher {
    if pattern.contains('*') || pattern.contains('?') {
        PatternMatcher::Glob(Pattern::new(pattern).expect("Invalid glob pattern"))
    } else {
        PatternMatcher::Substring {
            pattern_lower: pattern.to_lowercase(),
        }
    }
}
/// Parallel recursive file finder
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Pattern to search for (glob patterns like *.log or substring search)
    #[arg(required = true)]
    pattern: String,

    /// Starting directory (defaults to root directory)
    #[arg(short, long, default_value = "/")]
    dir: PathBuf,

    /// Maximum search depth
    #[arg(short, long, default_value = "100")]
    max_depth: usize,

    /// Number of worker threads (defaults to number of CPU cores)
    #[arg(short = 'j', long)]
    threads: Option<usize>,

    /// Never follow symbolic links (default)
    #[arg(short = 'P', long, group = "symlink_mode")]
    no_follow: bool,

    /// Follow symbolic links on command line only
    #[arg(short = 'H', long, group = "symlink_mode")]
    cmd_follow: bool,

    /// Follow all symbolic links
    #[arg(short = 'L', long, group = "symlink_mode")]
    follow_all: bool,

    /// Filter the results by type.
    /// Possible values: f|file, d|dir, l|symlink, or any.
    #[arg(short = 't', long = "type", default_value = "any")]
    type_filter: TypeFilter,
}

impl Args {
    fn symlink_mode(&self) -> SymlinkMode {
        if self.follow_all {
            SymlinkMode::Always
        } else if self.cmd_follow {
            SymlinkMode::Command
        } else {
            SymlinkMode::Never
        }
    }
}

struct ScannerContext {
    work: WorkUnit,
    pattern: Arc<PatternMatcher>,
    symlink_mode: SymlinkMode,
    is_command_line: bool,                       // True for initial directory
    visited_paths: Arc<Mutex<HashSet<PathBuf>>>, // For loop detection
    root_path: PathBuf,
    type_filter: TypeFilter,
}

fn normalize_path(path: &Path, root: &Path) -> PathBuf {
    let root_parent = root.parent().unwrap_or_else(|| Path::new(""));
    // This will maintain the root directory name in the path
    diff_paths(path, root_parent).unwrap_or_else(|| path.to_path_buf())
}

struct ScannerChannels {
    dir_tx: Sender<WorkUnit>,
    result_tx: Sender<PathBuf>,
}

fn handle_directory(
    path: PathBuf,
    depth: usize,
    _ctx: &ScannerContext,
    channels: &ScannerChannels,
) -> Result<(), Box<dyn Error>> {
    channels.dir_tx.send(WorkUnit {
        path,
        depth: depth + 1,
    })?;
    Ok(())
}

fn should_follow_symlink(ctx: &ScannerContext, is_command_path: bool) -> bool {
    match ctx.symlink_mode {
        SymlinkMode::Never => false,
        SymlinkMode::Command => is_command_path,
        SymlinkMode::Always => true,
    }
}

/// Checks if the file/directory/symlink should be recorded as a match
/// based on the --type / -t filter provided by the user.
fn is_type_match(file_type: &std::fs::FileType, filter: TypeFilter) -> bool {
    match filter {
        TypeFilter::Any => true,
        TypeFilter::File => file_type.is_file(),
        TypeFilter::Dir => file_type.is_dir(),
        TypeFilter::Symlink => file_type.is_symlink(),
    }
}

fn handle_entry(
    entry: std::fs::DirEntry,
    ctx: &ScannerContext,
    channels: &ScannerChannels,
) -> Result<(), Box<dyn Error>> {
    let path = entry.path();
    let file_type = entry.file_type()?;

    // Normalize the path relative to root
    let relative_path = normalize_path(&path, &ctx.root_path);

    // Symlink handling
    if file_type.is_symlink() {
        // If the filename matches our pattern, check if it also matches our --type filter
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if ctx.pattern.matches(file_name) && is_type_match(&file_type, ctx.type_filter) {
                channels.result_tx.send(relative_path.clone())?;
            }
        }

        match handle_symlink(&path, file_type, ctx, channels) {
            Ok(_) => (),
            Err(e) => debug!("Error handling symlink {:?}: {}", path, e),
        }
        return Ok(());
    }

    // Directory
    if file_type.is_dir() {
        // Always recurse into directories, even if --type=f
        handle_directory(path.clone(), ctx.work.depth, ctx, channels)?;

        // If user wants directories and the name matches our pattern, record it
        if is_type_match(&file_type, ctx.type_filter) {
            if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                if ctx.pattern.matches(dir_name) {
                    channels.result_tx.send(relative_path)?;
                }
            }
        }
    }
    // File
    else if file_type.is_file() {
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            // If filename matches pattern and type is file
            if ctx.pattern.matches(file_name) && is_type_match(&file_type, ctx.type_filter) {
                channels.result_tx.send(relative_path)?;
            }
        }
    }

    Ok(())
}

fn handle_symlink(
    path: &Path,
    _file_type: std::fs::FileType,
    ctx: &ScannerContext,
    channels: &ScannerChannels,
) -> Result<bool, Box<dyn Error>> {
    if !should_follow_symlink(ctx, ctx.is_command_line) {
        return Ok(false);
    }

    // Keep the original symlink path for directory traversal
    let symlink_path = path.to_path_buf();

    // Check for symlink loops using canonical paths
    let canonical = path.canonicalize().ok();
    if let Some(canonical_path) = canonical {
        let mut visited = ctx.visited_paths.lock();
        if !visited.insert(canonical_path) {
            return Ok(false);
        }
    }

    match std::fs::metadata(&symlink_path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                // Use the original symlink path for directory traversal
                handle_directory(symlink_path, ctx.work.depth, ctx, channels)?;
                Ok(false)
            } else {
                Ok(metadata.is_file())
            }
        }
        Err(_) => Ok(false),
    }
}

struct ScannerConfig {
    work_rx: Receiver<WorkUnit>,
    dir_tx: Sender<WorkUnit>,
    result_tx: Sender<PathBuf>,
    pattern: Arc<PatternMatcher>,
    active_scanners: Arc<AtomicUsize>,
    max_depth: usize,
    symlink_mode: SymlinkMode,
    root_path: PathBuf,
    type_filter: TypeFilter,
}

fn spawn_scanner_thread(config: ScannerConfig) -> thread::JoinHandle<()> {
    let visited_paths = Arc::new(Mutex::new(HashSet::new()));

    thread::spawn(move || {
        let channels = ScannerChannels {
            dir_tx: config.dir_tx,
            result_tx: config.result_tx,
        };

        while let Ok(work) = config.work_rx.recv() {
            config.active_scanners.fetch_add(1, Ordering::SeqCst);

            if work.depth > config.max_depth {
                config.active_scanners.fetch_sub(1, Ordering::SeqCst);
                continue;
            }

            let ctx = ScannerContext {
                work: work.clone(),
                pattern: Arc::clone(&config.pattern),
                symlink_mode: config.symlink_mode,
                is_command_line: work.depth == 0,
                visited_paths: Arc::clone(&visited_paths),
                root_path: config.root_path.clone(),
                type_filter: config.type_filter,
            };

            // More defensive read_dir handling
            let read_dir = match std::fs::read_dir(&work.path) {
                Ok(dir) => dir,
                Err(e) => {
                    debug!("Failed to read directory {:?}: {}", work.path, e);
                    config.active_scanners.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
            };

            for entry in read_dir.filter_map(|e| e.ok()) {
                if let Err(e) = handle_entry(entry, &ctx, &channels) {
                    debug!("Error processing entry: {}", e);
                }
            }

            config.active_scanners.fetch_sub(1, Ordering::SeqCst);
        }
    })
}

/// Represents a work unit for directory scanning
#[derive(Debug, Clone)]
struct WorkUnit {
    path: PathBuf,
    depth: usize,
}

struct ThreadPool {
    scanner_handles: Vec<thread::JoinHandle<()>>,
    distributor_handle: thread::JoinHandle<()>,
    result_receiver: Receiver<PathBuf>,
}

struct ChannelSet {
    work_tx: Sender<WorkUnit>,
    work_rx: Receiver<WorkUnit>,
    result_tx: Sender<PathBuf>,
    result_rx: Receiver<PathBuf>,
    dir_tx: Sender<WorkUnit>,
    dir_rx: Receiver<WorkUnit>,
}

fn create_channels(thread_count: usize) -> ChannelSet {
    let (work_tx, work_rx) = bounded(thread_count * 8);
    let (result_tx, result_rx) = unbounded();
    let (dir_tx, dir_rx) = unbounded();

    ChannelSet {
        work_tx,
        work_rx,
        result_tx,
        result_rx,
        dir_tx,
        dir_rx,
    }
}

fn spawn_work_distributor(
    work_tx: Sender<WorkUnit>,
    dir_rx: Receiver<WorkUnit>,
    active_scanners: Arc<AtomicUsize>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut pending_dirs = HashSet::new();
        pending_dirs.insert(String::from("initial"));

        let mut empty_reads = 0;
        const MAX_EMPTY_READS: u8 = 3;

        loop {
            match dir_rx.try_recv() {
                Ok(dir) => {
                    empty_reads = 0;
                    pending_dirs.insert(dir.path.to_string_lossy().to_string());
                    if work_tx.send(dir).is_err() {
                        break;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    empty_reads += 1;
                    if empty_reads >= MAX_EMPTY_READS
                        && active_scanners.load(Ordering::SeqCst) == 0
                        && dir_rx.is_empty()
                    {
                        break;
                    }
                    thread::sleep(std::time::Duration::from_micros(100));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
    })
}

fn setup_thread_pool(
    thread_count: usize,
    pattern: Arc<PatternMatcher>,
    channels: ChannelSet,
    max_depth: usize,
    symlink_mode: SymlinkMode,
    root_path: PathBuf,
    type_filter: TypeFilter,
) -> ThreadPool {
    let active_scanners = Arc::new(AtomicUsize::new(0));
    let mut scanner_handles = Vec::with_capacity(thread_count);

    // Spawn scanner threads
    for _ in 0..thread_count {
        let handle = spawn_scanner_thread(ScannerConfig {
            work_rx: channels.work_rx.clone(),
            dir_tx: channels.dir_tx.clone(),
            result_tx: channels.result_tx.clone(),
            pattern: Arc::clone(&pattern),
            active_scanners: Arc::clone(&active_scanners),
            max_depth,
            symlink_mode,
            root_path: root_path.clone(),
            type_filter,
        });
        scanner_handles.push(handle);
    }

    // Spawn work distributor
    let distributor_handle = spawn_work_distributor(
        channels.work_tx.clone(),
        channels.dir_rx,
        Arc::clone(&active_scanners),
    );

    ThreadPool {
        scanner_handles,
        distributor_handle,
        result_receiver: channels.result_rx,
    }
}

fn main() {
    let args = Args::parse();
    let pattern = Arc::new(create_pattern_matcher(&args.pattern));
    let thread_count = args.threads.unwrap_or_else(num_cpus::get);
    let symlink_mode = args.symlink_mode();

    let channels = create_channels(thread_count);

    // Get the canonical path of the root directory's parent
    let root_path = std::fs::canonicalize(&args.dir).unwrap_or_else(|_| args.dir.clone());

    // Submit initial work unit with the full path
    channels
        .work_tx
        .send(WorkUnit {
            path: root_path.clone(),
            depth: 0,
        })
        .expect("Failed to send initial work");

    let thread_pool = setup_thread_pool(
        thread_count,
        pattern,
        channels,
        args.max_depth,
        symlink_mode,
        root_path,
        args.type_filter,
    );

    // Process results
    while let Ok(path) = thread_pool.result_receiver.recv() {
        println!("{}", format!("{}", path.display()).green());
    }

    // Wait for all threads to complete
    for handle in thread_pool.scanner_handles {
        handle.join().unwrap();
    }
    thread_pool.distributor_handle.join().unwrap();
}
