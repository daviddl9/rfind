use std::fs;
use tempfile::tempdir;

use rfind::IndexManager;

#[test]
fn test_index_and_search_temp_dir() {
    // Create a temporary directory
    let temp = tempdir().unwrap();
    let temp_path = temp.path();

    // Create a file we expect to find
    let file_path = temp_path.join("hello_rfind.txt");
    fs::write(&file_path, b"some content").unwrap();

    // Create an IndexManager
    let mut manager = IndexManager::new(false);

    // Index this temp directory
    manager.index_directory(temp_path).unwrap();

    // Now search for "rfind" or "hello" (to trigger partial substring match)
    let results = manager.search("rfind").unwrap();
    assert!(!results.is_empty(), "Should find the newly-created file by partial match");

    // Clean up is automatic when `temp` goes out of scope
}
