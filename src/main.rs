use std::{collections::HashSet, path::PathBuf};
use std::thread;
use glob::Pattern;
use clap::Parser;
use colored::*;
use num_cpus;
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

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
}

/// Represents a work unit for directory scanning
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

fn spawn_scanner_thread(
    work_rx: Receiver<WorkUnit>,
    dir_tx: Sender<WorkUnit>,
    result_tx: Sender<PathBuf>,
    pattern: Arc<PatternMatcher>,
    active_scanners: Arc<AtomicUsize>,
    max_depth: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut dir_entries = Vec::with_capacity(100);
        
        while let Ok(work) = work_rx.recv() {
            active_scanners.fetch_add(1, Ordering::SeqCst);
            
            if work.depth > max_depth {
                active_scanners.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
    
            dir_entries.clear();
            
            let read_dir = match std::fs::read_dir(&work.path) {
                Ok(dir) => dir,
                Err(_) => {
                    active_scanners.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
            };
    
            dir_entries.extend(read_dir.filter_map(Result::ok));
    
            for entry in dir_entries.iter() {
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
    
                if file_type.is_dir() {
                    if dir_tx.send(WorkUnit {
                        path: path.to_path_buf(),
                        depth: work.depth + 1,
                    }).is_err() {
                        break;
                    }
                } else if file_type.is_file() {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if pattern.matches(file_name) {
                            if result_tx.send(path).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            
            active_scanners.fetch_sub(1, Ordering::SeqCst);
        }
    })
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
    let args = Args::parse();
    let pattern = Arc::new(create_pattern_matcher(&args.pattern));
    let thread_count = args.threads.unwrap_or_else(num_cpus::get);
    
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