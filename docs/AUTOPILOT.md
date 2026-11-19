# FerroJS autopilot

FerroJS is built by scheduled Claude sessions with no human in the loop.
John chose full automation on 2026-11-19 (decision D-012): every run makes
its own decisions, builds, checks its work and merges. This file is the
procedure every run follows. Read it in full at the start of every run.

## Files that carry state between runs

| File | What it holds | Who updates it |
| --- | --- | --- |
| `docs/AUTOPILOT.md` | This procedure | Only a monthly review, with a logged reason |
| `docs/PLAN.md` | Goals, architecture, phases (from the planning document) | Monthly review only |
| `docs/DECISIONS.md` | Every design decision, accepted by the run that makes it | Any run that makes a decision |
| `docs/ROADMAP.md` | The task queue: phases, milestones, tasks, status | Every run |
| `docs/PROGRESS.md` | One entry per run: what was done, numbers, what's next | Every run |
| `docs/reports/` | Weekly and monthly reports | Weekly / monthly runs |

Runs have no memory of earlier runs. If something matters later, it goes in
one of these files.

## Each run, step by step

1. **Orient (keep it short).** Read this file, `docs/ROADMAP.md`, the last
   five entries of `docs/PROGRESS.md`, and `docs/DECISIONS.md`. Skim
   `docs/PLAN.md` sections relevant to the task.
2. **Check main is healthy.** Run the gate (below) on `main`. If it fails,
   the run's task is to fix `main`, nothing else.
3. **Special runs come first:**
   - First run on or after the 1st of a month: do the **monthly review**
     (below) instead of a task.
   - First run on or after a Sunday: write the **weekly report** (below),
     then continue with a task if there's room.
4. **Pick one task:** the first `[ ]` task in `docs/ROADMAP.md` whose
   dependencies are done, skipping tasks marked `[!]` (stuck) unless every
   other task is blocked. Mark it `[~]` (in progress) with the date.
5. **Size it.** A task must fit in one run: one coherent change, normally
   under about 1,500 changed lines. If it's bigger, split it into subtasks in
   the roadmap, commit that, and do the first subtask.
6. **Research when needed.** Use web search and the ECMAScript spec
   (tc39.es/ecma262), Test262's `INTERPRETING.md`, and the docs of the crates
   in `docs/PLAN.md`. Read other engines (V8, SpiderMonkey, JavaScriptCore,
   QuickJS, Boa) for ideas only; never copy their code (licensing).
7. **Decide.** Any choice that is hard to change later, or that a future run
   needs to know, gets a new `D-NNN` entry in `docs/DECISIONS.md` with
   status **Accepted (autopilot, date)**, the choice, what was rejected and
   why. Never contradict an accepted decision silently: supersede it with a
   new entry that explains why.
8. **Build on a branch.** `git switch -c auto/<yyyy-mm-dd>-<short-task-name>`.
   Write the code and its tests. Every bug fix lands with a test that
   reproduces it.
9. **Run the gate.** All of it must pass.
10. **Risky code gets an independent review.** If the task is marked
    `(risky)` in the roadmap, or the change touches `ferrojs-core`,
    `unsafe` code, the GC, JIT code generation or deoptimization: start a
    separate subagent that has not seen the work and ask it to review the
    diff for memory-safety, GC-rooting, soundness and spec bugs. Fix what it
    finds and re-run the gate. If it finds a problem you can't fix this run,
    don't merge.
11. **Merge.** Rebase on `origin/main`, re-run the gate if anything changed,
    then fast-forward `main` and push. Delete the branch.
12. **Record.** In the same push or a follow-up commit:
    - mark the task `[x]` in `docs/ROADMAP.md` (or `[ ]` again with a note if
      not finished, or `[!]` if stuck — see below);
    - append a `docs/PROGRESS.md` entry (format below).
13. **Stop.** One task per run. Leave the rest of the usage for other runs
    and for John.

### When a task fails

- Not finished but progressing: commit the work on the branch, push the
  branch, set the task back to `[ ]` with a note pointing at the branch.
  The next run continues from it.
- Three runs in a row make no progress on the same task: mark it `[!]` with
  a short description of what was tried and why it failed, and move on. The
  monthly review revisits stuck tasks, and may change approach, split the
  task, or pick a fallback from `docs/PLAN.md` (for example Cranelift instead
  of copy-and-patch, or Boa's parser instead of oxc).
- Never weaken the gate, delete a failing test, or shrink the Test262
  baseline to make a change pass.

## The gate

Nothing merges unless all of these pass:

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --release
tools/fetch-test262.sh
cargo run --release -p ferrojs-test262 -- --suite test262 \
  --baseline conformance/test262-baseline.txt \
  --write-baseline conformance/test262-baseline.txt
```

- The Test262 run must not lose any test in the baseline. New passes are
  written into the baseline in the same commit.
- As the roadmap adds them, the gate also includes: differential tests
  against Node.js, fuzzing smoke runs, Miri on `ferrojs-core`, and the
  instruction-count benchmarks (a slowdown over 3% on a key benchmark blocks
  the merge unless the decision log explains it).
- When a gate step is added, add it to this list and to `.github/workflows/ci.yml`.

## Pacing

John chose a middle pace: about three runs a day. Usage is protected by
keeping each run to **one task** and stopping when it's done. If a task
keeps growing, split it rather than working longer. Do not start extra
runs or loop.

The weekly report compares progress with the phase timeline in
`docs/PLAN.md`. If the project is far behind, the monthly review re-plans
(smaller scope, reorder, fallbacks); it does not raise the pace.

## PROGRESS.md entry format

```
## <yyyy-mm-dd hh:mm UTC> — <task id and name>
- Result: merged | partial (branch auto/...) | stuck | fixed main | review
- Changed: <one or two lines>
- Decisions: <D-NNN, or none>
- Test262: <passing runs> (<+/- vs previous>)
- Next: <the task the next run should pick, if not the obvious one>
```

## Weekly report (first run on or after Sunday)

Write `docs/reports/<yyyy>-W<ww>.md`: tasks finished, Test262 pass count and
trend, benchmark numbers once they exist, stuck tasks, new decisions, and
where the project stands against the phase timeline. Keep it under a page.
It's for John to read when he wants; nothing waits on him.

## Monthly review (first run on or after the 1st)

Step back from the task queue:

1. Read the last month's progress entries, reports and decisions.
2. Check the direction against `docs/PLAN.md`: are recent decisions
   consistent with the architecture? Is anything drifting toward a weaker
   design? Is the code getting harder to change?
3. Revisit every `[!]` task: change approach, split, or use a fallback.
4. Re-plan the next month's milestones in `docs/ROADMAP.md`; break the next
   milestone into run-sized tasks.
5. Write `docs/reports/<yyyy>-<mm>-review.md` with what changed and why.

## Rules that never change

- `unsafe` only in `ferrojs-core`, every block with a `// SAFETY:` comment (D-004).
- The interpreter is always the complete, correct implementation; every
  faster tier must fall back to it.
- No copied code from other engines. Dependencies must be MIT-compatible
  (see `docs/PLAN.md`, Licensing).
- Never rewrite git history on `main`, never force-push `main`.
- Don't commit secrets, large binaries, or the `test262/` checkout.
