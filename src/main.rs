use std::path::PathBuf;
use std::thread;
use std::time::Instant;
use walkdir::DirEntry;
use glob::Pattern;
use clap::Parser;
use colored::*;
use rayon::prelude::*;
use num_cpus;
use crossbeam_channel::{bounded, unbounded};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

/// Parallel recursive file finder
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Pattern to search for (supports glob patterns like *.log)
    #[arg(required = true)]
    pattern: String,

    /// Starting directory (defaults to current directory)
    #[arg(short, long, default_value = "/")]
    dir: PathBuf,

    /// Maximum search depth
    #[arg(short, long, default_value = "100")]
    max_depth: usize,

    /// Number of worker threads (defaults to number of CPU cores)
    #[arg(short, long)]
    threads: Option<usize>,
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry.file_name()
        .to_str()
        .map(|s| s.starts_with("."))
        .unwrap_or(false)
}

/// Represents a work unit for directory scanning
struct WorkUnit {
    path: PathBuf,
    depth: usize,
}

fn main() {
    // Start timing
    let start_time = Instant::now();
    
    let args = Args::parse();
    let pattern = Arc::new(Pattern::new(&args.pattern).expect("Invalid pattern"));
    let max_depth = args.max_depth;
    
    // Determine number of worker threads
    let thread_count = args.threads.unwrap_or_else(num_cpus::get);
    
    // Counter for active directory scanners
    let active_scanners = Arc::new(AtomicUsize::new(0));
    
    // Channels for work distribution and result collection
    let (work_tx, work_rx) = bounded::<WorkUnit>(thread_count * 2);
    let (result_tx, result_rx) = unbounded();
    let (dir_tx, dir_rx) = unbounded();

    // Submit initial work unit
    work_tx.send(WorkUnit {
        path: args.dir,
        depth: 0,
    }).expect("Failed to send initial work");

    // Spawn directory scanner threads
    let mut scanner_handles = vec![];
    
    for _ in 0..thread_count {
        let work_rx = work_rx.clone();
        let dir_tx = dir_tx.clone();
        let result_tx = result_tx.clone();
        let pattern = Arc::clone(&pattern);
        let active_scanners = Arc::clone(&active_scanners);
        
        let handle = thread::spawn(move || {
            while let Ok(work) = work_rx.recv() {
                active_scanners.fetch_add(1, Ordering::SeqCst);
                
                if work.depth >= max_depth {
                    active_scanners.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }

                let read_dir = match std::fs::read_dir(&work.path) {
                    Ok(dir) => dir,
                    Err(_) => {
                        active_scanners.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                };

                for entry in read_dir.filter_map(Result::ok) {
                    let path = entry.path();
                    let file_type = match entry.file_type() {
                        Ok(ft) => ft,
                        Err(_) => continue,
                    };

                    if file_type.is_dir() {
                        // Send subdirectory for processing
                        if dir_tx.send(WorkUnit {
                            path: path.to_path_buf(),
                            depth: work.depth + 1,
                        }).is_err() {
                            break;
                        }
                    } else if file_type.is_file() {
                        // Check if file matches pattern
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
        });
        scanner_handles.push(handle);
    }

    // Spawn work distributor thread
    let work_tx_clone = work_tx.clone();
    let active_scanners = Arc::clone(&active_scanners);
    let distributor_handle = thread::spawn(move || {
        let mut pending_dirs = vec![true]; // Track each directory separately
        let mut current_index = 0;
        
        loop {
            // First check if we're done
            if pending_dirs.iter().all(|&x| !x) {
                break;
            }
            
            // Process any new directories
            match dir_rx.try_recv() {
                Ok(dir) => {
                    pending_dirs.push(true);
                    if work_tx_clone.send(dir).is_err() {
                        break;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // No new directories, check active scanners
                    if active_scanners.load(Ordering::SeqCst) == 0 {
                        // Mark current batch as complete
                        for i in current_index..pending_dirs.len() {
                            pending_dirs[i] = false;
                        }
                        current_index = pending_dirs.len();
                    }
                    // Small sleep to prevent busy waiting
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
        
        // Signal end of work
        drop(work_tx_clone);
    });

    // Drop original sender to allow proper shutdown
    drop(work_tx);
    drop(dir_tx);
    drop(result_tx);

    // Print results as they come in
    let mut count = 0;
    while let Ok(path) = result_rx.recv() {
        count += 1;
        println!("{}", format!("Found: {}", path.display()).green());
    }

    // Wait for all threads to complete
    for handle in scanner_handles {
        handle.join().unwrap();
    }
    distributor_handle.join().unwrap();

    // Calculate and print elapsed time
    let elapsed = start_time.elapsed();
    
    println!("\n{}", format!("Total matches found: {}", count).blue());
    println!("{}", format!("Used {} worker threads", thread_count).yellow());
    println!("{}", format!("Total time: {:.2?}", elapsed).cyan());
}