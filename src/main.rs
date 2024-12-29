use std::error::Error;
use std::path::Path;
use std::sync::Mutex;
use log::debug;
use std::{collections::HashSet, path::PathBuf};
use std::thread;
use glob::Pattern;
use clap:: Parser;
use colored::*;
use num_cpus;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

#[derive(Debug, Clone, Copy)]
enum SymlinkMode {
    Never,      // -P: Never follow symlinks
    Command,    // -H: Follow symlinks on command line only
    Always,     // -L: Follow all symlinks
}

impl Default for SymlinkMode {
    fn default() -> Self {
        SymlinkMode::Never  // Default to -P behavior
    }
}

/// Pattern matcher that supports both glob and fuzzy matching
enum PatternMatcher {
    Glob(Pattern),
    Substring {
        pattern_lower: String,
    }
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
            pattern_lower: pattern.to_lowercase()
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
    #[arg(short, long)]
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
    max_depth: usize,
    is_command_line: bool,  // True for initial directory
    visited_paths: Arc<Mutex<HashSet<PathBuf>>>,  // For loop detection
}

struct ScannerChannels {
    dir_tx: Sender<WorkUnit>,
    result_tx: Sender<PathBuf>,
}

fn handle_directory(
    path: PathBuf,
    depth: usize,
    ctx: &ScannerContext,
    channels: &ScannerChannels,
) -> Result<(), Box<dyn Error>> {
    channels.dir_tx.send(WorkUnit {
        path,
        depth: depth + 1,
    })?;
    Ok(())
}

fn should_follow_symlink(
    ctx: &ScannerContext,
    is_command_path: bool,
) -> bool {
    match ctx.symlink_mode {
        SymlinkMode::Never => false,
        SymlinkMode::Command => is_command_path,
        SymlinkMode::Always => true,
    }
}

fn handle_entry(
    entry: std::fs::DirEntry,
    ctx: &ScannerContext,
    channels: &ScannerChannels,
) -> Result<(), Box<dyn Error>> {
    let path = entry.path();
    
    // Get file_type with fallback handling
    let file_type = match entry.file_type() {
        Ok(ft) => ft,
        Err(e) => {
            // Log the error if you want to track these cases
            debug!("Unable to get file type for {:?}: {}", path, e);
            return Ok(());
        }
    };

    // First check if it's a symlink that matches our pattern
    if file_type.is_symlink() {
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if ctx.pattern.matches(file_name) {
                // Try to send the symlink path, but don't fail if we can't
                if let Err(e) = channels.result_tx.send(path.clone()) {
                    debug!("Failed to send result for symlink {:?}: {}", path, e);
                    return Ok(());
                }
            }
        }
        
        // Handle symlink traversal with better error handling
        match handle_symlink(&path, file_type, ctx, channels) {
            Ok(_) => (),
            Err(e) => {
                debug!("Error handling symlink {:?}: {}", path, e);
            }
        }
        return Ok(());
    }

    // Handle regular files and directories
    if file_type.is_dir() {
        if let Err(e) = handle_directory(path.clone(), ctx.work.depth, ctx, channels) {
            debug!("Error handling directory {:?}: {}", path, e);
        }
    } else if file_type.is_file() {
        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
            if ctx.pattern.matches(file_name) {
                let cloned_path = path.clone();
                if let Err(e) = channels.result_tx.send(path) {
                    debug!("Failed to send result for file {:?}: {}", cloned_path, e);
                }
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

    // Read symlink with better error handling
    let real_path = match std::fs::read_link(path) {
        Ok(p) => p,
        Err(e) => {
            debug!("Failed to read symlink {:?}: {}", path, e);
            return Ok(false);
        }
    };

    let absolute_path = if real_path.is_relative() {
        path.parent()
            .unwrap_or_else(|| Path::new(""))
            .join(real_path)
    } else {
        real_path
    };

    // Check for symlink loops
    {
        let mut visited = match ctx.visited_paths.lock() {
            Ok(guard) => guard,
            Err(e) => {
                debug!("Failed to acquire lock for visited paths: {}", e);
                return Ok(false);
            }
        };
        
        if !visited.insert(absolute_path.clone()) {
            return Ok(false);
        }
    }

    // Get metadata with better error handling
    match std::fs::metadata(&absolute_path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                let cloned_path = absolute_path.clone();
                if let Err(e) = handle_directory(absolute_path, ctx.work.depth, ctx, channels) {
                    debug!("Error handling symlinked directory {:?}: {}", cloned_path, e);
                }
                Ok(false)
            } else {
                Ok(metadata.is_file())
            }
        }
        Err(e) => {
            debug!("Failed to get metadata for {:?}: {}", absolute_path, e);
            Ok(false)
        }
    }
}

fn spawn_scanner_thread(
    work_rx: Receiver<WorkUnit>,
    dir_tx: Sender<WorkUnit>,
    result_tx: Sender<PathBuf>,
    pattern: Arc<PatternMatcher>,
    active_scanners: Arc<AtomicUsize>,
    max_depth: usize,
    symlink_mode: SymlinkMode,
) -> thread::JoinHandle<()> {
    let visited_paths = Arc::new(Mutex::new(HashSet::new()));

    thread::spawn(move || {
        let channels = ScannerChannels { dir_tx, result_tx };
        
        while let Ok(work) = work_rx.recv() {
            active_scanners.fetch_add(1, Ordering::SeqCst);
            
            if work.depth > max_depth {
                active_scanners.fetch_sub(1, Ordering::SeqCst);
                continue;
            }

            let ctx = ScannerContext {
                work: work.clone(),
                pattern: Arc::clone(&pattern),
                symlink_mode,
                max_depth,
                is_command_line: work.depth == 0,
                visited_paths: Arc::clone(&visited_paths),
            };

            // More defensive read_dir handling
            let read_dir = match std::fs::read_dir(&work.path) {
                Ok(dir) => dir,
                Err(e) => {
                    debug!("Failed to read directory {:?}: {}", work.path, e);
                    active_scanners.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
            };

            for entry in read_dir.filter_map(|e| e.ok()) {
                if let Err(e) = handle_entry(entry, &ctx, &channels) {
                    debug!("Error processing entry: {}", e);
                }
            }
            
            active_scanners.fetch_sub(1, Ordering::SeqCst);
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
                    if empty_reads >= MAX_EMPTY_READS && 
                       active_scanners.load(Ordering::SeqCst) == 0 && 
                       dir_rx.is_empty() {
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
) -> ThreadPool {
    let active_scanners = Arc::new(AtomicUsize::new(0));
    let mut scanner_handles = Vec::with_capacity(thread_count);
    
    // Spawn scanner threads
    for _ in 0..thread_count {
        let handle = spawn_scanner_thread(
            channels.work_rx.clone(),
            channels.dir_tx.clone(),
            channels.result_tx.clone(),
            Arc::clone(&pattern),
            Arc::clone(&active_scanners),
            max_depth,
            symlink_mode,
        );
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
    env_logger::init();
    let args = Args::parse();
    let args = Args::parse();
    let pattern = Arc::new(create_pattern_matcher(&args.pattern));
    let thread_count = args.threads.unwrap_or_else(num_cpus::get);
    let symlink_mode = args.symlink_mode();
    
    let channels = create_channels(thread_count);
    
    // Submit initial work unit
    channels.work_tx.send(WorkUnit {
        path: args.dir.clone(),
        depth: 0,
    }).expect("Failed to send initial work");

    let thread_pool = setup_thread_pool(
        thread_count,
        pattern,
        channels,
        args.max_depth,
        symlink_mode,
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