/// Represents a size comparison operation
#[derive(Debug, Clone, Copy)]
pub enum SizeComparison {
    Exactly, // n
    Lesser,  // -n
    Greater, // +n
}

/// Represents a size unit for comparison
#[derive(Debug, Clone, Copy)]
pub enum SizeUnit {
    Bytes,     // c
    Kilobytes, // k
    Megabytes, // M
    Gigabytes, // G
}

/// Holds size-based filter configuration
#[derive(Debug, Clone)]
pub struct SizeFilter {
    comparison: SizeComparison,
    value: u64,
    unit: SizeUnit,
}

impl SizeFilter {
    /// Parse a size filter string in the format: [+-]N[ckmG]
    /// Examples: "+1M" (more than 1 MiB), "-500k" (less than 500 KiB), "1G" (about 1 GiB)
    pub fn parse(s: &str) -> Result<Self, String> {
        let (comparison, rest) = match s.chars().next() {
            Some('+') => (SizeComparison::Greater, &s[1..]),
            Some('-') => (SizeComparison::Lesser, &s[1..]),
            Some(_) => (SizeComparison::Exactly, s),
            None => return Err("Empty size filter".to_string()),
        };

        let unit = match rest.chars().last() {
            Some('c') => SizeUnit::Bytes,
            Some('k') => SizeUnit::Kilobytes,
            Some('M') => SizeUnit::Megabytes,
            Some('G') => SizeUnit::Gigabytes,
            _ => {
                return Err(
                    "Invalid size unit. Use c (bytes), k (KB), M (MB), or G (GB)".to_string(),
                )
            }
        };

        let value_str = &rest[..rest.len() - 1];
        let value = value_str
            .parse::<u64>()
            .map_err(|_| "Invalid number in size filter".to_string())?;

        Ok(SizeFilter {
            comparison,
            value,
            unit,
        })
    }

    /// Convert the size filter value to bytes
    pub fn to_bytes(&self) -> u64 {
        match self.unit {
            SizeUnit::Bytes => self.value,
            SizeUnit::Kilobytes => self.value * 1024,
            SizeUnit::Megabytes => self.value * 1024 * 1024,
            SizeUnit::Gigabytes => self.value * 1024 * 1024 * 1024,
        }
    }

    /// Check if a file's size matches the filter
    pub fn matches(&self, file_size: u64) -> bool {
        let target_size = self.to_bytes();

        match self.comparison {
            SizeComparison::Exactly => {
                // For exact matches, we'll allow a small tolerance based on the unit
                let tolerance = match self.unit {
                    SizeUnit::Bytes => 0,
                    SizeUnit::Kilobytes => 512,         // ±0.5KB
                    SizeUnit::Megabytes => 524_288,     // ±0.5MB
                    SizeUnit::Gigabytes => 536_870_912, // ±0.5GB
                };

                let lower = target_size.saturating_sub(tolerance);
                let upper = target_size.saturating_add(tolerance);
                file_size >= lower && file_size <= upper
            }
            SizeComparison::Lesser => file_size < target_size,
            SizeComparison::Greater => file_size > target_size,
        }
    }
}
