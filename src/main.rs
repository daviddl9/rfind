use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    env,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
    thread,
    sync::Arc,
};
use glob::Pattern;
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::WalkDir;
use structopt::StructOpt;
use bincode::{serialize_into, deserialize_from};
use serde::{Serialize, Deserialize};
use ctrlc;
use std::sync::atomic::{AtomicBool, Ordering};
use strsim::{jaro_winkler, normalized_levenshtein};

// --------------------
// Constants & Structs
// --------------------

// Fuzzy-matching threshold
const FUZZY_THRESHOLD: f64 = 0.8;  // Minimum similarity score to consider a match

// Fuzzy-matching search results
#[derive(Debug, Clone)]
struct SearchResult {
    path: PathBuf,
    score: f64,
}

impl SearchResult {
    fn new(path: PathBuf, score: f64) -> Self {
        Self { path, score }
    }
}

// Reusable chunk size for index splitting
const CHUNK_SIZE: usize = 1000;
// How often (in seconds) before we recompute directory hashes
const HASH_CACHE_DURATION: u64 = 3600; // 1 hour in seconds

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

// File and Directory Structures
#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileEntry {
    path: PathBuf,
    modified: u64,
    is_dir: bool,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct DirectoryHash {
    path: PathBuf,
    hash: u64,
    last_check: u64,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct DirectoryHashes {
    hashes: HashMap<PathBuf, DirectoryHash>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct IndexChunk {
    files: HashMap<PathBuf, FileEntry>,
    terms: HashMap<String, HashSet<PathBuf>>,
}

#[derive(Debug, Default)]
struct Index {
    chunks: Vec<IndexChunk>,
    current_chunk: IndexChunk,
    files_in_current_chunk: usize,
}

// --------------------
// DirectoryHashes
// --------------------
impl DirectoryHashes {
    fn load() -> Self {
        if let Ok(home) = env::var("HOME") {
            let hash_path = PathBuf::from(home).join(".rfind").join("dir_hashes.bin");
            if let Ok(file) = File::open(hash_path) {
                if let Ok(hashes) = deserialize_from(BufReader::new(file)) {
                    return hashes;
                }
            }
        }
        Self::default()
    }

    fn save(&self) -> io::Result<()> {
        let home = env::var("HOME").map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let hash_dir = PathBuf::from(home).join(".rfind");
        fs::create_dir_all(&hash_dir)?;
        let hash_path = hash_dir.join("dir_hashes.bin");
        let file = File::create(hash_path)?;
        serialize_into(BufWriter::new(file), self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }
}

// --------------------
// Index
// --------------------
impl Index {
    // Fuzzy match a single "haystack" vs. a single "needle"
    fn fuzzy_match(haystack: &str, needle: &str) -> Option<f64> {
        let haystack = haystack.to_lowercase();
        let needle = needle.to_lowercase();
        
        // Direct substring match gets highest score
        if haystack.contains(&needle) {
            return Some(1.0);
        }
        
        // Check individual components for fuzzy matches
        let haystack_parts: Vec<&str> = haystack
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();
            
        let mut max_score: f64 = 0.0;
        
        for part in haystack_parts {
            // If needle is short, Jaro-Winkler is often better for minor typos
            // If needle is longer, normalized Levenshtein can be better
            let score = if needle.len() <= 5 {
                jaro_winkler(part, &needle)
            } else {
                normalized_levenshtein(part, &needle)
            };
            max_score = max_score.max(score);
        }
        
        if max_score >= FUZZY_THRESHOLD {
            Some(max_score)
        } else {
            None
        }
    }

    // Search across a chunk using fuzzy logic + optional glob pattern
    fn search_chunk_fuzzy(&self, chunk: &IndexChunk, search_terms: &[String], glob_pattern: &Pattern) -> Vec<SearchResult> {
        let mut results = Vec::new();
        
        for (path, _) in &chunk.files {
            // If the user is using a custom glob (contains * or ?), filter
            if glob_pattern.as_str() != "**/*" && !glob_pattern.matches(&path.to_string_lossy()) {
                continue;
            }
            
            let path_str = path.to_string_lossy();
            let filename = path.file_name()
                .map(|f| f.to_string_lossy())
                .unwrap_or_default();
                
            let mut min_score: f64 = 1.0;
            let mut found_all_terms = true;
            
            // Each term must be found in path or filename
            for term in search_terms {
                if let Some(filename_score) = Self::fuzzy_match(&filename, term) {
                    min_score = min_score.min(filename_score);
                } else if let Some(path_score) = Self::fuzzy_match(&path_str, term) {
                    min_score = min_score.min(path_score);
                } else {
                    found_all_terms = false;
                    break;
                }
            }
            
            if found_all_terms {
                results.push(SearchResult::new(path.clone(), min_score));
            }
        }
        
        results
    }

    // Main search function
    fn search(&self, pattern: &str) -> Vec<PathBuf> {
        // Check if it looks like a glob
        let is_pure_glob = pattern.contains('*') || pattern.contains('?');
        
        // If we detect a glob, build a Pattern accordingly
        let glob_pattern = if pattern.starts_with("**") {
            Pattern::new(pattern).unwrap_or_else(|_| Pattern::new("**/*").unwrap())
        } else if is_pure_glob {
            Pattern::new(&format!("**/{}", pattern)).unwrap_or_else(|_| Pattern::new("**/*").unwrap())
        } else {
            // If no glob characters, use a universal glob to skip
            Pattern::new("**/*").unwrap()
        };

        // For non-glob searches, split pattern into search terms
        let search_terms = if is_pure_glob {
            Vec::new()  // If it's purely a glob, don't do fuzzy matching
        } else {
            pattern.split_whitespace().map(|s| s.to_string()).collect()
        };

        let mut all_results = Vec::new();

        // Search all saved chunks
        for chunk in &self.chunks {
            let chunk_results = self.search_chunk_fuzzy(chunk, &search_terms, &glob_pattern);
            all_results.extend(chunk_results);
        }

        // Search the current chunk
        let current_results = self.search_chunk_fuzzy(&self.current_chunk, &search_terms, &glob_pattern);
        all_results.extend(current_results);

        // Sort by best fuzzy score first, then remove duplicates
        all_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let mut seen = HashSet::new();
        all_results.into_iter()
            .filter(|r| seen.insert(r.path.clone()))
            .map(|r| r.path)
            .collect()
    }

    // Retrieve file entry if present
    fn get_file_entry(&self, path: &Path) -> Option<&FileEntry> {
        // Check current chunk first
        if let Some(entry) = self.current_chunk.files.get(path) {
            return Some(entry);
        }
        // Then all historical chunks
        for chunk in &self.chunks {
            if let Some(entry) = chunk.files.get(path) {
                return Some(entry);
            }
        }
        None
    }

    // Constructor
    fn new() -> Self {
        Self::load().unwrap_or_default()
    }

    // Load from disk
    fn load() -> Option<Self> {
        let home = env::var("HOME").ok()?;
        let index_dir = PathBuf::from(home).join(".rfind");
        fs::create_dir_all(&index_dir).ok()?;

        let mut chunks = Vec::new();
        let mut chunk_id = 0;

        // Keep reading chunk_0, chunk_1, etc.
        loop {
            let chunk_path = index_dir.join(format!("chunk_{}.idx", chunk_id));
            if !chunk_path.exists() {
                break;
            }
            if let Ok(file) = File::open(&chunk_path) {
                if let Ok(chunk) = deserialize_from(BufReader::new(file)) {
                    chunks.push(chunk);
                }
            }
            chunk_id += 1;
        }

        Some(Index {
            chunks,
            current_chunk: IndexChunk::default(),
            files_in_current_chunk: 0,
        })
    }

    // Save entire index to disk
    fn save(&self) -> io::Result<()> {
        let home = env::var("HOME").map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let index_dir = PathBuf::from(home).join(".rfind");
        fs::create_dir_all(&index_dir)?;

        // Save older chunks
        for (i, chunk) in self.chunks.iter().enumerate() {
            let chunk_path = index_dir.join(format!("chunk_{}.idx", i));
            let file = File::create(chunk_path)?;
            serialize_into(BufWriter::new(file), chunk)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        // Save current chunk
        if self.files_in_current_chunk > 0 {
            let chunk_path = index_dir.join(format!("chunk_{}.idx", self.chunks.len()));
            let file = File::create(chunk_path)?;
            serialize_into(BufWriter::new(file), &self.current_chunk)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        Ok(())
    }

    // Check if a file has already been indexed
    fn contains_file(&self, path: &Path) -> bool {
        if self.current_chunk.files.contains_key(path) {
            return true;
        }
        self.chunks.iter().any(|chunk| chunk.files.contains_key(path))
    }

    // Extract search terms from a path for indexing
    fn extract_terms(path: &Path) -> Vec<String> {
        let path_str = path.to_string_lossy().to_lowercase();
        let mut terms = Vec::new();
        
        // Add the full filename
        if let Some(filename) = path.file_name() {
            terms.push(filename.to_string_lossy().to_lowercase());
        }
        
        // Add path components
        for component in path.components() {
            if let std::path::Component::Normal(os_str) = component {
                if let Some(s) = os_str.to_str() {
                    terms.push(s.to_lowercase());
                }
            }
        }
        
        // Split on special characters
        let split_terms: Vec<String> = path_str
            .split(['.', '_', '-', '[', ']', '(', ')', '{', '}'])
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .flat_map(|s| {
                let mut parts = vec![s.to_string()];
                parts.extend(
                    s.split_whitespace()
                        .filter(|w| w.len() >= 2)
                        .map(|w| w.to_string())
                );
                parts
            })
            .collect();
        terms.extend(split_terms);
        
        // Also add a cleaned version of the full path
        let clean_path = path_str
            .replace(['[', ']', '(', ')', '{', '}'], "")
            .replace(|c: char| !c.is_alphanumeric() && !c.is_whitespace(), " ")
            .trim()
            .to_string();
        terms.push(clean_path);
        
        // Remove duplicates
        terms.retain(|s| !s.is_empty());
        terms.sort_unstable();
        terms.dedup();
        
        terms
    }

    // Add a new file to the index
    fn add_file(&mut self, entry: FileEntry) {
        let terms = Self::extract_terms(&entry.path);

        for term in terms {
            self.current_chunk
                .terms
                .entry(term)
                .or_default()
                .insert(entry.path.clone());
        }

        self.current_chunk.files.insert(entry.path.clone(), entry);
        self.files_in_current_chunk += 1;

        // If the chunk is large, rotate it
        if self.files_in_current_chunk >= CHUNK_SIZE {
            let full_chunk = std::mem::replace(&mut self.current_chunk, IndexChunk::default());
            self.chunks.push(full_chunk);
            self.files_in_current_chunk = 0;
        }
    }
}

// --------------------
// IndexManager
// --------------------
#[derive(Debug)]
struct IndexManager {
    index: Index,
    verbose: bool,
    dir_hashes: DirectoryHashes,
    reindexing: Arc<AtomicBool>,
}

impl IndexManager {
    fn new(verbose: bool) -> Self {
        Self {
            index: Index::new(),
            verbose,
            dir_hashes: DirectoryHashes::load(),
            reindexing: Arc::new(AtomicBool::new(false)),
        }
    }

    fn is_reindexing(&self) -> bool {
        self.reindexing.load(Ordering::SeqCst)
    }

    // Spawn a background reindex if needed
    fn background_reindex(&self, verbose: bool, dirs: Vec<PathBuf>) {
        let reindexing = self.reindexing.clone();
        reindexing.store(true, Ordering::SeqCst);

        thread::spawn(move || {
            let mut manager = IndexManager::new(verbose);
            if verbose {
                println!("Background: Re-indexing all directories");
            }
            for dir in dirs {
                if let Err(e) = manager.index_directory(&dir) {
                    eprintln!("Background: Error indexing {}: {}", dir.display(), e);
                }
            }
            if verbose {
                println!("Background: Re-indexing complete");
            }
            reindexing.store(false, Ordering::SeqCst);
        });
    }

    // Compute a simple directory hash
    fn compute_directory_hash(dir: &Path) -> io::Result<u64> {
        let mut hasher = DefaultHasher::new();
        let mut entries = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH).unwrap().as_secs();
            
            entry.file_name().to_string_lossy().hash(&mut hasher);
            metadata.len().hash(&mut hasher);
            modified.hash(&mut hasher);
            
            if metadata.is_dir() {
                entries.push(entry.path());
            }
        }

        // Sort subdirs so hashing is consistent
        entries.sort();
        for subdir in entries {
            if let Ok(hash) = Self::compute_directory_hash(&subdir) {
                hash.hash(&mut hasher);
            }
        }

        Ok(hasher.finish())
    }

    // Decide if we need to reindex a given directory
    fn needs_reindex(&self, dir: &Path) -> io::Result<bool> {
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        if let Some(dir_hash) = self.dir_hashes.hashes.get(dir) {
            if current_time - dir_hash.last_check < HASH_CACHE_DURATION {
                // Recently checked => skip
                return Ok(false);
            }
        }

        let new_hash = Self::compute_directory_hash(dir)?;
        Ok(match self.dir_hashes.hashes.get(dir) {
            Some(dir_hash) => new_hash != dir_hash.hash,
            None => true,
        })
    }

    // Update directory's hash after indexing
    fn update_directory_hash(&mut self, dir: &Path) -> io::Result<()> {
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let new_hash = Self::compute_directory_hash(dir)?;
        
        self.dir_hashes.hashes.insert(
            dir.to_path_buf(),
            DirectoryHash {
                path: dir.to_path_buf(),
                hash: new_hash,
                last_check: current_time,
            },
        );
        self.dir_hashes.save()
    }

    // Index a single directory
    fn index_directory(&mut self, dir: &Path) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        // Check if directory needs reindex
        if !self.needs_reindex(dir)? {
            if self.verbose {
                println!("Directory unchanged, skipping: {}", dir.display());
            }
            return Ok(());
        }

        if self.verbose {
            println!("Changes detected, indexing: {}", dir.display());
        }

        // Walk the directory
        for entry in WalkDir::new(dir).follow_links(true).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();

            // If we already indexed this path, check mod time
            if self.index.contains_file(path) {
                if let Ok(metadata) = entry.metadata() {
                    let modified = metadata
                        .modified()
                        .unwrap_or(UNIX_EPOCH)
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    
                    if let Some(existing) = self.index.get_file_entry(path) {
                        if existing.modified == modified {
                            continue; // Not changed
                        }
                    }
                }
            }

            if self.verbose {
                println!("Indexing file: {}", path.display());
            }

            if let Ok(metadata) = entry.metadata() {
                let modified = metadata
                    .modified()
                    .unwrap_or(UNIX_EPOCH)
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                self.index.add_file(FileEntry {
                    path: path.to_path_buf(),
                    modified,
                    is_dir: metadata.is_dir(),
                });
            }
        }

        // Update hash, then save
        self.update_directory_hash(dir)?;
        self.index.save()?;
        Ok(())
    }

    // Gather standard user directories
    fn get_user_directories() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(user_dirs) = directories_next::UserDirs::new() {
            // Add standard directories
            let standard_dirs = [
                user_dirs.download_dir(),
                user_dirs.desktop_dir(),
                user_dirs.document_dir(),
                user_dirs.picture_dir(),
                user_dirs.audio_dir(),
                user_dirs.video_dir(),
                user_dirs.public_dir(),
                user_dirs.template_dir(),
            ];

            for dir in standard_dirs.iter().filter_map(|d| *d) {
                dirs.push(dir.to_path_buf());
            }

            // Add local app data directories
            if let Some(home) = user_dirs.home_dir().to_str() {
                #[cfg(target_os = "macos")]
                {
                    let app_dir = PathBuf::from(format!("{}/Applications", home));
                    if app_dir.exists() {
                        dirs.push(app_dir);
                    }

                    let system_app_dir = PathBuf::from("/Applications");
                    if system_app_dir.exists() {
                        dirs.push(system_app_dir);
                    }
                }

                #[cfg(target_os = "linux")]
                {
                    let app_dir = PathBuf::from(format!("{}/.local/share/applications", home));
                    if app_dir.exists() {
                        dirs.push(app_dir);
                    }
                }

                #[cfg(target_os = "windows")]
                {
                    let app_dir = PathBuf::from(format!("{}\\AppData\\Local\\Programs", home));
                    if app_dir.exists() {
                        dirs.push(app_dir);
                    }
                }
            }

            // Platform-specific: iCloud, OneDrive, etc.
            #[cfg(target_os = "macos")]
            if let Some(home) = user_dirs.home_dir().to_str() {
                let icloud_dir = PathBuf::from(format!("{}/Library/Mobile Documents", home));
                if icloud_dir.exists() {
                    dirs.push(icloud_dir.clone());
                    if let Ok(entries) = fs::read_dir(&icloud_dir) {
                        for entry in entries.filter_map(|e| e.ok()) {
                            if let Ok(metadata) = entry.metadata() {
                                if metadata.is_dir() {
                                    dirs.push(entry.path());
                                }
                            }
                        }
                    }
                }
            }

            #[cfg(target_os = "windows")]
            if let Ok(onedrive) = env::var("OneDriveConsumer") {
                dirs.push(PathBuf::from(onedrive));
            }
        }
        dirs
    }

    // Index the user's home directories
    fn index_home_directory(&mut self) -> io::Result<()> {
        let spinner_style = ProgressStyle::default_spinner()
            .template("{spinner:.green} {wide_msg}")
            .unwrap();

        let progress = ProgressBar::new_spinner();
        progress.set_style(spinner_style);
        
        let dirs = Self::get_user_directories();
        let total_dirs = dirs.len();
        let mut indexed_dirs = 0;

        for dir in dirs {
            if !dir.exists() {
                continue;
            }
            if self.verbose {
                progress.set_message(format!(
                    "Indexing directory ({}/{}): {}",
                    indexed_dirs + 1,
                    total_dirs,
                    dir.display()
                ));
            }

            self.index_directory(&dir)?;
            indexed_dirs += 1;
        }

        if self.verbose {
            progress.finish_with_message(format!("Indexed {} directories", indexed_dirs));
        }

        self.index.save()?;
        Ok(())
    }

    // The main search entry point
    fn search(&mut self, pattern: &str) -> io::Result<Vec<PathBuf>> {
        let timer = std::time::Instant::now();
        
        // Perform the fuzzy/glob search on the existing index
        let mut results = self.index.search(pattern);
        
        // If we find stale or deleted entries, we might want to trigger reindex
        let mut needs_reindex = false;
        let mut valid_results = Vec::new();
        
        for path in &results {
            match fs::metadata(path) {
                Ok(metadata) => {
                    let current_modified = metadata
                        .modified()
                        .unwrap_or(UNIX_EPOCH)
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                        
                    // Compare to index
                    if let Some(entry) = self.index.get_file_entry(path) {
                        if entry.modified == current_modified {
                            valid_results.push(path.clone());
                            continue;
                        }
                    }
                    needs_reindex = true;
                    valid_results.push(path.clone());
                }
                Err(_) => {
                    // File doesn't exist => definitely need reindex
                    needs_reindex = true;
                }
            }
        }

        // If something changed, spawn background reindex
        if needs_reindex {
            if self.verbose {
                println!("Changes detected, triggering background re-index");
            }
            self.background_reindex(self.verbose, Self::get_user_directories());
        }

        // If no valid results, try a quick partial reindex (lazy) on recently modified directories
        if valid_results.is_empty() {
            if self.verbose {
                println!("No results found, performing lazy re-index...");
            }

            // Index recently modified directories first
            let recent_dirs = self.get_recently_modified_directories();
            for dir in recent_dirs {
                let _ = self.index_directory(&dir);
            }

            // Search again
            results = self.index.search(pattern);
            valid_results = results.into_iter().filter(|p| p.exists()).collect();

            // If still empty, do a full reindex
            if valid_results.is_empty() {
                for dir in Self::get_user_directories() {
                    let _ = self.index_directory(&dir);
                }
                // Final search
                let final_results = self.index.search(pattern);
                valid_results = final_results.into_iter().filter(|p| p.exists()).collect();
            }
        }

        // Sort final results by recency or other heuristics
        valid_results.sort_by(|a, b| {
            let a_score = self.compute_result_score(a);
            let b_score = self.compute_result_score(b);
            b_score.partial_cmp(&a_score).unwrap_or(std::cmp::Ordering::Equal)
        });

        if self.verbose {
            println!("Search completed in {:?}", timer.elapsed());
        }

        Ok(valid_results)
    }

    // Return directories checked within the last 24 hours
    fn get_recently_modified_directories(&self) -> Vec<PathBuf> {
        let mut recent_dirs = Vec::new();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        for (path, hash) in &self.dir_hashes.hashes {
            // E.g. if last_check was within 24 hours
            if current_time - hash.last_check < 3600 * 24 {
                recent_dirs.push(path.clone());
            }
        }
        recent_dirs
    }

    // Compute a "score" for each result based on recency, directory importance, etc.
    fn compute_result_score(&self, path: &Path) -> f64 {
        let mut score = 1.0;

        // 1) Boost or decay by recency
        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                    let age_hours = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() 
                                     - duration.as_secs()) as f64 / 3600.0;
                    // Exponential decay: half-life ~30 days (720 hours)
                    score *= (-age_hours / 720.0).exp();
                }
            }
        }

        // 2) Boost if in "important" user directories
        if let Some(user_dirs) = directories_next::UserDirs::new() {
            let important_dirs = [
                user_dirs.download_dir(),
                user_dirs.desktop_dir(),
                user_dirs.document_dir(),
            ];
            for dir in important_dirs.iter().filter_map(|d| *d) {
                if path.starts_with(dir) {
                    score *= 1.5;
                    break;
                }
            }
        }

        // 3) Light penalty for deeper paths
        let depth = path.components().count() as f64;
        score *= 1.0 / (depth * 0.1 + 1.0);

        score
    }
}

// --------------------
// main()
// --------------------
fn main() -> io::Result<()> {
    let opt = Opt::from_args();

    // Set up Ctrl+C handling
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
        println!("\nReceived Ctrl+C, finishing current operation...");
    }).expect("Error setting Ctrl+C handler");

    // If --reindex is specified with no pattern, just reindex everything then exit
    if opt.reindex && opt.pattern.is_none() {
        let mut manager = IndexManager::new(opt.verbose);
        manager.index_home_directory()?;
        println!("Reindexing completed!");
        return Ok(());
    }

    // Otherwise, we are doing a search scenario:
    let pattern = match &opt.pattern {
        Some(p) => p,
        None => {
            eprintln!("No pattern provided. Use --reindex to just reindex, or pass a pattern like `rfind \"chrome\"`.");
            std::process::exit(1);
        }
    };

    // Initialize manager
    let mut manager = IndexManager::new(opt.verbose);

    // If forced reindex (e.g. `rfind -f "chrome"`), reindex first
    if opt.force_reindex || manager.index.chunks.is_empty() {
        if opt.verbose {
            println!("Building initial index (forced)...");
        }
        manager.index_home_directory()?;
    }

    // Create a spinner if verbose
    let spinner = if opt.verbose {
        let sp = ProgressBar::new_spinner();
        sp.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {wide_msg}")
                .unwrap()
        );
        sp.set_message("Searching...");
        Some(sp)
    } else {
        None
    };

    // Perform the search
    let results = manager.search(pattern)?;

    // Finish spinner (if any) and clear
    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

    // Print results
    if results.is_empty() {
        if opt.verbose {
            println!("No matches found for: {}", pattern);
        }
        return Ok(());
    }

    // Sort for consistent output
    let mut sorted_results: Vec<_> = results.into_iter().collect();
    sorted_results.sort();

    if opt.verbose {
        println!("\nFound {} matches:", sorted_results.len());
    }
    for path in &sorted_results {
        if let Ok(metadata) = fs::metadata(path) {
            let file_type = if metadata.is_dir() { "DIR" } else { "FILE" };
            let size = if metadata.is_file() {
                humansize::format_size(metadata.len(), humansize::DECIMAL)
            } else {
                String::from("-")
            };
            if opt.verbose {
                println!("{:<5} {:>10} {}", file_type, size, path.display());
            } else {
                println!("{}", path.display());
            }
        } else {
            // Possibly deleted after indexing
            println!("{} (not accessible)", path.display());
        }
    }

    // If a background reindex was triggered, wait if verbose
    if manager.is_reindexing() && opt.verbose {
        println!("\nBackground reindexing in progress. Press Ctrl+C to exit...");
        while running.load(Ordering::SeqCst) && manager.is_reindexing() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    Ok(())
}
