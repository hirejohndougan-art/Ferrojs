//! The stored instruction-count baseline and the regression check.
//!
//! The baseline file has one `<benchmark id> <instructions>` line per
//! benchmark, plus a `# toolchain:` line naming the rustc and Valgrind
//! versions that produced it. Counts from different toolchains are not
//! comparable, so a mismatch is reported separately (see `tools/bench.sh`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// Benchmark id → instruction count (Callgrind `Ir`).
pub type Counts = BTreeMap<String, u64>;

#[derive(Debug, Default, PartialEq)]
pub struct Baseline {
    pub toolchain: String,
    pub counts: Counts,
}

impl Baseline {
    pub fn parse(text: &str) -> Result<Baseline, String> {
        let mut b = Baseline::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if let Some(t) = line.strip_prefix("# toolchain:") {
                b.toolchain = t.trim().to_string();
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || format!("line {}: expected `<id> <instructions>`", n + 1);
            let (id, count) = line.rsplit_once(' ').ok_or_else(bad)?;
            let count = count.parse().map_err(|_| bad())?;
            if b.counts.insert(id.trim().to_string(), count).is_some() {
                return Err(format!("line {}: duplicate benchmark {id}", n + 1));
            }
        }
        Ok(b)
    }

    pub fn render(&self) -> String {
        let mut s = String::from(
            "# FerroJS instruction-count baseline (D-015). Written by tools/bench.sh.\n\
             # One line per benchmark: <id> <Callgrind instructions>.\n",
        );
        writeln!(s, "# toolchain: {}", self.toolchain).unwrap();
        for (id, count) in &self.counts {
            writeln!(s, "{id} {count}").unwrap();
        }
        s
    }
}

/// Result of comparing a run with the baseline.
#[derive(Debug, Default)]
pub struct Comparison {
    /// Lines for the report, one per benchmark.
    pub report: Vec<String>,
    /// Benchmarks that got slower than the threshold allows.
    pub regressions: Vec<String>,
    /// Baseline benchmarks that didn't run.
    pub missing: Vec<String>,
}

impl Comparison {
    pub fn passed(&self) -> bool {
        self.regressions.is_empty() && self.missing.is_empty()
    }
}

/// Compares `new` counts with `base`. A benchmark regresses when its count
/// grows by more than `threshold_pct` percent.
pub fn compare(base: &Counts, new: &Counts, threshold_pct: f64) -> Comparison {
    let mut c = Comparison::default();
    for (id, &now) in new {
        match base.get(id) {
            None => c.report.push(format!("  new     {id}: {now}")),
            Some(&before) => {
                let change = if before == 0 {
                    if now == 0 {
                        0.0
                    } else {
                        f64::INFINITY
                    }
                } else {
                    (now as f64 - before as f64) * 100.0 / before as f64
                };
                let slower = change > threshold_pct;
                let tag = if slower { "SLOWER " } else { "ok     " };
                c.report
                    .push(format!("  {tag} {id}: {before} -> {now} ({change:+.2}%)"));
                if slower {
                    c.regressions.push(id.clone());
                }
            }
        }
    }
    for id in base.keys().filter(|id| !new.contains_key(*id)) {
        c.report.push(format!("  MISSING {id}"));
        c.missing.push(id.clone());
    }
    c
}

/// Reads every gungraun `summary.json` under `dir`. The id is the path of
/// the benchmark's output directory relative to `dir`, with `/` separators.
pub fn read_summaries(dir: &Path) -> Result<Counts, String> {
    let mut counts = Counts::new();
    walk(dir, dir, &mut counts)?;
    Ok(counts)
}

fn walk(root: &Path, dir: &Path, counts: &mut Counts) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            walk(root, &path, counts)?;
        } else if path.file_name().is_some_and(|n| n == "summary.json") {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let ir = instructions(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let rel = path.parent().unwrap().strip_prefix(root).unwrap();
            let id = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            counts.insert(id, ir);
        }
    }
    Ok(())
}

/// The total Callgrind instruction count in one gungraun summary.
pub fn instructions(summary_json: &str) -> Result<u64, String> {
    let v: serde_json::Value = serde_json::from_str(summary_json).map_err(|e| e.to_string())?;
    let profiles = v["profiles"].as_array().ok_or("no profiles")?;
    let callgrind = profiles
        .iter()
        .find(|p| p["tool"] == "Callgrind")
        .ok_or("no Callgrind profile")?;
    callgrind["data"]["total"]["metrics"]["Ir"]["values"]["new"]
        .as_u64()
        .ok_or_else(|| "no Ir count".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pairs: &[(&str, u64)]) -> Counts {
        pairs.iter().map(|&(k, v)| (k.to_string(), v)).collect()
    }

    #[test]
    fn baseline_round_trips() {
        let b = Baseline {
            toolchain: "rustc 1.0 / valgrind-3".into(),
            counts: counts(&[("a/b.x", 10), ("c", 0)]),
        };
        assert_eq!(Baseline::parse(&b.render()).unwrap(), b);
    }

    #[test]
    fn parse_rejects_garbage_and_duplicates() {
        assert!(Baseline::parse("a 1\na 2\n").is_err());
        assert!(Baseline::parse("a x\n").is_err());
        assert!(Baseline::parse("lonely\n").is_err());
    }

    #[test]
    fn three_percent_threshold() {
        let base = counts(&[("a", 1000), ("b", 1000), ("c", 1000)]);
        let new = counts(&[("a", 1030), ("b", 1031), ("c", 900), ("d", 5)]);
        let c = compare(&base, &new, 3.0);
        assert_eq!(c.regressions, vec!["b".to_string()]);
        assert!(c.missing.is_empty());
        assert!(!c.passed());
    }

    #[test]
    fn missing_benchmark_fails() {
        let c = compare(&counts(&[("a", 1)]), &counts(&[]), 3.0);
        assert_eq!(c.missing, vec!["a".to_string()]);
        assert!(!c.passed());
    }

    #[test]
    fn reads_gungraun_summary() {
        let json = r#"{"profiles":[{"tool":"Callgrind","data":{"parts":[],
            "total":{"metrics":{"Ir":{"values":{"new":10460}}}}}}]}"#;
        assert_eq!(instructions(json), Ok(10460));
        assert!(instructions(r#"{"profiles":[]}"#).is_err());
    }
}
