//! Process memory, without platform bindings: macOS asks `footprint` (the
//! physical footprint Activity Monitor shows, compressed pages included) and
//! falls back to `ps` RSS; Linux reads `/proc/self/status`.

use std::process::Command;

/// Memory in use by this process now, in bytes; `None` where unsupported.
pub fn current_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let kb: u64 = s
            .lines()
            .find_map(|l| l.strip_prefix("VmRSS:"))?
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        Some(kb * 1024)
    }
    #[cfg(target_os = "macos")]
    {
        footprint(std::process::id()).or_else(|| ps_rss(std::process::id()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Physical footprint of a process (macOS `footprint`), bytes.
pub fn footprint(pid: u32) -> Option<u64> {
    let out = Command::new("footprint")
        .args(["-p", &pid.to_string(), "-f", "bytes", "--noCategories"])
        .output()
        .ok()?;
    parse_footprint(&String::from_utf8_lossy(&out.stdout))
}

fn parse_footprint(text: &str) -> Option<u64> {
    let rest = text.split("Footprint:").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Resident set size of a process from `ps`, bytes.
pub fn ps_rss(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kb * 1024)
}

/// How memory behaved over a run.
#[derive(Clone, Debug, PartialEq)]
pub struct Trend {
    /// Samples used (after the warm-up).
    pub samples: usize,
    /// Least-squares slope over the samples after the warm-up, bytes per
    /// simulated hour.
    pub slope_bytes_per_hour: f64,
    /// Slope times the span it was fitted over: the growth the line implies.
    pub implied_growth_bytes: f64,
    /// True when the implied growth is within the tolerance.
    pub stable: bool,
}

/// Fits a line to `(simulated seconds, bytes)` samples after discarding the
/// first quarter as warm-up. Memory counts as stable when the growth the line
/// implies over the fitted span is under 3% of the mean or 4 MiB, whichever
/// is larger. Needs at least 8 samples.
pub fn trend(samples: &[(f64, u64)]) -> Option<Trend> {
    if samples.len() < 8 {
        return None;
    }
    let used = &samples[samples.len() / 4..];
    let n = used.len() as f64;
    let (mx, my) = (
        used.iter().map(|s| s.0).sum::<f64>() / n,
        used.iter().map(|s| s.1 as f64).sum::<f64>() / n,
    );
    let sxx: f64 = used.iter().map(|s| (s.0 - mx).powi(2)).sum();
    let sxy: f64 = used.iter().map(|s| (s.0 - mx) * (s.1 as f64 - my)).sum();
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let span = used.last()?.0 - used.first()?.0;
    let implied = slope * span;
    let tolerance = (0.03 * my).max(4.0 * 1024.0 * 1024.0);
    Some(Trend {
        samples: used.len(),
        slope_bytes_per_hour: slope * 3600.0,
        implied_growth_bytes: implied,
        stable: implied <= tolerance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_footprint_output() {
        let t = "====\nzsh [1]: 64-bit    Footprint: 1687912 B (16384 bytes per page)\n====\n";
        assert_eq!(parse_footprint(t), Some(1_687_912));
        assert_eq!(parse_footprint("nothing"), None);
    }

    #[test]
    fn flat_and_growing_series() {
        let flat: Vec<_> = (0..40)
            .map(|i| (i as f64 * 600.0, 100_000_000 + (i % 3) * 100_000))
            .collect();
        assert!(trend(&flat).unwrap().stable);
        let leak: Vec<_> = (0..40)
            .map(|i| (i as f64 * 600.0, 100_000_000 + i * 2_000_000))
            .collect();
        let t = trend(&leak).unwrap();
        assert!(!t.stable);
        assert!(t.slope_bytes_per_hour > 1.0e7);
        assert!(trend(&flat[..5]).is_none());
    }

    #[test]
    fn reads_this_process() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(current_bytes().unwrap() > 1_000_000);
    }
}
