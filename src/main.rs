use glob::Pattern;
use std::{
    collections::{HashMap, HashSet},
    env,
    fs::{self, File},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
    thread
};
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::WalkDir;
use structopt::StructOpt;
use bincode::{serialize_into, deserialize_from};
use serde::{Serialize, Deserialize};

const CHUNK_SIZE: usize = 1000;
const SAVE_INTERVAL: usize = 5000;

#[derive(StructOpt, Debug)]
#[structopt(name = "rfind", about = "Fast home directory search tool")]
struct Opt {
    /// Search pattern
    #[structopt(name = "PATTERN")]
    pattern: String,

    /// Verbose output
    #[structopt(short, long)]
    verbose: bool,

    /// Force reindex
    #[structopt(short, long)]
    force_reindex: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FileEntry {
    path: PathBuf,
    modified: u64,
    is_dir: bool,
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

impl Index {
    fn new() -> Self {
        Self::load().unwrap_or_default()
    }

    fn contains_file(&self, path: &Path) -> bool {
        // Check current chunk
        if self.current_chunk.files.contains_key(path) {
            return true;
        }
        // Check all other chunks
        self.chunks.iter().any(|chunk| chunk.files.contains_key(path))
    }

    fn load() -> Option<Self> {
        let home = env::var("HOME").ok()?;
        let index_dir = PathBuf::from(home).join(".rfind");
        fs::create_dir_all(&index_dir).ok()?;

        let mut chunks = Vec::new();
        let mut chunk_id = 0;

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

    fn save(&self) -> io::Result<()> {
        let home = env::var("HOME").map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let index_dir = PathBuf::from(home).join(".rfind");
        fs::create_dir_all(&index_dir)?;

        // Save existing chunks
        for (i, chunk) in self.chunks.iter().enumerate() {
            let chunk_path = index_dir.join(format!("chunk_{}.idx", i));
            let file = File::create(chunk_path)?;
            serialize_into(BufWriter::new(file), chunk)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        // Save current chunk if not empty
        if self.files_in_current_chunk > 0 {
            let chunk_path = index_dir.join(format!("chunk_{}.idx", self.chunks.len()));
            let file = File::create(chunk_path)?;
            serialize_into(BufWriter::new(file), &self.current_chunk)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }

        Ok(())
    }

    fn extract_terms(path: &Path) -> Vec<String> {
        path.to_string_lossy()
            .to_lowercase()
            .split([std::path::MAIN_SEPARATOR, '.', '_', '-', ' '])
            .filter(|s| !s.is_empty() && s.len() >= 2)
            .map(String::from)
            .collect()
    }

    fn add_file(&mut self, entry: FileEntry) {
        let terms = Self::extract_terms(&entry.path);

        // Add to current chunk
        for term in terms {
            self.current_chunk.terms
                .entry(term)
                .or_default()
                .insert(entry.path.clone());
        }

        self.current_chunk.files.insert(entry.path.clone(), entry);
        self.files_in_current_chunk += 1;

        // If chunk is full, move it to chunks list and create new chunk
        if self.files_in_current_chunk >= CHUNK_SIZE {
            let full_chunk = std::mem::replace(&mut self.current_chunk, IndexChunk::default());
            self.chunks.push(full_chunk);
            self.files_in_current_chunk = 0;
        }
    }

    fn search(&self, pattern: &str) -> Vec<PathBuf> {
        let glob_pattern = if pattern.contains('*') {
            Pattern::new(pattern).unwrap()
        } else {
            Pattern::new(&format!("**/{}", pattern)).unwrap()
        };

        let search_terms: Vec<String> = Self::extract_terms(Path::new(pattern));
        let mut results = HashSet::new();

        // Search in all chunks
        for chunk in &self.chunks {
            let chunk_results = self.search_chunk(chunk, &search_terms, &glob_pattern);
            results.extend(chunk_results);
        }

        // Search in current chunk
        let current_results = self.search_chunk(&self.current_chunk, &search_terms, &glob_pattern);
        results.extend(current_results);

        results.into_iter().collect()
    }

    fn search_chunk(&self, chunk: &IndexChunk, search_terms: &[String], glob_pattern: &Pattern) -> HashSet<PathBuf> {
        let mut results = HashSet::new();

        if search_terms.is_empty() {
            // If no search terms, match only by glob pattern
            results.extend(
                chunk.files.keys()
                    .filter(|path| glob_pattern.matches(&path.to_string_lossy()))
                    .cloned()
            );
        } else {
            // Start with the first term's matches
            if let Some(first_term) = search_terms.first() {
                if let Some(paths) = chunk.terms.get(first_term) {
                    results = paths.clone();

                    // Intersect with other terms
                    for term in search_terms.iter().skip(1) {
                        if let Some(paths) = chunk.terms.get(term) {
                            results.retain(|path| paths.contains(path));
                        } else {
                            results.clear();
                            break;
                        }
                    }

                    // Apply glob pattern
                    results.retain(|path| glob_pattern.matches(&path.to_string_lossy()));
                }
            }
        }

        results
    }
}

struct IndexManager {
    index: Index,
    verbose: bool,
}

impl IndexManager {
    fn filter_deleted_files(&self, paths: Vec<PathBuf>) -> (Vec<PathBuf>, bool) {
        let mut valid_paths = Vec::new();
        let mut found_deleted = false;
        
        for path in paths {
            if path.exists() {
                valid_paths.push(path);
            } else {
                found_deleted = true;
                if self.verbose {
                    println!("Detected deleted file: {}", path.display());
                }
            }
        }
        
        (valid_paths, found_deleted)
    }

    fn background_reindex(&self) {
        if self.verbose {
            println!("Starting background re-index...");
        }

        // Clone necessary data for background thread
        let dirs = Self::get_user_directories();
        let verbose = self.verbose;

        thread::spawn(move || {
            let mut manager = IndexManager::new(verbose);
            
            if verbose {
                println!("Background: Re-indexing all directories");
            }

            for dir in dirs {
                if let Err(e) = manager.index_directory(&dir) {
                    eprintln!("Background: Error indexing directory {}: {}", dir.display(), e);
                }
            }

            if verbose {
                println!("Background: Re-indexing complete");
            }
        });
    }

    fn new(verbose: bool) -> Self {
        Self {
            index: Index::new(),
            verbose,
        }
    }

    fn index_directory(&mut self, dir: &Path) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        if self.verbose {
            println!("Indexing directory: {}", dir.display());
        }

        for entry in WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            
            // Skip if already indexed
            if self.index.contains_file(path) {
                continue;
            }

            if self.verbose {
                println!("Indexing new file: {}", path.display());
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

        self.index.save()?;
        Ok(())
    }

    fn get_user_directories() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        
        // Use the directories-next crate to get standard directories
        if let Some(proj_dirs) = directories_next::ProjectDirs::from("com", "rfind", "rfind") {
            if let Some(data_dir) = proj_dirs.data_local_dir().parent() {
                // Include Trash/Recycle Bin
                #[cfg(target_os = "windows")]
                {
                    if let Ok(recycler) = std::env::var("SystemDrive") {
                        dirs.push(PathBuf::from(format!("{}\\$Recycle.Bin", recycler)));
                    }
                }
                #[cfg(target_family = "unix")]
                {
                    dirs.push(data_dir.join(".local/share/Trash/files"));
                }
            }
        }
        
        // Add user directories using the UserDirs API
        if let Some(user_dirs) = directories_next::UserDirs::new() {
            // These are cross-platform and will resolve to the correct paths
            if let Some(dir) = user_dirs.download_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.desktop_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.document_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.picture_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.audio_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.video_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.public_dir() { dirs.push(dir.to_path_buf()); }
            if let Some(dir) = user_dirs.template_dir() { dirs.push(dir.to_path_buf()); }
            // dirs.push(user_dirs.home_dir().to_path_buf());

            // Add iCloud directory if on macOS
            #[cfg(target_os = "macos")]
            if let Some(home) = user_dirs.home_dir().to_str() {
                let icloud_dir = PathBuf::from(format!("{}/Library/Mobile Documents", home));
                if icloud_dir.exists() {
                    dirs.push(icloud_dir.clone());
                    // Add all immediate subdirectories
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
        }
        dirs
    }

    fn index_home_directory(&mut self) -> io::Result<()> {
        let spinner_style = ProgressStyle::default_spinner()
            .template("{spinner:.green} {wide_msg}")
            .unwrap();

        let progress = ProgressBar::new_spinner();
        progress.set_style(spinner_style);
        
        let mut file_count = 0;

        for dir in Self::get_user_directories() {
            if !dir.exists() {
                continue;
            }

            if self.verbose {
                progress.set_message(format!("Indexing directory: {}", dir.display()));
            }

            for entry in WalkDir::new(&dir)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
        {
            let path = entry.path();
            
            if self.verbose {
                progress.set_message(format!("Indexing: {}", path.display()));
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

                file_count += 1;
                if file_count % SAVE_INTERVAL == 0 {
                    self.index.save()?;
                }
            }
        }
    }

        if self.verbose {
            progress.finish_with_message(format!("Indexed {} files", file_count));
        }

        self.index.save()?;
        Ok(())
    }

    fn search(&mut self, pattern: &str) -> io::Result<Vec<PathBuf>> {
        // First try searching with current index
        let mut results = self.index.search(pattern);

        // Check for deleted files first
        let (valid_results, found_deleted) = self.filter_deleted_files(results);

        // If deleted files found, trigger background reindex
        if found_deleted {
            if self.verbose {
                println!("Deleted files detected, triggering background re-index");
            }
            self.background_reindex();
        }

        // If no valid results found, do a lazy re-index of all directories
        if valid_results.is_empty() {
            if self.verbose {
                println!("No results found, performing lazy re-index...");
            }

            for dir in Self::get_user_directories() {
                self.index_directory(&dir)?;
            }

            // Search again after re-indexing
            results = self.index.search(pattern);
            // Check again for deleted files
            let (reindexed_results, found_deleted) = self.filter_deleted_files(results);
            
            // If deleted files found after reindex, trigger background refresh
            if found_deleted {
                self.background_reindex();
            }
            
            return Ok(reindexed_results);
        }

        Ok(valid_results)
    }
}

fn main() -> io::Result<()> {
    let opt = Opt::from_args();
    
    let mut manager = IndexManager::new(opt.verbose);
    
    // Only do initial indexing if forced or no index exists
    if opt.force_reindex || manager.index.chunks.is_empty() {
        if opt.verbose {
            println!("Building initial index...");
        }
        manager.index_home_directory()?;
    }
    
    let results = manager.search(&opt.pattern)?;
    
    if results.is_empty() && opt.verbose {
        println!("No matches found for: {}", opt.pattern);
    }
    
    for path in results {
        println!("{}", path.display());
    }
    
    Ok(())
}