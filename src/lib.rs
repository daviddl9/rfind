mod index;

pub use index::{
    FileEntry,
    IndexChunk,
    IndexManager,
    Index,
    DirectoryHashes,
    // etc. -- re-export anything you want testable or accessible outside
};
