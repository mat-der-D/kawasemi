---
name: kiro-auto-implement
description: Use when unattended, no-human-in-the-loop automation (a Claude Code Routine, cron trigger, or explicit request to "keep advancing the implementation") should pick the next actionable spec/task from the roadmap dependency graph and current tasks.md progress, implement and review it entirely through sub-agents, and push AI-reviewed work directly to the `agent` branch.
disable-model-invocation: true
allowed-tools: Read, Agent
argument-hint: [feature-name]
---

# kiro-auto-implement

## Overview

This skill turns one trigger (typically a Routine firing on a schedule) into one unit of unattended forward progress on the kawasemi implementation: pick the next eligible spec, drive `kiro-impl` to completion or a natural stopping point, tag each completed task, run a final AI review, and push straight to `agent`. There is no human review step — an AI reviewer's `APPROVED` verdict is the only gate before push.

**REQUIRED SUB-SKILLS:** `kiro-impl` (task execution engine this wraps), `kiro-review` (per-task and final review protocol), `kiro-verify-completion` (evidence gate), `kiro-validate-impl` (feature-level GO/NO-GO), `kiro-debug` (blocked-task investigation).

## MODEL & EFFORT RULE (applies to every Agent dispatch below, no exceptions)

Every `Agent` tool call performed anywhere in this procedure — the Step 0 dispatch of `main`, and every dispatch `main` performs later (implementer, reviewer, debugger, final reviewer) — MUST:
- pass `model: "sonnet"` (the current latest Sonnet model)
- prepend this line to the dispatched prompt verbatim: `Reasoning effort: medium. Think carefully through edge cases, verify assumptions against the actual repository state, and do not shortcut analysis before acting.`

## Step 0 — Dispatch (executed by whoever invoked this skill)

**Do not execute Steps 1+ yourself.** Your only job in this step:

1. Resolve the absolute path of this file (`.../.claude/skills/kiro-auto-implement/SKILL.md`).
2. Dispatch exactly one subagent via the `Agent` tool:
   - `subagent_type: "general-purpose"`
   - `model: "sonnet"`
   - no `isolation` (operate directly in the current working directory — this automation pushes straight to `origin/agent`, and a worktree would leave that push disconnected from the shared repo state)
   - `description`: "kiro-auto-implement orchestrator"
   - `prompt`: a self-contained instruction (fresh agents have no context) telling it: it is **`main`**, the orchestrator for autonomous kawasemi implementation; it must `Read` the absolute path from step 1 in full and execute **Steps 1 through 6 exactly as written**, acting as `main` throughout; include the effort line from the rule above; pass along `$ARGUMENTS` verbatim if the user supplied a feature-name override; instruct it to report back using the Step 6 output format.
3. Wait for `main`'s result and relay its final report to the user. Do not perform any git, spec-selection, or implementation actions yourself — that would violate the orchestrator-only contract this skill exists to enforce.

---

**Everything below this line is executed by `main` (the dispatched subagent), not by the invoker.**

## Step 1: Preflight & branch setup

- `git status --porcelain` in the current worktree. If dirty with changes you didn't just make, STOP and report — never discard pre-existing work.
- `git fetch origin`.
- Branch `agent`:
  - If local `agent` exists: `git checkout agent`, then fast-forward only (`git merge --ff-only origin/agent` if it exists remotely). If the fast-forward fails, STOP and report — never force or rebase over it.
  - Else if `origin/agent` exists: `git checkout -b agent origin/agent`.
  - Else: `git checkout -b agent origin/main` (first run ever).
- If `origin/main` has commits not yet in `agent` (humans may have added/changed specs), merge them in with a normal merge commit (`git merge origin/main`) — never rebase, never force. If it conflicts, STOP and report; do not resolve conflicts autonomously.
- Record `RUN_START_SHA=$(git rev-parse HEAD)` — everything committed after this point belongs to this run.

## Step 2: Select the next feature

If `$ARGUMENTS` names a feature explicitly, use it — but still run the eligibility rule in point 3 below before proceeding. If the named feature fails eligibility (already complete, or a dependency is incomplete), STOP and report why rather than silently falling back to auto-selection. Otherwise (no argument given):

1. Read `.kiro/steering/roadmap.md`, section "Specs (dependency order)" — this is the topologically-sorted Phase 1 list, each entry with a `Dependencies:` line.
2. For each spec in that order, compute completeness from `.kiro/specs/<name>/tasks.md`: complete iff there are zero remaining `- [ ] <n>.<m>` actionable lines (a spec with unresolved `_Blocked:_` tasks still pending is NOT complete).
3. A spec is **eligible** iff: it is not yet complete, `.kiro/specs/<name>/spec.json` has `"ready_for_implementation": true`, and every spec named in its `Dependencies:` is complete per step 2.
4. Pick the **first** eligible spec in roadmap order. That is the feature for this run.
5. If no Phase 1 spec is eligible, check "Future Phases" entries that already have a `.kiro/specs/<name>/` directory (meaning a spec was created since roadmap.md was written) and repeat 2-4 over those.
6. If nothing is eligible anywhere (all complete, or the next one has no spec directory yet — spec creation is out of scope for this skill), STOP: report that implementation is caught up and, if applicable, which spec needs `/kiro-spec-init` next. Skip to Step 6.

## Step 3: Drive implementation via `kiro-impl`, with per-task tagging

Invoke the `kiro-impl` skill for the chosen feature with **no task numbers** (autonomous mode) and follow its documented procedure exactly, with exactly one addition:

> Immediately after each per-task commit that `kiro-impl`'s "Commit (parent-only, selective staging)" step performs, before moving to the next task, run:
> `git tag -a agent/<feature>/<task-id> -m "<feature> <task-id>: <task description>"`
> on the commit you just made.

Do not alter any other part of `kiro-impl`'s behavior: implementer and reviewer are still fresh subagents dispatched per task (apply the Model & Effort Rule above to those dispatches too), `kiro-review` and `kiro-verify-completion` still gate every task, and `kiro-debug` still handles `BLOCKED`/rejection-round-3 per its existing bounded-retry rules. `kiro-impl` will run until its own natural stop: the feature's tasks are all complete and validated via `kiro-validate-impl` (GO/NO-GO), or a task is `BLOCKED`/`STOP_FOR_HUMAN` and no further tasks in the feature are actionable.

## Step 4: Handle the stop condition

- **All tasks complete, `kiro-validate-impl` returned GO**: proceed to Step 5.
- **NO-GO after `kiro-impl`'s capped remediation, or a task is `BLOCKED`**: this is not a failure of the run — proceed to Step 5 with whatever is committed so far. Do not attempt further remediation yourself (that would be `main` doing concrete work instead of delegating).
- **`STOP_FOR_HUMAN`**: the task plan itself needs human attention. Do not try to fix `tasks.md` yourself. Proceed to Step 5 with whatever is committed so far, and flag this clearly in the Step 6 report — "no human review" for code correctness does not extend to overriding a broken task decomposition.
- **Nothing was committed this run** (`git rev-parse HEAD` == `RUN_START_SHA`): skip Step 5 (nothing to push), go to Step 6.

## Step 5: Final review gate, then push

1. Diff the whole run: `git diff RUN_START_SHA...HEAD` (and `git log RUN_START_SHA..HEAD` for the commit list).
2. Dispatch one **final reviewer** subagent (fresh, per the Model & Effort Rule) with: the full run diff, the list of tasks/commits/tags produced, the feature's `requirements.md`/`design.md` paths, and the repo's build/test commands (discover the same way `kiro-impl`'s preflight does: `Cargo.toml` → `cargo check` / `cargo test`, etc.). Ask it to apply the `kiro-review` protocol at run scope — read the code, run the build/test commands itself, do not trust prior reports — and return a structured `## Review Verdict` / `- VERDICT: APPROVED|REJECTED` block. This is the AI review agent gate; there is no human approval step.
3. **REJECTED**: do NOT push. The work stays committed locally on `agent` (already individually task-reviewed) for the next run or a human to inspect. Report the findings verbatim in Step 6.
4. **APPROVED**: push everything from this run in one shot:
   - `git push origin agent`
   - `git push origin <tag1> <tag2> ...` (the exact tag names created in Step 3 — never `git push --tags`, which could push unrelated local tags).

## Step 6: Report

Report concisely (this is what gets relayed to the user):
- Feature selected and why (or why none was eligible).
- Tasks completed this run (task IDs + one-line descriptions), with commit SHAs and tag names.
- Final review verdict and whether it was pushed.
- If stopped early (`BLOCKED`/`STOP_FOR_HUMAN`/NO-GO/REJECTED): the exact reason, and what the next run (or a human) needs to do.

## Non-negotiable constraints

- Only `main` and its dispatched subagents touch this repo; `main` never writes implementation code itself — that's always a dispatched implementer subagent. `main`'s own tool use is limited to git plumbing, `tasks.md`/tag bookkeeping that `kiro-impl` already documents as parent-context actions, and reading spec/steering files.
- Never push to any branch other than `agent`. Never force-push, never `git reset --hard`, `git checkout -f`, or `git clean` — if the branch state is unexpected, stop and report instead of forcing past it.
- Never skip the per-task reviewer, `kiro-verify-completion`, or the Step 5 final reviewer. A push without an `APPROVED` verdict backing it is not allowed.
- Never batch multiple features into one run. One `main` dispatch advances at most one spec to its next natural stopping point, then reports. Continuous progress comes from the Routine re-triggering this skill, not from this skill looping internally across specs.
