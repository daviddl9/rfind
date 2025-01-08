/// Enum to filter results by type.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFilter {
    #[default]
    Any,
    File,
    Dir,
    Symlink,
}

impl std::str::FromStr for TypeFilter {
    type Err = String;

    /// Converts user input to a `TypeFilter`.
    /// Example: "-t f" => `TypeFilter::File`, "-t d" => `TypeFilter::Dir`, "-t l" => `TypeFilter::Symlink`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "f" | "file" => Ok(TypeFilter::File),
            "d" | "dir" => Ok(TypeFilter::Dir),
            "l" | "link" | "symlink" => Ok(TypeFilter::Symlink),
            "any" => Ok(TypeFilter::Any),
            other => Err(format!("Invalid type filter '{}'. Use f|d|l|any.", other)),
        }
    }
}
