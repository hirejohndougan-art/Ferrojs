//! The interface between the runner and a JavaScript engine.
//!
//! An engine only reports what happened (an error with its phase and name,
//! and what the test printed). Deciding pass or fail is the runner's job
//! (`judge.rs`), so every engine is held to exactly the same rules.

use crate::job::{Job, Mode};
use crate::metadata::Phase;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::Duration;

/// An uncaught error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thrown {
    /// `None` when the engine cannot tell which phase it came from.
    pub phase: Option<Phase>,
    /// The thrown value's constructor name, e.g. `SyntaxError`, `Test262Error`.
    pub name: String,
    pub message: String,
}

/// What happened when an engine ran a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Execution {
    /// The test ran to completion (with or without an uncaught error).
    Completed {
        error: Option<Thrown>,
        printed: Vec<String>,
    },
    /// The engine can't run this yet. Counted as "not run", never as passing.
    Unsupported(String),
    Timeout,
    /// The engine process died or misbehaved.
    Crash(String),
}

pub trait Engine: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self, job: &Job) -> Execution;
}

/// FerroJS itself. It cannot run JavaScript yet; this adapter is where the
/// interpreter plugs in during Phase 1.
pub struct FerroJs;

impl Engine for FerroJs {
    fn name(&self) -> &str {
        "ferrojs"
    }

    fn run(&self, _job: &Job) -> Execution {
        Execution::Unsupported("FerroJS cannot run JavaScript yet".into())
    }
}

#[derive(Serialize)]
struct WireJob<'a> {
    kind: &'static str,
    /// Script runs: the full source. Module runs: the prelude script.
    prelude: &'a str,
    /// Module runs only: the module source.
    source: &'a str,
    path: String,
    is_async: bool,
    timeout_ms: u64,
}

#[derive(Deserialize)]
struct WireResult {
    error: Option<Thrown>,
    #[serde(default)]
    printed: Vec<String>,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Worker {
    fn spawn(command: &[String]) -> std::io::Result<Worker> {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Worker {
            child,
            stdin,
            lines,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An engine running as long-lived worker processes that speak one JSON
/// object per line. Used for reference engines (Node.js today; SpiderMonkey
/// later) so the runner can be checked against known-good results, and for
/// the differential testing in the quality plan.
pub struct External {
    name: String,
    command: Vec<String>,
    timeout: Duration,
    idle: Mutex<Vec<Worker>>,
}

impl External {
    pub fn new(name: impl Into<String>, command: Vec<String>, timeout: Duration) -> External {
        assert!(!command.is_empty(), "external engine needs a command");
        External {
            name: name.into(),
            command,
            timeout,
            idle: Mutex::new(Vec::new()),
        }
    }

    /// Node.js with the bundled driver (`drivers/node-driver.js`).
    pub fn node(timeout: Duration) -> External {
        let driver: PathBuf = [env!("CARGO_MANIFEST_DIR"), "drivers", "node-driver.js"]
            .iter()
            .collect();
        External::new(
            "node",
            vec![
                "node".into(),
                "--experimental-vm-modules".into(),
                "--expose-gc".into(),
                "--no-warnings".into(),
                driver.to_string_lossy().into_owned(),
            ],
            timeout,
        )
    }

    fn take_worker(&self) -> std::io::Result<Worker> {
        let pooled = self.idle.lock().expect("worker pool lock").pop();
        match pooled {
            Some(w) => Ok(w),
            None => Worker::spawn(&self.command),
        }
    }
}

impl Engine for External {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self, job: &Job) -> Execution {
        let script;
        let wire = match job.mode {
            Mode::Module => WireJob {
                kind: "module",
                prelude: &job.prelude,
                source: &job.body,
                path: job.path.to_string_lossy().into_owned(),
                is_async: job.is_async,
                timeout_ms: self.timeout.as_millis() as u64,
            },
            _ => {
                script = job.script_source();
                WireJob {
                    kind: "script",
                    prelude: &script,
                    source: "",
                    path: job.path.to_string_lossy().into_owned(),
                    is_async: job.is_async,
                    timeout_ms: self.timeout.as_millis() as u64,
                }
            }
        };
        let line = match serde_json::to_string(&wire) {
            Ok(l) => l,
            Err(e) => return Execution::Crash(format!("could not encode job: {e}")),
        };

        let mut worker = match self.take_worker() {
            Ok(w) => w,
            Err(e) => return Execution::Crash(format!("could not start {}: {e}", self.command[0])),
        };
        if writeln!(worker.stdin, "{line}")
            .and_then(|_| worker.stdin.flush())
            .is_err()
        {
            return Execution::Crash("worker stopped accepting jobs".into());
        }
        // The driver enforces the test's own timeout; this is a hard backstop.
        let deadline = self.timeout + Duration::from_secs(5);
        match worker.lines.recv_timeout(deadline) {
            Ok(reply) => {
                let result = serde_json::from_str::<WireResult>(&reply);
                self.idle.lock().expect("worker pool lock").push(worker);
                match result {
                    Ok(r) => Execution::Completed {
                        error: r.error,
                        printed: r.printed,
                    },
                    Err(e) => Execution::Crash(format!("bad reply from worker: {e}")),
                }
            }
            // Dropping the worker kills it; a fresh one is started next time.
            Err(RecvTimeoutError::Timeout) => Execution::Timeout,
            Err(RecvTimeoutError::Disconnected) => Execution::Crash("worker exited".into()),
        }
    }
}
