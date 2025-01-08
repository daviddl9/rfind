use std::time::{Duration, SystemTime};
/// Represents a time comparison operation
#[derive(Debug, Clone, Copy)]
pub enum TimeComparison {
    Exactly, // n
    Lesser,  // -n
    Greater, // +n
}

/// Represents a time unit for comparison
#[derive(Debug, Clone, Copy)]
pub enum TimeUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
}

/// Holds time-based filter configuration
#[derive(Debug, Clone)]
pub struct TimeFilter {
    comparison: TimeComparison,
    value: i64,
    unit: TimeUnit,
}

impl TimeFilter {
    /// Parse a time filter string in the format: [+-]N[smhd]
    /// Examples: "+1h" (more than 1 hour), "-2m" (less than 2 minutes), "3d" (about 3 days back)
    pub fn parse(s: &str) -> Result<Self, String> {
        let (comparison, rest) = match s.chars().next() {
            Some('+') => (TimeComparison::Greater, &s[1..]),
            Some('-') => (TimeComparison::Lesser, &s[1..]),
            Some(_) => (TimeComparison::Exactly, s),
            None => return Err("Empty time filter".to_string()),
        };

        let unit = match rest.chars().last() {
            Some('s') => TimeUnit::Seconds,
            Some('m') => TimeUnit::Minutes,
            Some('d') => TimeUnit::Days,
            Some('h') => TimeUnit::Hours,
            _ => return Err("Invalid time unit. Use 'm' for minutes or 'd' for days".to_string()),
        };

        let value_str = &rest[..rest.len() - 1];
        let value = value_str
            .parse::<i64>()
            .map_err(|_| "Invalid number in time filter".to_string())?;

        Ok(TimeFilter {
            comparison,
            value,
            unit,
        })
    }

    /// Convert the time filter value to a Duration
    pub fn to_duration(&self) -> Duration {
        match self.unit {
            TimeUnit::Seconds => Duration::from_secs(self.value.unsigned_abs()),
            TimeUnit::Minutes => Duration::from_secs(self.value.unsigned_abs() * 60),
            TimeUnit::Hours => Duration::from_secs(self.value.unsigned_abs() * 60 * 60),
            TimeUnit::Days => Duration::from_secs(self.value.unsigned_abs() * 24 * 60 * 60),
        }
    }

    /// Check if a file's modification time matches the filter
    pub fn matches(&self, file_time: SystemTime, now: SystemTime) -> bool {
        let duration = self.to_duration();
        let age = now.duration_since(file_time).unwrap_or(Duration::ZERO);

        match self.comparison {
            TimeComparison::Exactly => {
                let tolerance = match self.unit {
                    TimeUnit::Seconds => Duration::from_secs(2), // ±2 second
                    TimeUnit::Minutes => Duration::from_secs(30), // ±30 seconds
                    TimeUnit::Hours => Duration::from_secs(60 * 30), // ±30 minutes
                    TimeUnit::Days => Duration::from_secs(60 * 60 * 12), // ±12 hours
                };
                let lower = duration.saturating_sub(tolerance);
                let upper = duration.saturating_add(tolerance);
                age >= lower && age <= upper
            }
            TimeComparison::Lesser => age < duration,
            TimeComparison::Greater => age > duration,
        }
    }
}
