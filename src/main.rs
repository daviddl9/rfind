use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use structopt::StructOpt;
use ctrlc;

use rfind::IndexManager;

// CLI Options
#[derive(StructOpt, Debug)]
#[structopt(name = "rfind", about = "Fast home directory search tool")]
struct Opt {
    /// Search pattern (omit if using --reindex)
    #[structopt(name = "PATTERN", required_unless = "reindex")]
    pattern: Option<String>,

    /// Verbose output
    #[structopt(short, long)]
    verbose: bool,

    /// Force a full index before searching
    #[structopt(short, long)]
    force_reindex: bool,

    /// Reindex system and exit (no search)
    #[structopt(long)]
    reindex: bool,
}

fn main() -> io::Result<()> {
    let opt = Opt::from_args();

    // Ctrl+C handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("\nReceived Ctrl+C, finishing current operation...");
    }).expect("Error setting Ctrl+C handler");

    // If --reindex is specified with no pattern, just reindex everything
    if opt.reindex && opt.pattern.is_none() {
        let mut manager = IndexManager::new(opt.verbose);
        manager.index_home_directory()?;
        println!("Reindexing completed!");
        return Ok(());
    }

    // Otherwise, we are doing a search
    let pattern = match &opt.pattern {
        Some(p) => p,
        None => {
            eprintln!("No pattern provided. Use --reindex to just reindex, or pass a search pattern.");
            std::process::exit(1);
        }
    };

    let mut manager = IndexManager::new(opt.verbose);

    // If forced reindex or no existing chunks, index first
    if opt.force_reindex || manager.index.chunks.is_empty() {
        if opt.verbose {
            println!("Building initial index (forced)...");
        }
        manager.index_home_directory()?;
    }

    if opt.verbose {
        println!("Searching for pattern: {}", pattern);
    }

    // Perform the search
    let results = manager.search(pattern)?;

    // Print results
    if results.is_empty() {
        if opt.verbose {
            println!("No matches found for: {}", pattern);
        }
        return Ok(());
    }

    if opt.verbose {
        println!("\nFound {} matches:", results.len());
    }

    for path in &results {
        println!("{}", path.display());
    }

    // If a background reindex is in progress, optionally wait
    if manager.is_reindexing() && opt.verbose {
        println!("\nBackground reindexing in progress. Press Ctrl+C to exit...");
        while running.load(Ordering::SeqCst) && manager.is_reindexing() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    Ok(())
}
