use clap::Parser;
use colored::*;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use glob::Pattern;
use log::debug;
use memchr::memmem::FinderBuilder; // Uses Boyer-Moore-Horspool algorithm for substring search
use parking_lot::Mutex;
use pathdiff::diff_paths;
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, SystemTime};
use std::{collections::HashSet, path::PathBuf};
mod filters;

#[derive(Default, Debug, Clone, Copy)]
enum SymlinkMode {
    #[default]
    Never, // -P: Never follow symlinks
    Command, // -H: Follow symlinks on command line only
    Always,  // -L: Follow all symlinks
}

enum PatternMatcher {
    Glob(Pattern),
    Substring { pattern_bytes: Box<[u8]> },
}

impl PatternMatcher {
    fn matches(&self, filename: &str) -> bool {
        match self {
            PatternMatcher::Glob(pattern) => pattern.matches(filename),
            PatternMatcher::Substring { pattern_bytes, .. } => {
                let filename_lower = filename.to_lowercase();
                FinderBuilder::new()
                    .build_forward(pattern_bytes)
                    .find(filename_lower.as_bytes())
                    .is_some()
            }
        }
    }
}

fn create_pattern_matcher(pattern: &str) -> PatternMatcher {
    if pattern.contains('*') || pattern.contains('?') {
        PatternMatcher::Glob(Pattern::new(pattern).expect("Invalid glob pattern"))
    } else {
        let pattern_lower = pattern.to_lowercase();
        let pattern_bytes = pattern_lower.as_bytes().to_vec().into_boxed_slice();

        PatternMatcher::Substring { pattern_bytes }
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
    type_filter: filters::TypeFilter,

    /// Print each matching path followed by a null character ('\0')
    /// instead of a newline, similar to "find -print0".
    #[arg(long = "print0")]
    print0: bool,

    /// Filter by modification time (format: [+-]N[smhd])
    /// Examples: +1d (more than 1 day), -2m (less than 2 minutes), 3d (exactly 3 days), +1h (more than 1 hour), -45s (less than 45 seconds)
    #[arg(long = "mtime", allow_hyphen_values = true)]
    mtime: Option<String>,

    /// Filter by access time (format: [+-]N[smhd])
    #[arg(long = "atime", allow_hyphen_values = true)]
    atime: Option<String>,

    /// Filter by change time (format: [+-]N[smhd])
    #[arg(long = "ctime", allow_hyphen_values = true)]
    ctime: Option<String>,

    /// Filter by file size (format: [+-]N[ckMG])
    /// Examples: +1M (more than 1MiB), -500k (less than 500KiB), 1G (approximately 1GiB)
    #[arg(long = "size", allow_hyphen_values = true)]
    size: Option<String>,
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
    type_filter: filters::TypeFilter,
    mtime_filter: Option<filters::TimeFilter>,
    atime_filter: Option<filters::TimeFilter>,
    ctime_filter: Option<filters::TimeFilter>,
    now: SystemTime,
    size_filter: Option<filters::SizeFilter>,
    system_checker: Arc<SystemPathChecker>,
}

fn normalize_path(path: &Path, root: &Path) -> PathBuf {
    if let Some(relative) = diff_paths(path, root) {
        // Always use the root path and join with relative to preserve symlink paths
        root.to_path_buf().join(relative)
    } else {
        // If diff_paths fails, return the original path
        path.to_path_buf()
    }
}
/// Represents a work unit for directory scanning
#[derive(Debug, Clone)]
struct WorkUnit {
    path: PathBuf,
    depth: usize,
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
fn is_type_match(
    metadata: &std::fs::Metadata,
    filter: filters::TypeFilter,
    ctx: &ScannerContext,
) -> bool {
    let file_type = metadata.file_type();
    let base_match = match filter {
        filters::TypeFilter::Any => true,
        filters::TypeFilter::File => file_type.is_file(),
        filters::TypeFilter::Dir => file_type.is_dir(),
        filters::TypeFilter::Symlink => file_type.is_symlink(),
    };

    if !base_match {
        return false;
    }

    // Apply size filter if present
    if let Some(size_filter) = &ctx.size_filter {
        if !size_filter.matches(metadata.len()) {
            return false;
        }
    }

    // Apply time filters
    if let Some(mtime_filter) = &ctx.mtime_filter {
        if !mtime_filter.matches(metadata.modified().unwrap_or(ctx.now), ctx.now) {
            return false;
        }
    }

    if let Some(atime_filter) = &ctx.atime_filter {
        if !atime_filter.matches(metadata.accessed().unwrap_or(ctx.now), ctx.now) {
            return false;
        }
    }

    if let Some(ctime_filter) = &ctx.ctime_filter {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ctime = SystemTime::UNIX_EPOCH + Duration::from_secs(metadata.ctime() as u64);
            if !ctime_filter.matches(ctime, ctx.now) {
                return false;
            }
        }
        #[cfg(not(unix))]
        {
            // Fall back to mtime on non-Unix systems
            if !ctime_filter.matches(metadata.modified().unwrap_or(ctx.now), ctx.now) {
                return false;
            }
        }
    }

    true
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
    type_filter: filters::TypeFilter,
    mtime_filter: Option<filters::TimeFilter>,
    atime_filter: Option<filters::TimeFilter>,
    ctime_filter: Option<filters::TimeFilter>,
    now: SystemTime,
    size_filter: Option<filters::SizeFilter>,
    system_checker: Arc<SystemPathChecker>,
}

fn spawn_scanner_thread(config: ScannerConfig) -> thread::JoinHandle<()> {
    let visited_paths = Arc::new(Mutex::new(HashSet::with_capacity(1000)));

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
                mtime_filter: config.mtime_filter.clone(),
                atime_filter: config.atime_filter.clone(),
                ctime_filter: config.ctime_filter.clone(),
                now: config.now,
                size_filter: config.size_filter.clone(),
                system_checker: Arc::clone(&config.system_checker),
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

struct ThreadPoolOptions {
    thread_count: usize,
    pattern: Arc<PatternMatcher>,
    channels: ChannelSet,
    max_depth: usize,
    symlink_mode: SymlinkMode,
    root_path: PathBuf,
    type_filter: filters::TypeFilter,
    mtime_filter: Option<filters::TimeFilter>,
    atime_filter: Option<filters::TimeFilter>,
    ctime_filter: Option<filters::TimeFilter>,
    now: SystemTime,
    size_filter: Option<filters::SizeFilter>,
}

#[derive(Default)]
struct SystemPathChecker {
    system_paths: Vec<PathBuf>,
}

impl SystemPathChecker {
    fn new() -> Self {
        #[cfg(test)]
        return SystemPathChecker::default();

        let mut checker = SystemPathChecker::default();

        #[cfg(target_os = "macos")]
        {
            checker.system_paths.extend_from_slice(&[
                PathBuf::from("/System"),
                PathBuf::from("/Library"),
                PathBuf::from("/private"),
                PathBuf::from("/Volumes"),
            ]);
        }

        #[cfg(target_os = "linux")]
        {
            checker.system_paths.extend_from_slice(&[
                PathBuf::from("/proc"),
                PathBuf::from("/sys"),
                PathBuf::from("/dev"),
                PathBuf::from("/run"),
                PathBuf::from("/private"),
            ]);
        }

        #[cfg(target_os = "windows")]
        {
            checker.system_paths.extend_from_slice(&[
                PathBuf::from("C:\\Windows"),
                PathBuf::from("C:\\Program Files\\Windows"),
                PathBuf::from("C:\\ProgramData\\Microsoft"),
                PathBuf::from("C:\\System Volume Information"),
            ]);
        }

        checker
    }

    #[inline]
    fn is_system_path(&self, path: &Path) -> bool {
        // Case-insensitive check for Windows paths
        #[cfg(target_os = "windows")]
        {
            let path_str = path.to_string_lossy().to_lowercase();
            self.system_paths.iter().any(|sys_path| {
                path_str.starts_with(&sys_path.to_string_lossy().to_lowercase())
                    || path_str.contains("\\system32")
                    || path_str.contains("\\syswow64")
            })
        }

        // Case-sensitive check for Unix-like systems
        #[cfg(not(target_os = "windows"))]
        {
            self.system_paths
                .iter()
                .any(|sys_path| path.starts_with(sys_path))
        }
    }
}

// Update handle_entry function to use SystemPathChecker
fn handle_entry(
    entry: std::fs::DirEntry,
    ctx: &ScannerContext,
    channels: &ScannerChannels,
) -> Result<(), Box<dyn Error>> {
    let path = entry.path();

    // Skip system paths early
    if ctx.system_checker.is_system_path(&path) {
        debug!("Skipping system path: {:?}", path);
        return Ok(());
    }

    let metadata = entry.metadata()?;
    let relative_path = normalize_path(&path, &ctx.root_path);

    // Rest of the original handle_entry logic remains the same...
    if metadata.file_type().is_symlink() {
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if ctx.pattern.matches(file_name) && is_type_match(&metadata, ctx.type_filter, ctx) {
                channels.result_tx.send(relative_path.clone())?;
            }
        }

        match handle_symlink(&path, metadata.file_type(), ctx, channels) {
            Ok(_) => (),
            Err(e) => debug!("Error handling symlink {:?}: {}", path, e),
        }
        return Ok(());
    }

    if metadata.file_type().is_dir() {
        handle_directory(path.clone(), ctx.work.depth, ctx, channels)?;

        if is_type_match(&metadata, ctx.type_filter, ctx) {
            if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
                if ctx.pattern.matches(dir_name) {
                    channels.result_tx.send(relative_path)?;
                }
            }
        }
    } else if metadata.file_type().is_file() {
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if ctx.pattern.matches(file_name) && is_type_match(&metadata, ctx.type_filter, ctx) {
                channels.result_tx.send(relative_path)?;
            }
        }
    }

    Ok(())
}

// Update setup_thread_pool to include SystemPathChecker
fn setup_thread_pool(pool_options: ThreadPoolOptions) -> ThreadPool {
    let active_scanners = Arc::new(AtomicUsize::new(0));
    let system_checker = Arc::new(SystemPathChecker::new());
    let mut scanner_handles = Vec::with_capacity(pool_options.thread_count);

    for _ in 0..pool_options.thread_count {
        let scanner_config = ScannerConfig {
            work_rx: pool_options.channels.work_rx.clone(),
            dir_tx: pool_options.channels.dir_tx.clone(),
            result_tx: pool_options.channels.result_tx.clone(),
            pattern: Arc::clone(&pool_options.pattern),
            active_scanners: Arc::clone(&active_scanners),
            max_depth: pool_options.max_depth,
            symlink_mode: pool_options.symlink_mode,
            root_path: pool_options.root_path.clone(),
            type_filter: pool_options.type_filter,
            mtime_filter: pool_options.mtime_filter.clone(),
            atime_filter: pool_options.atime_filter.clone(),
            ctime_filter: pool_options.ctime_filter.clone(),
            now: pool_options.now,
            size_filter: pool_options.size_filter.clone(),
            system_checker: Arc::clone(&system_checker),
        };
        scanner_handles.push(spawn_scanner_thread(scanner_config));
    }

    // Rest of the setup_thread_pool implementation remains the same...
    ThreadPool {
        scanner_handles,
        distributor_handle: spawn_work_distributor(
            pool_options.channels.work_tx,
            pool_options.channels.dir_rx,
            active_scanners,
        ),
        result_receiver: pool_options.channels.result_rx,
    }
}

fn main() {
    let args = Args::parse();

    // Parse time filters
    let mtime_filter = args
        .mtime
        .as_deref()
        .map(filters::TimeFilter::parse)
        .transpose()
        .unwrap_or_else(|e| {
            eprintln!("Invalid mtime filter: {}", e);
            std::process::exit(1);
        });

    let atime_filter = args
        .atime
        .as_deref()
        .map(filters::TimeFilter::parse)
        .transpose()
        .unwrap_or_else(|e| {
            eprintln!("Invalid atime filter: {}", e);
            std::process::exit(1);
        });

    let ctime_filter = args
        .ctime
        .as_deref()
        .map(filters::TimeFilter::parse)
        .transpose()
        .unwrap_or_else(|e| {
            eprintln!("Invalid ctime filter: {}", e);
            std::process::exit(1);
        });
    let size_filter = args
        .size
        .as_deref()
        .map(filters::SizeFilter::parse)
        .transpose()
        .unwrap_or_else(|e| {
            eprintln!("Invalid size filter: {}", e);
            std::process::exit(1);
        });
    let pattern = Arc::new(create_pattern_matcher(&args.pattern));
    let thread_count = args.threads.unwrap_or_else(num_cpus::get);
    let symlink_mode = args.symlink_mode();

    let channels = create_channels(thread_count);

    // Keep original path for normalization
    let root_path = args.dir.clone();

    // Use canonicalized path for actual filesystem operations
    let work_path = std::fs::canonicalize(&args.dir).unwrap_or_else(|_| args.dir.clone());

    // Submit initial work unit with the canonicalized path
    channels
        .work_tx
        .send(WorkUnit {
            path: work_path,
            depth: 0,
        })
        .expect("Failed to send initial work");

    let thread_pool = setup_thread_pool(ThreadPoolOptions {
        thread_count,
        pattern,
        channels,
        max_depth: args.max_depth,
        symlink_mode,
        root_path,
        type_filter: args.type_filter,
        mtime_filter,
        atime_filter,
        ctime_filter,
        now: SystemTime::now(),
        size_filter,
    });

    // Process results
    while let Ok(path) = thread_pool.result_receiver.recv() {
        if args.print0 {
            print!("{}\0", path.display());
            std::io::stdout().flush().expect("Failed to flush stdout");
        } else {
            println!("{}", format!("{}", path.display()).green());
        }
    }

    // Wait for all threads to complete
    for handle in thread_pool.scanner_handles {
        handle.join().unwrap();
    }
    thread_pool.distributor_handle.join().unwrap();
}
