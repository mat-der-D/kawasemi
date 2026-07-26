---
name: kiro-auto-implement
description: Use when unattended, no-human-in-the-loop automation (a Claude Code Routine, cron trigger, or explicit request to "keep advancing the implementation") should pick the next actionable spec/task from the roadmap dependency graph and current tasks.md progress, implement and review it through dispatched sub-agents, and push AI-reviewed work directly to the `agent` branch.
disable-model-invocation: false 
allowed-tools: Read, Write, Edit, MultiEdit, Bash, Glob, Grep, Agent
argument-hint: [feature-name]
---

# kiro-auto-implement

## Overview

This skill turns one trigger (typically a Routine firing on a schedule) into one bounded unit of unattended forward progress on the kawasemi implementation: pick the next eligible spec, drive `kiro-impl` through exactly one top-level task group of it, run a final AI review, and push straight to `agent`. There is no human review step — an AI reviewer's `APPROVED` verdict is the only gate before push. A single group, not the whole feature, keeps each run within budget; the Routine re-firing is what accumulates progress across groups over time.

**REQUIRED SUB-SKILLS:** `kiro-impl` (task execution engine this wraps), `kiro-review` (per-task and final review protocol), `kiro-verify-completion` (evidence gate), `kiro-validate-impl` (feature-level GO/NO-GO), `kiro-debug` (blocked-task investigation).

## You are `main` — execute this yourself, do not dispatch a subagent to run it

**Whoever's context this file is read into (the Routine-triggered session, or whatever session was told to run `/kiro-auto-implement`) executes Steps 1 through 6 directly, in this same context.** There is no separate orchestrator subagent and no `Step 0` dispatch layer. Read this file in full, then proceed straight to Step 1 yourself.

This is a deliberate change from an earlier version of this skill that dispatched a fresh `Agent` to act as `"main"`. That indirection caused a real, observed failure mode: completion notifications for agents dispatched _by_ a subagent do not reliably reach that subagent — they surface at the top-level/root session instead, regardless of the `run_in_background` value used for the nested dispatch. A subagent playing `main` therefore had no first-hand way to receive its own children's results and had to depend on the root session relaying them as plain messages, which the subagent then had no tool-layer way to verify — a trust problem with no clean resolution. Removing the extra layer removes the problem at the root: whichever session executes Steps 1-6 is, by construction, the same session that dispatches every implementer/reviewer/debugger/final-reviewer subagent below, so their completion notifications land exactly where the logic that needs them is running.

You (running Steps 1-6) still dispatch fresh subagents for implementer/reviewer/debugger/final-reviewer work — that discipline is unchanged. You just don't dispatch a copy of yourself first.

## MODEL, EFFORT & EXECUTION-MODE RULE (applies to every Agent dispatch below, no exceptions)

Every `Agent` tool call you make while executing this procedure — implementer, reviewer, debugger, final reviewer, and any `kiro-validate-impl` validation-dimension dispatch — MUST:

- pass `model: "sonnet"` (the current latest Sonnet model)
- prepend this line to the dispatched prompt verbatim: `Reasoning effort: medium. Think carefully through edge cases, verify assumptions against the actual repository state, and do not shortcut analysis before acting.`
- pass `run_in_background: false` (foreground/blocking dispatch) — **never omit this and never rely on the default.**

`run_in_background: false` makes the dispatching call block until the child finishes and return the child's result as this call's own tool result, in-context, with nothing to relay and nothing to distrust. Every dispatch below needs its child's result before it can decide what to do next, so there is no tradeoff being made by always blocking — only a bug (see above) being avoided.

If dispatching several independent subagents concurrently (e.g. `kiro-validate-impl`'s parallel validation dimensions), issue all of their `Agent` calls together in a single response (multiple tool-use blocks in one turn), each with `run_in_background: false`. They still execute concurrently; the difference is that you directly receive every one of their results before continuing, instead of depending on background completion notifications that (per above) may not reach you reliably at deeper nesting.

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

**Push-blocked backlog takes priority over new selection.** Before anything else, grep every `.kiro/specs/*/tasks.md` for a `_Push-Blocked:_` group-level annotation (see Step 5's escalation procedure for how this gets written). If any spec has one, that spec/group is this run's mandatory focus — go straight to Step 5's escalation procedure for it (fresh root-cause investigation, since the previous run's classification and attempts are already recorded there) instead of selecting new work below. Only proceed to feature selection below if no `_Push-Blocked:_` marker exists anywhere.

If `$ARGUMENTS` names a feature explicitly, use it — but still run the eligibility rule in point 3 below before proceeding. If the named feature fails eligibility (already complete, or a dependency is incomplete), STOP and report why rather than silently falling back to auto-selection. Otherwise (no argument given):

1. Read `.kiro/steering/roadmap.md`, section "Specs (dependency order)" — this is the topologically-sorted Phase 1 list, each entry with a `Dependencies:` line.
2. For each spec in that order, compute completeness from `.kiro/specs/<name>/tasks.md`: complete iff there are zero remaining `- [ ] <n>.<m>` actionable lines (a spec with unresolved `_Blocked:_` tasks still pending is NOT complete).
3. A spec is **eligible** iff: it is not yet complete, `.kiro/specs/<name>/spec.json` has `"ready_for_implementation": true`, and every spec named in its `Dependencies:` is complete per step 2.
4. Pick the **first** eligible spec in roadmap order. That is the feature for this run.
5. If no Phase 1 spec is eligible, check "Future Phases" entries that already have a `.kiro/specs/<name>/` directory (meaning a spec was created since roadmap.md was written) and repeat 2-4 over those.
6. If nothing is eligible anywhere (all complete, or the next one has no spec directory yet — spec creation is out of scope for this skill), STOP: report that implementation is caught up and, if applicable, which spec needs `/kiro-spec-init` next. Skip to Step 6.

## Step 3: Select one task group, then drive implementation via `kiro-impl`

A whole feature (11-26 subtasks across specs seen so far) is too large a unit for one run's budget. This step bounds the run to a single top-level task group instead.

1. Read `.kiro/specs/<feature>/tasks.md`. Task IDs are `N` (top-level group heading, not directly executable) and `N.M` (actionable subtask). Find the **first** top-level group `N` that has at least one subtask `N.M` which is not yet `[x]` and has no `_Blocked:_` annotation. Call this `GROUP`. This run works on `GROUP` only — never subtasks from any other group, even if individually eligible (e.g. a `(P)` task in a later group).
2. Invoke the `kiro-impl` skill for the chosen feature in **autonomous mode** (fresh-subagent-per-task dispatch discipline — do not switch to manual/main-context mode), with these modifications to its documented procedure:
   - Restrict the task queue built in `kiro-impl`'s "Build task queue" step to subtasks whose ID starts with `GROUP.` (e.g. `2.1`, `2.2`, ...). Ignore other groups' tasks entirely for this run, even ones with satisfied dependencies.
   - Skip `kiro-impl`'s own Step 4 ("Final Validation") entirely. Whether `kiro-validate-impl` runs at all is decided by this skill's Step 4 below, based on whether the _whole feature_ — not just `GROUP` — is complete after this run.

Do not alter any other part of `kiro-impl`'s behavior: implementer and reviewer are still fresh subagents dispatched per task (apply the Model, Effort & Execution-Mode Rule above to those dispatches too), `kiro-review` and `kiro-verify-completion` still gate every task, and `kiro-debug` still handles `BLOCKED`/rejection-round-3 per its existing bounded-retry rules. Within this restricted queue, `kiro-impl` runs until its own natural stop: every subtask in `GROUP` is complete or `BLOCKED`, or `STOP_FOR_HUMAN` is raised.

## Step 4: Handle the stop condition

- **`GROUP` finished (all its subtasks complete or `BLOCKED`), but the feature has other groups with remaining subtasks**: this is the expected, normal stopping point for this run. Do not run `kiro-validate-impl` (the feature isn't complete yet). Proceed to Step 5 with whatever is committed so far.
- **`GROUP` finished and it was the feature's last group with remaining subtasks (the whole feature is now complete)**: now run `/kiro-validate-impl {feature}` as the GO/NO-GO gate that `kiro-impl`'s own Step 4 would normally have run.
  - **GO**: proceed to Step 5.
  - **NO-GO after capped remediation (max 3 rounds, per `kiro-impl`'s rule)**: this is not a failure of the run — proceed to Step 5 with whatever is committed so far. Do not attempt further remediation yourself beyond that cap (that would be you doing concrete work instead of delegating).
- **A subtask in `GROUP` is `BLOCKED` and no further subtasks in `GROUP` are actionable**: this is not a failure of the run — proceed to Step 5 with whatever is committed so far.
- **`STOP_FOR_HUMAN`**: the task plan itself needs human attention. Do not try to fix `tasks.md` yourself. Proceed to Step 5 with whatever is committed so far, and flag this clearly in the Step 6 report — "no human review" for code correctness does not extend to overriding a broken task decomposition.
- **Nothing was committed this run** (`git rev-parse HEAD` == `RUN_START_SHA`): skip Step 5 (nothing to push), go to Step 6.

## Step 5: Final review gate, then push

1. Diff the whole run: `git diff RUN_START_SHA...HEAD` (and `git log RUN_START_SHA..HEAD` for the commit list).
2. Dispatch one **final reviewer** subagent (fresh, per the Model, Effort & Execution-Mode Rule) with: the full run diff, the list of tasks/commits produced, the feature's `requirements.md`/`design.md` paths, and the repo's build/test commands (discover the same way `kiro-impl`'s preflight does: `Cargo.toml` → `cargo check` / `cargo test`, etc.). Ask it to apply the `kiro-review` protocol at run scope — read the code, run the build/test commands itself, do not trust prior reports — and return a structured `## Review Verdict` / `- VERDICT: APPROVED|REJECTED` block. This is the AI review agent gate; there is no human approval step.
3. **REJECTED**: do NOT push yet, and do NOT stop the run. A REJECTED run-level verdict is a remediation trigger, not a stopping condition: dispatch a fresh remediation implementer subagent (per the Model, Effort & Execution-Mode Rule) with the reviewer's exact findings, addressing precisely the cross-task/integration defect(s) raised — do not expand scope beyond what the reviewer flagged. After the implementer reports `READY_FOR_REVIEW`, dispatch a fresh task-local reviewer (`kiro-review` protocol) against just that remediation diff; on `APPROVED`, apply `kiro-verify-completion`, commit the fix (selective staging, same conventions as every other task commit), then re-dispatch a fresh run-level final reviewer against the full updated run diff (`RUN_START_SHA...HEAD`). Repeat this remediate → re-review → re-verify → commit → re-run-level-review cycle until the run-level verdict is `APPROVED`.
   - **Bounded remediation**: cap this cycle at 3 rounds, mirroring `kiro-impl`'s own bounded-remediation rule for feature-level NO-GO. If any remediation round's task-local reviewer itself returns `REJECTED`, that follows `kiro-impl`'s normal per-task rules (re-dispatch implementer with feedback, debug subagent after 2 rounds, etc.) before counting as one completed round of this outer cycle.
   - **If still `REJECTED` after 3 rounds, this is an abnormal condition, not a normal stopping point — do not just stop and hope a future run notices.** Repeated failure to close a well-scoped, reviewer-identified gap after 3 focused attempts means the remaining blocker is unlikely to be a plain implementation mistake; run the escalation procedure below before falling back to stop-and-report.

### Escalation when the remediation cap is exhausted

The purpose of this whole skill is to eliminate human intervention, not to relocate the stopping point — "a human needs to decide" is therefore not an acceptable terminal classification on its own. Every escalation path below must end in either a mechanically-produced resolution that gets tried, or a narrowly-defined external blocker that is a fact about the environment (a missing real credential, an unavailable third-party service, absent hardware) rather than an opinion about the right design. Judgment calls get resolved by a panel-and-default mechanism, not deferred to a person.

1. **Root-cause classification**: dispatch one fresh investigator subagent (apply the `kiro-debug` protocol at run scope, per the Model, Effort & Execution-Mode Rule) with the full history of all 3 rounds — each round's reviewer findings, the fix attempted in response, and why the next round's review still rejected it — plus the feature's `requirements.md`/`design.md`. Ask it to classify the *reason remediation isn't converging*, not just the surface bug, as exactly one of:
   - `IMPLEMENTATION_DEFECT` — a mechanical coding bug that 3 rounds happened not to land on, but a differently-scoped fix attempt could plausibly still resolve (e.g. each round patched a symptom instead of the shared root cause). Require a concrete, specific `FIX_PLAN` from this investigator — not "try again."
   - `SPEC_INCONSISTENCY` — `requirements.md`/`design.md` is internally contradictory, or contradicts an already-shipped upstream contract, such that no implementation can satisfy both the spec as written and the reviewer's (correct) reading of it. This is `kiro-impl`'s existing "Spec Conflicts with Reality" / "Upstream Ownership Detected" situation, just discovered at run-scope instead of task-scope.
   - `AMBIGUOUS_DESIGN_CHOICE` — the spec genuinely underdetermines the answer and multiple resolutions are each individually defensible (the same kind of gap every task in this run already resolved by picking one documented, conservative interpretation — e.g. 5.1's poll-creation deferral, 3.1's unauthenticated-`unlisted` reading — except this time 3 rounds of independent implementers/reviewers didn't converge on the same interpretation as each other).
   - `EXTERNAL_BLOCKER` — resolving it needs something that doesn't exist inside this repo/sandbox at all: a real secret/credential, network access to a third-party service, physical hardware, or similar. This is a fact about the environment, not a judgment call, and is the only category below that can end in a stop.
2. **If `SPEC_INCONSISTENCY`**: dispatch one fresh subagent (per the Model, Effort & Execution-Mode Rule — do not edit spec files yourself even though they aren't implementation code, for the same independence reasons task work is always delegated) to correct the specific contradiction in `requirements.md`/`design.md`, with the rationale written into the spec file itself and mirrored as a `tasks.md` Implementation Notes entry (same documentation convention as every other deliberate-deviation note in this spec). Keep the edit as narrowly scoped as the investigator's finding — not license for a broader spec rewrite. Then run exactly **one more** remediate → re-review → re-verify → commit → re-run-level-review cycle against the corrected spec.
3. **If `AMBIGUOUS_DESIGN_CHOICE`**: resolve it with a small judge panel instead of stopping. Dispatch 3 independent subagents in parallel (single response, multiple tool-use blocks, each `run_in_background: false`), each proposing and justifying one resolution from a different lens: (a) the most spec-literal reading, (b) the reading most consistent with conventions already shipped elsewhere in this codebase, (c) the most conservative/narrowest-scope option (least behavioral surface, easiest to revise later). Then apply a deterministic tie-break, no human vote needed: if 2 or more of the 3 converge on the same resolution, take it; otherwise default to option (c), the conservative one — this mirrors how every ambiguity so far in this codebase was already resolved when a single implementer hit one. Document the chosen resolution and which lenses it came from in `requirements.md`/`design.md`'s Implementation Notes equivalent (or `tasks.md`'s Implementation Notes if no better home exists), then run exactly **one more** remediate → re-review → re-verify → commit → re-run-level-review cycle implementing it.
4. **If `EXTERNAL_BLOCKER`**: this is the one legitimate stop. Do NOT push. Write the cross-run handoff marker (below) stating plainly what external prerequisite is missing, then report it in Step 6 as a concrete, actionable ask (not a vague "needs review").
5. **If still `REJECTED`** after the one extra cycle (`SPEC_INCONSISTENCY` or `AMBIGUOUS_DESIGN_CHOICE` path) or after the `IMPLEMENTATION_DEFECT` fix attempt — do not simply give up: re-run this escalation procedure from step 1 with a fresh investigator (do not reuse the previous classification uncritically; a wrong classification is itself a plausible reason the first attempt didn't converge). Cap this at 2 escalation cycles total. Only after 2 full escalation cycles still end in `REJECTED` (and it is not an `EXTERNAL_BLOCKER`), stop, write the cross-run handoff marker, and report — framed as "the next run should retry with a different angle," never as "waiting for a human to approve."

**Cross-run handoff marker**: whenever a run stops without reaching run-level `APPROVED`, append a line directly beneath the affected `GROUP`'s top-level heading in `tasks.md` (not a subtask line):
```
_Push-Blocked: <one-line reason> — see Implementation Notes (<date or commit ref>). Root cause: IMPLEMENTATION_DEFECT|SPEC_INCONSISTENCY|AMBIGUOUS_DESIGN_CHOICE|EXTERNAL_BLOCKER._
```
and add a fuller matching entry to the `## Implementation Notes` section (classification history across every escalation cycle tried, what was attempted, why it didn't converge, and — for `EXTERNAL_BLOCKER` only — exactly what external prerequisite is missing). This marker is the actual information handoff to the future — a `tasks.md` full of `[x]` checkboxes with no marker is indistinguishable from a fully-clean, pushable state to any future run's Step 2, so writing it is not optional. Step 2's push-blocked-backlog check makes the *next* run pick this up automatically and retry with fresh context before starting any new work — this is how convergence happens without a human in the loop, not a substitute for it. Once a future run resolves the block (reaches run-level `APPROVED` and pushes), remove the `_Push-Blocked:_` line as part of that run's own commit.
6. **APPROVED** (on the first pass or after any remediation/escalation cycle): push everything from this run: `git push origin agent`.

## Step 6: Report

Report concisely (this is what gets relayed to the user):

- Feature selected and why (or why none was eligible), and which group (`GROUP`) this run worked on.
- Tasks completed this run (task IDs + one-line descriptions), with commit SHAs.
- Whether the feature is now fully complete or groups remain for a future run.
- Final review verdict and whether it was pushed.
- If stopped early (`BLOCKED`/`STOP_FOR_HUMAN`/NO-GO/REJECTED): the exact reason, and what the next run (or a human) needs to do.

## Non-negotiable constraints

- Only you and your dispatched subagents touch this repo; you never write implementation code yourself — that's always a dispatched implementer subagent. Your own tool use is limited to git plumbing, `tasks.md` bookkeeping that `kiro-impl` already documents as parent-context actions, and reading spec/steering files.
- Never push to any branch other than `agent`. Never force-push, never `git reset --hard`, `git checkout -f`, or `git clean`.
- If the branch state is unexpected (HEAD moved, or a commit's contents differ, without an action you took), do not just force past it — but do not assume tampering either. Verify independently before deciding:
  1. `git reflog` — what operation actually produced this state (amend, reset, a foreign commit, etc.)?
  2. If it looks like a rewrite of a commit you or your subagents made (not a wholly foreign commit), diff the old and new tree (`git diff <old-sha> <new-sha>`) — is the tree byte-identical?
  3. If the reflog shows a plausible, nameable, metadata-only operation (e.g. an amend that only touched commit signing) **and** the tree diff is empty, treat it as benign, proceed, and record exactly what you found (the reflog entry and the empty tree-diff) in the Step 6 report as evidence. Do not silently ignore it — surface it, just don't let it block the run.
  4. If the tree differs, or the reflog shows something you cannot explain, or you cannot complete both checks, STOP and report — do not force past it.
  - Do not use local commit-signature status as evidence either way: this environment's SSH commit-signing helper only implements the `sign` operation, not `verify`, so `git log --show-signature` / `%G?` reports `N` (no signature) for every commit regardless of whether it is validly signed. An "unsigned"-looking commit is not by itself evidence of tampering — judge git-metadata-only changes by reflog + tree-diff, never by local signature verification.
- Never skip the per-task reviewer, `kiro-verify-completion`, or the Step 5 final reviewer. A push without an `APPROVED` verdict backing it is not allowed.
- **The push gate is absolute and is never itself the thing being escalated.** Step 5's escalation procedure (root-cause classification, spec fixes, judge panels, bounded extra remediation cycles) exists to automate *reaching* `APPROVED` without a human — it must never be read as, and must never become, a path to pushing an unresolved or `REJECTED` state. No number of exhausted rounds, no `EXTERNAL_BLOCKER`, no elapsed time, and no pressure to keep the feature moving justifies pushing anything short of a genuine run-level `APPROVED`. When resolution isn't reached, the correct and only outcome is: work stays committed locally on `agent` (unpushed), the `_Push-Blocked:_` marker is written, and the run reports — never "push it anyway and let a later pass clean it up." A stuck local branch blocks only this one group's push; a pushed dead end pollutes the shared branch everything else builds on, which is a strictly worse failure mode this rule exists to prevent.
- Never batch multiple features into one run, and never batch multiple task groups into one run. One run advances at most one top-level task group (`N.1`..`N.M`) of one spec, then reports — even if further groups or specs are immediately eligible afterward. Continuous progress comes from the Routine re-triggering this skill, not from this skill looping internally across groups, specs, or runs.
- Never dispatch any `Agent` call without `run_in_background: false` (see the Model, Effort & Execution-Mode Rule above).
