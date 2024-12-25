use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use glob::Pattern;
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::WalkDir;
use serde::{Serialize, Deserialize};
use bincode::{serialize_into, deserialize_from};
use strsim::{jaro_winkler, normalized_levenshtein};
use directories_next;
use directories_next::BaseDirs;

// --------------------------------------------------
// Constants, Structs, and Shared Utilities
// --------------------------------------------------

const FUZZY_THRESHOLD: f64 = 0.8;  // Minimum similarity score to consider a match
const CHUNK_SIZE: usize = 1000;
const HASH_CACHE_DURATION: u64 = 3600; // 1 hour in seconds

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: PathBuf,
    pub score: f64,
}

impl SearchResult {
    pub fn new(path: PathBuf, score: f64) -> Self {
        Self { path, score }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub modified: u64,
    pub is_dir: bool,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct DirectoryHash {
    pub path: PathBuf,
    pub hash: u64,
    pub last_check: u64,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct DirectoryHashes {
    pub hashes: HashMap<PathBuf, DirectoryHash>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct IndexChunk {
    pub files: HashMap<PathBuf, FileEntry>,
    pub terms: HashMap<String, HashSet<PathBuf>>,
}

#[derive(Debug, Default)]
pub struct Index {
    pub chunks: Vec<IndexChunk>,
    pub current_chunk: IndexChunk,
    pub files_in_current_chunk: usize,
}

// --------------------------------------------------
// DirectoryHashes
// --------------------------------------------------
impl DirectoryHashes {
    pub fn load() -> Self {
        if let Ok(rfind_dir) = get_rfind_dir() {
            let hash_path = rfind_dir.join("dir_hashes.bin");
            if let Ok(file) = File::open(hash_path) {
                if let Ok(hashes) = deserialize_from(BufReader::new(file)) {
                    return hashes;
                }
            }
        }
        Self::default()
    }
    
    pub fn save(&self) -> io::Result<()> {
        let rfind_dir = get_rfind_dir()?;
        fs::create_dir_all(&rfind_dir)?;
        let hash_path = rfind_dir.join("dir_hashes.bin");
        let file = File::create(hash_path)?;
        serialize_into(BufWriter::new(file), self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }   
}

// --------------------------------------------------
// Index
// --------------------------------------------------
impl Index {
    /// Load an existing index from disk if possible
    pub fn load() -> Option<Self> {
        // Try to get a BaseDirs instance (home directory, cache directory, etc.)
        let base_dirs = BaseDirs::new()?;

        // Use home_dir() + ".rfind" => ~/.rfind on Unix, 
        // C:\Users\<user>\.rfind on Windows, etc.
        let index_dir = base_dirs.home_dir().join(".rfind");
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

    /// Save the current index to disk
    pub fn save(&self) -> io::Result<()> {
        // Obtain a cross-platform home directory using directories_next
        let base_dirs = BaseDirs::new().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "Could not determine home directory")
        })?;
        
        // Create ~/.rfind (or the Windows equivalent)
        let index_dir = base_dirs.home_dir().join(".rfind");
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

    /// Construct a new Index (loading from disk if available, else empty)
    pub fn new() -> Self {
        Self::load().unwrap_or_default()
    }

    // --------------------------------------------------
    // Searching
    // --------------------------------------------------

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

    fn search_chunk_fuzzy(
        &self,
        chunk: &IndexChunk,
        search_terms: &[String],
        glob_pattern: &Pattern
    ) -> Vec<SearchResult> {
        let mut results = Vec::new();

        for (path, _) in &chunk.files {
            // Check the glob
            if glob_pattern.as_str() != "**/*" && !glob_pattern.matches(&path.to_string_lossy()) {
                continue;
            }

            let path_str = path.to_string_lossy();
            let filename = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();

            let mut min_score: f64 = 1.0;
            let mut found_all_terms = true;

            // Each term must match path or filename
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

    pub fn search(&self, pattern: &str) -> Vec<PathBuf> {
        let is_pure_glob = pattern.contains('*') || pattern.contains('?');

        // If we detect a glob, build a Pattern
        let glob_pattern = if pattern.starts_with("**") {
            Pattern::new(pattern).unwrap_or_else(|_| Pattern::new("**/*").unwrap())
        } else if is_pure_glob {
            Pattern::new(&format!("**/{}", pattern)).unwrap_or_else(|_| Pattern::new("**/*").unwrap())
        } else {
            Pattern::new("**/*").unwrap()
        };

        // Split pattern into search terms (only if not a pure glob)
        let search_terms = if is_pure_glob {
            Vec::new()
        } else {
            pattern.split_whitespace().map(|s| s.to_string()).collect()
        };

        let mut all_results = Vec::new();

        // Search historical chunks
        for chunk in &self.chunks {
            let chunk_results = self.search_chunk_fuzzy(chunk, &search_terms, &glob_pattern);
            all_results.extend(chunk_results);
        }

        // Search current chunk
        let current_results = self.search_chunk_fuzzy(&self.current_chunk, &search_terms, &glob_pattern);
        all_results.extend(current_results);

        // Sort by best fuzzy score first, remove duplicates
        all_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let mut seen = HashSet::new();
        all_results
            .into_iter()
            .filter(|r| seen.insert(r.path.clone()))
            .map(|r| r.path)
            .collect()
    }

    // --------------------------------------------------
    // Indexing
    // --------------------------------------------------

    pub fn get_file_entry(&self, path: &Path) -> Option<&FileEntry> {
        if let Some(entry) = self.current_chunk.files.get(path) {
            return Some(entry);
        }
        for chunk in &self.chunks {
            if let Some(entry) = chunk.files.get(path) {
                return Some(entry);
            }
        }
        None
    }

    pub fn contains_file(&self, path: &Path) -> bool {
        if self.current_chunk.files.contains_key(path) {
            return true;
        }
        self.chunks.iter().any(|chunk| chunk.files.contains_key(path))
    }

    fn extract_terms(path: &Path) -> Vec<String> {
        let path_str = path.to_string_lossy().to_lowercase();
        let mut terms = Vec::new();

        // Add filename
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

    pub fn add_file(&mut self, entry: FileEntry) {
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

        if self.files_in_current_chunk >= CHUNK_SIZE {
            let full_chunk = std::mem::replace(&mut self.current_chunk, IndexChunk::default());
            self.chunks.push(full_chunk);
            self.files_in_current_chunk = 0;
        }
    }
}

fn get_rfind_dir() -> io::Result<PathBuf> {
    if let Some(base_dirs) = directories_next::BaseDirs::new() {
        // E.g. store indexing data in ~/.rfind or an OS-appropriate location
        Ok(base_dirs.home_dir().join(".rfind"))
    } else {
        // If there's truly no home directory, bail out
        Err(io::Error::new(io::ErrorKind::Other, "No home directory found."))
    }
}


// --------------------------------------------------
// IndexManager
// --------------------------------------------------
#[derive(Debug)]
pub struct IndexManager {
    pub index: Index,
    pub verbose: bool,
    pub dir_hashes: DirectoryHashes,
    pub reindexing: Arc<AtomicBool>,
}

impl IndexManager {
    pub fn new(verbose: bool) -> Self {
        Self {
            index: Index::new(),
            verbose,
            dir_hashes: DirectoryHashes::load(),
            reindexing: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_reindexing(&self) -> bool {
        self.reindexing.load(Ordering::SeqCst)
    }

    pub fn background_reindex(&self, verbose: bool, dirs: Vec<PathBuf>) {
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

    pub fn index_directory(&mut self, dir: &Path) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        if !self.needs_reindex(dir)? {
            if self.verbose {
                println!("Directory unchanged, skipping: {}", dir.display());
            }
            return Ok(());
        }

        if self.verbose {
            println!("Changes detected, indexing: {}", dir.display());
        }

        for entry in WalkDir::new(dir).follow_links(true).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
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

        self.update_directory_hash(dir)?;
        self.index.save()?;
        Ok(())
    }

    pub fn index_home_directory(&mut self) -> io::Result<()> {
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

    pub fn search(&mut self, pattern: &str) -> io::Result<Vec<PathBuf>> {
        let timer = std::time::Instant::now();
        let mut results = self.index.search(pattern);

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
                    needs_reindex = true;
                }
            }
        }

        if needs_reindex {
            if self.verbose {
                println!("Changes detected, triggering background re-index");
            }
            self.background_reindex(self.verbose, Self::get_user_directories());
        }

        if valid_results.is_empty() {
            if self.verbose {
                println!("No results found, performing lazy re-index...");
            }
            let recent_dirs = self.get_recently_modified_directories();
            for dir in recent_dirs {
                let _ = self.index_directory(&dir);
            }

            results = self.index.search(pattern);
            valid_results = results.into_iter().filter(|p| p.exists()).collect();

            if valid_results.is_empty() {
                for dir in Self::get_user_directories() {
                    let _ = self.index_directory(&dir);
                }
                let final_results = self.index.search(pattern);
                valid_results = final_results.into_iter().filter(|p| p.exists()).collect();
            }
        }

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

    // -----------------------------------------
    // Helpers for reindex logic and scoring
    // -----------------------------------------
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

        entries.sort();
        for subdir in entries {
            if let Ok(hash) = Self::compute_directory_hash(&subdir) {
                hash.hash(&mut hasher);
            }
        }

        Ok(hasher.finish())
    }

    fn needs_reindex(&self, dir: &Path) -> io::Result<bool> {
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        if let Some(dir_hash) = self.dir_hashes.hashes.get(dir) {
            if current_time - dir_hash.last_check < HASH_CACHE_DURATION {
                return Ok(false);
            }
        }
        let new_hash = Self::compute_directory_hash(dir)?;
        Ok(match self.dir_hashes.hashes.get(dir) {
            Some(dir_hash) => new_hash != dir_hash.hash,
            None => true,
        })
    }

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

    fn get_recently_modified_directories(&self) -> Vec<PathBuf> {
        let mut recent_dirs = Vec::new();
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

        for (path, hash) in &self.dir_hashes.hashes {
            if current_time - hash.last_check < 3600 * 24 {
                recent_dirs.push(path.clone());
            }
        }
        recent_dirs
    }

    fn compute_result_score(&self, path: &Path) -> f64 {
        let mut score = 1.0;

        if let Ok(metadata) = fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                    let age_hours = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
                                     - duration.as_secs()) as f64 / 3600.0;
                    score *= (-age_hours / 720.0).exp(); // 30-day half-life
                }
            }
        }

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

        let depth = path.components().count() as f64;
        score *= 1.0 / (depth * 0.1 + 1.0);

        score
    }

    // -----------------------------------------
    // Get standard user directories
    // -----------------------------------------
    pub fn get_user_directories() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(user_dirs) = directories_next::UserDirs::new() {
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
}

// --------------------------------------------------
// Unit Tests Within This Module
// --------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match_exact_substring() {
        let haystack = "Documents";
        let needle = "doc";
        let score_opt = Index::fuzzy_match(haystack, needle);
        assert!(score_opt.is_some(), "Should match substring");
        assert!(score_opt.unwrap() > 0.9, "Should have a high fuzzy match score");
    }

    #[test]
    fn test_fuzzy_match_no_match() {
        let haystack = "Desktop";
        let needle = "random";
        let score_opt = Index::fuzzy_match(haystack, needle);
        assert!(score_opt.is_none(), "No match expected for unrelated strings");
    }
}
