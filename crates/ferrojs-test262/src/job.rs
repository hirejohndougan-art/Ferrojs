//! Finding tests and turning each one into the runs Test262 requires.
//!
//! Per INTERPRETING.md: a test runs twice (non-strict and strict) unless its
//! flags say otherwise; `module` tests run once as module code; `raw` tests run
//! once, unmodified, with no harness. Files named `*_FIXTURE*` are helpers for
//! other tests and never run on their own.

use crate::metadata::{self, Metadata, Negative};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How a single run of a test is executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    NonStrict,
    Strict,
    Module,
    Raw,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::NonStrict => "default",
            Mode::Strict => "strict",
            Mode::Module => "module",
            Mode::Raw => "raw",
        }
    }
}

/// Runs required for a test, from its flags.
pub fn modes(meta: &Metadata) -> Vec<Mode> {
    let f = &meta.flags;
    // `module` wins over `raw`: a raw module test runs as module code, just
    // without the harness (see `Harness::prelude`).
    if f.module {
        vec![Mode::Module]
    } else if f.raw {
        vec![Mode::Raw]
    } else if f.only_strict {
        vec![Mode::Strict]
    } else if f.no_strict {
        vec![Mode::NonStrict]
    } else {
        vec![Mode::NonStrict, Mode::Strict]
    }
}

/// One execution of one test.
#[derive(Clone, Debug)]
pub struct Job {
    /// Stable id: `<path relative to test/>:<mode>`, e.g. `built-ins/Array/length.js:strict`.
    pub id: String,
    /// Path relative to the suite's `test/` directory.
    pub rel_path: String,
    /// Absolute path of the test file (module tests resolve imports from it).
    pub path: PathBuf,
    pub mode: Mode,
    /// Harness code evaluated before the test. Starts with the strict
    /// directive in strict mode; empty for raw tests.
    pub prelude: String,
    /// The test's own source, unmodified.
    pub body: String,
    pub negative: Option<Negative>,
    pub is_async: bool,
    pub features: Vec<String>,
}

impl Job {
    /// The complete script for script-mode runs (prelude then body).
    /// Module runs evaluate `prelude` as a script, then `body` as a module.
    pub fn script_source(&self) -> String {
        let mut s = String::with_capacity(self.prelude.len() + self.body.len());
        s.push_str(&self.prelude);
        s.push_str(&self.body);
        s
    }

    /// Report grouping: the first two path components, e.g. `built-ins/Array`.
    pub fn area(&self) -> String {
        self.rel_path
            .splitn(3, '/')
            .take(2)
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// The contents of every file in the suite's `harness/` directory.
pub struct Harness {
    files: HashMap<String, String>,
}

impl Harness {
    pub fn load(test262_root: &Path) -> std::io::Result<Harness> {
        let dir = test262_root.join("harness");
        let mut files = HashMap::new();
        for entry in walkdir::WalkDir::new(&dir) {
            let entry = entry.map_err(std::io::Error::other)?;
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&dir)
                    .expect("walkdir yields paths under its root")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(rel, fs::read_to_string(entry.path())?);
            }
        }
        Ok(Harness { files })
    }

    fn get(&self, name: &str) -> Result<&str, String> {
        self.files
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("harness file `{name}` not found"))
    }

    /// Harness code for a run: assert.js, sta.js, doneprintHandle.js for
    /// async tests, then the test's own includes, in that order.
    pub fn prelude(&self, meta: &Metadata, mode: Mode) -> Result<String, String> {
        if mode == Mode::Raw || meta.flags.raw {
            return Ok(String::new());
        }
        let mut out = String::new();
        if mode == Mode::Strict {
            out.push_str("\"use strict\";\n");
        }
        let mut names: Vec<&str> = vec!["assert.js", "sta.js"];
        if meta.flags.is_async {
            names.push("doneprintHandle.js");
        }
        names.extend(meta.includes.iter().map(String::as_str));
        for name in names {
            out.push_str(self.get(name)?);
            out.push('\n');
        }
        Ok(out)
    }
}

/// Options that decide which tests are included.
#[derive(Clone, Debug)]
pub struct Selection {
    /// Only tests whose path (relative to `test/`) starts with one of these.
    /// Empty means all tests.
    pub prefixes: Vec<String>,
    /// Whether the host's main agent can block (`Atomics.wait`).
    pub can_block: bool,
    /// Run `intl402/` tests (ECMA-402). FerroJS reports these separately.
    pub include_intl: bool,
}

impl Default for Selection {
    fn default() -> Self {
        Selection {
            prefixes: Vec::new(),
            can_block: true,
            include_intl: true,
        }
    }
}

/// Why a test file was not turned into jobs.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub rel_path: String,
    pub reason: String,
}

/// Every test file under `test/`, sorted, fixtures excluded.
pub fn discover(test262_root: &Path, selection: &Selection) -> std::io::Result<Vec<String>> {
    let test_dir = test262_root.join("test");
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&test_dir) {
        let entry = entry.map_err(std::io::Error::other)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.ends_with(".js") || name.contains("_FIXTURE") {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&test_dir)
            .expect("walkdir yields paths under its root")
            .to_string_lossy()
            .replace('\\', "/");
        if !selection.include_intl
            && (rel.starts_with("intl402/") || rel.starts_with("staging/intl402/"))
        {
            continue;
        }
        if !selection.prefixes.is_empty()
            && !selection
                .prefixes
                .iter()
                .any(|p| rel.starts_with(p.as_str()))
        {
            continue;
        }
        out.push(rel);
    }
    out.sort();
    Ok(out)
}

/// Turns one test file into its jobs, or explains why it was skipped.
pub fn jobs_for(
    test262_root: &Path,
    harness: &Harness,
    rel_path: &str,
    selection: &Selection,
) -> Result<Vec<Job>, Skipped> {
    let skip = |reason: String| Skipped {
        rel_path: rel_path.to_owned(),
        reason,
    };
    let path = test262_root.join("test").join(rel_path);
    let body = fs::read_to_string(&path).map_err(|e| skip(format!("unreadable: {e}")))?;
    let meta = metadata::parse(&body).map_err(|e| skip(e.to_string()))?;

    if meta.flags.can_block_is_false && selection.can_block {
        return Err(skip("requires CanBlock = false".into()));
    }
    if meta.flags.can_block_is_true && !selection.can_block {
        return Err(skip("requires CanBlock = true".into()));
    }

    modes(&meta)
        .into_iter()
        .map(|mode| {
            let prelude = harness.prelude(&meta, mode).map_err(skip)?;
            Ok(Job {
                id: format!("{rel_path}:{}", mode.label()),
                rel_path: rel_path.to_owned(),
                path: path.clone(),
                mode,
                prelude,
                body: body.clone(),
                negative: meta.negative.clone(),
                is_async: meta.flags.is_async,
                features: meta.features.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::Flags;

    fn meta_with(flags: Flags) -> Metadata {
        Metadata {
            flags,
            ..Metadata::default()
        }
    }

    #[test]
    fn default_runs_twice() {
        assert_eq!(modes(&Metadata::default()), [Mode::NonStrict, Mode::Strict]);
    }

    #[test]
    fn flags_pick_one_mode() {
        assert_eq!(
            modes(&meta_with(Flags {
                only_strict: true,
                ..Flags::default()
            })),
            [Mode::Strict]
        );
        assert_eq!(
            modes(&meta_with(Flags {
                no_strict: true,
                ..Flags::default()
            })),
            [Mode::NonStrict]
        );
        assert_eq!(
            modes(&meta_with(Flags {
                module: true,
                ..Flags::default()
            })),
            [Mode::Module]
        );
        assert_eq!(
            modes(&meta_with(Flags {
                raw: true,
                ..Flags::default()
            })),
            [Mode::Raw]
        );
    }

    #[test]
    fn raw_module_runs_as_module_without_harness() {
        let meta = meta_with(Flags {
            raw: true,
            module: true,
            ..Flags::default()
        });
        assert_eq!(modes(&meta), [Mode::Module]);
        assert_eq!(harness().prelude(&meta, Mode::Module).unwrap(), "");
    }

    fn harness() -> Harness {
        let mut files = HashMap::new();
        for (k, v) in [
            ("assert.js", "A"),
            ("sta.js", "S"),
            ("doneprintHandle.js", "D"),
            ("x.js", "X"),
        ] {
            files.insert(k.to_owned(), v.to_owned());
        }
        Harness { files }
    }

    #[test]
    fn prelude_order_and_strict_prefix() {
        let meta = Metadata {
            includes: vec!["x.js".into()],
            flags: Flags {
                is_async: true,
                ..Flags::default()
            },
            ..Metadata::default()
        };
        let h = harness();
        assert_eq!(h.prelude(&meta, Mode::NonStrict).unwrap(), "A\nS\nD\nX\n");
        assert_eq!(
            h.prelude(&meta, Mode::Strict).unwrap(),
            "\"use strict\";\nA\nS\nD\nX\n"
        );
        assert_eq!(h.prelude(&meta, Mode::Raw).unwrap(), "");
    }

    #[test]
    fn missing_include_is_an_error() {
        let meta = Metadata {
            includes: vec!["nope.js".into()],
            ..Metadata::default()
        };
        assert!(harness().prelude(&meta, Mode::NonStrict).is_err());
    }
}
