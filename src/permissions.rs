use std::fs::Metadata;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

/// Represents permission filter mode
#[derive(Debug, Clone, Copy)]
pub enum PermissionMode {
    User,   // u
    Group,  // g
    Others, // o
    All,    // a
}

/// Represents permission type
#[derive(Debug, Clone, Copy)]
pub enum PermissionType {
    Read,    // r
    Write,   // w
    Execute, // x
    SetID,   // s
}

/// Holds permission-based filter configuration
#[derive(Debug, Clone)]
pub struct PermissionFilter {
    pub mode: PermissionMode,
    pub perm_type: PermissionType,
    pub expected: bool, // true if permission should exist, false if it shouldn't
}

impl PermissionFilter {
    /// Parse a permission filter string in the format: [ugoa][+-][rwx]
    /// Examples: "u+x" (user has execute), "g-w" (group doesn't have write), "o=r" (others have read)
    pub fn parse(s: &str) -> Result<Self, String> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() != 3 {
            return Err("Permission filter must be exactly 3 characters".to_string());
        }

        let mode = match chars[0] {
            'u' => PermissionMode::User,
            'g' => PermissionMode::Group,
            'o' => PermissionMode::Others,
            'a' => PermissionMode::All,
            _ => return Err("Invalid permission mode. Use u|g|o|a".to_string()),
        };

        let expected = match chars[1] {
            '+' => true,
            '-' => false,
            _ => return Err("Invalid permission operator. Use + or -".to_string()),
        };

        let perm_type = match chars[2] {
            'r' => PermissionType::Read,
            'w' => PermissionType::Write,
            'x' => PermissionType::Execute,
            's' => PermissionType::SetID,
            _ => return Err("Invalid permission type. Use r|w|x|s".to_string()),
        };

        Ok(PermissionFilter {
            mode,
            perm_type,
            expected,
        })
    }

    /// Check if file permissions match the filter
    pub fn matches(&self, metadata: &Metadata) -> bool {
        #[cfg(unix)]
        {
            let mode = metadata.mode();
            let check_permission = |bits: u32| -> bool {
                match self.perm_type {
                    PermissionType::Read => (mode & bits & 0o444) != 0,
                    PermissionType::Write => (mode & bits & 0o222) != 0,
                    PermissionType::Execute => (mode & bits & 0o111) != 0,
                    PermissionType::SetID => match self.mode {
                        PermissionMode::User => (mode & 0o4000) != 0,  // setuid
                        PermissionMode::Group => (mode & 0o2000) != 0, // setgid
                        _ => false, // setid bit only valid for user/group
                    },
                }
            };

            let result = match self.mode {
                PermissionMode::User => check_permission(0o700),
                PermissionMode::Group => check_permission(0o070),
                PermissionMode::Others => check_permission(0o007),
                PermissionMode::All => {
                    check_permission(0o700) && check_permission(0o070) && check_permission(0o007)
                }
            };

            result == self.expected
        }
        #[cfg(windows)]
        {
            // Windows has a different permission model - we'll map some basic concepts
            let readonly = metadata.file_attributes() & 0x1 != 0;

            let result = match self.perm_type {
                PermissionType::Read => !readonly,
                PermissionType::Write => !readonly,
                PermissionType::Execute => {
                    // Just check executable bit in attributes
                    let is_directory = metadata.file_attributes() & 0x10 != 0;
                    let is_executable = metadata.file_attributes() & 0x40 != 0; // FILE_ATTRIBUTE_ARCHIVE
                    !is_directory && is_executable
                }
                PermissionType::SetID => false, // Windows doesn't have setuid/setgid
            };

            result == self.expected
        }
    }
}

/// Holds ownership filter configuration
#[derive(Debug, Clone)]
pub struct OwnershipFilter {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl OwnershipFilter {
    /// Create a new ownership filter
    pub fn new(uid: Option<u32>, gid: Option<u32>) -> Self {
        OwnershipFilter { uid, gid }
    }

    /// Check if file ownership matches the filter
    pub fn matches(&self, metadata: &Metadata) -> bool {
        #[cfg(unix)]
        {
            let uid_match = self.uid.map_or(true, |uid| metadata.uid() == uid);
            let gid_match = self.gid.map_or(true, |gid| metadata.gid() == gid);
            uid_match && gid_match
        }

        #[cfg(windows)]
        {
            // Windows doesn't use UID/GID - could potentially map to SID/ACL checks
            // For now, we'll just allow all ownership checks
            true
        }
    }
}

/// Extended file permissions for special Unix modes
#[derive(Debug, Clone, Copy)]
pub enum SpecialMode {
    SetUID, // s for user
    SetGID, // s for group
    Sticky, // t
}

/// Check for special mode bits
pub fn has_special_mode(metadata: &Metadata, mode: SpecialMode) -> bool {
    #[cfg(unix)]
    {
        let mode_bits = metadata.mode();
        match mode {
            SpecialMode::SetUID => (mode_bits & 0o4000) != 0,
            SpecialMode::SetGID => (mode_bits & 0o2000) != 0,
            SpecialMode::Sticky => (mode_bits & 0o1000) != 0,
        }
    }

    #[cfg(windows)]
    {
        // Windows doesn't support these special modes
        false
    }
}

/// Get string representation of file permissions (like ls -l)
pub fn get_permission_string(metadata: &Metadata) -> String {
    #[cfg(unix)]
    {
        let mode = metadata.mode();
        let mut result = String::with_capacity(10);

        // File type
        result.push(match mode & 0o170000 {
            0o140000 => 's', // socket
            0o120000 => 'l', // symlink
            0o100000 => '-', // regular file
            0o060000 => 'b', // block device
            0o040000 => 'd', // directory
            0o020000 => 'c', // character device
            0o010000 => 'p', // fifo
            _ => '?',
        });

        // User permissions
        result.push(if mode & 0o400 != 0 { 'r' } else { '-' });
        result.push(if mode & 0o200 != 0 { 'w' } else { '-' });
        result.push(if mode & 0o4000 != 0 {
            if mode & 0o100 != 0 {
                's'
            } else {
                'S'
            }
        } else if mode & 0o100 != 0 {
            'x'
        } else {
            '-'
        });

        // Group permissions
        result.push(if mode & 0o040 != 0 { 'r' } else { '-' });
        result.push(if mode & 0o020 != 0 { 'w' } else { '-' });
        result.push(if mode & 0o2000 != 0 {
            if mode & 0o010 != 0 {
                's'
            } else {
                'S'
            }
        } else if mode & 0o010 != 0 {
            'x'
        } else {
            '-'
        });

        // Others permissions
        result.push(if mode & 0o004 != 0 { 'r' } else { '-' });
        result.push(if mode & 0o002 != 0 { 'w' } else { '-' });
        result.push(if mode & 0o1000 != 0 {
            if mode & 0o001 != 0 {
                't'
            } else {
                'T'
            }
        } else if mode & 0o001 != 0 {
            'x'
        } else {
            '-'
        });

        result
    }
    #[cfg(windows)]
    {
        let attrs = metadata.file_attributes();
        let mut result = String::with_capacity(10);

        // Simplified Windows-style permissions
        result.push(if attrs & 0x10 != 0 { 'd' } else { '-' }); // Directory
        result.push(if attrs & 0x1 == 0 { 'w' } else { '-' }); // !Readonly
        result.push(if attrs & 0x1 == 0 { 'r' } else { '-' }); // !Readonly
        result.push(if attrs & 0x20 != 0 { 'a' } else { '-' }); // Archive
        result.push(if attrs & 0x2 != 0 { 'h' } else { '-' }); // Hidden
        result.push(if attrs & 0x4 != 0 { 's' } else { '-' }); // System

        result
    }
}
