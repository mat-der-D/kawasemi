# Recovering from a session/rate-limit failure during an Agent dispatch

This is a reference file, not part of the main procedure — `SKILL.md` links here so this
rare case doesn't bloat the primary read. Consult this file only when an `Agent` dispatch
(implementer, reviewer, debugger, or final reviewer) fails with an **infrastructure/API
error** rather than returning one of its normal structured outcomes (`READY_FOR_REVIEW`,
`BLOCKED`, `NEEDS_CONTEXT`, `APPROVED`, `REJECTED`). The characteristic symptom is a tool
result like:

> Agent terminated early due to an API error: You've hit your session limit · resets 11pm (UTC)

## What this actually means

This is a full stop of the whole session — not a failure specific to that one subagent
call, and not a task-level verdict at all. The runtime pauses and automatically resumes
processing later (potentially hours later, whenever the stated reset time arrives), with
no action required to make that resumption happen. By the time you are able to take any
further action in the session at all, real wall-clock time has already elapsed — often
enough for the limit to have already cleared, since the pause/resume is what consumed
that time in the first place. From inside the session this can *feel* continuous (there's
no direct perception of elapsed wall-clock time between turns), which makes it tempting to
conclude "the limit wasn't a real multi-hour thing, it must have been a fluke" — that
conclusion is usually wrong. The multi-hour estimate in the error message is typically
accurate; it just isn't something you need to actively manage around.

## What NOT to do

- **Do not classify this as `EXTERNAL_BLOCKER`** or route it through Step 5's escalation
  procedure. That classification is reserved for a genuinely un-retryable fact about the
  environment (a missing real credential, an unavailable third-party service, absent
  hardware), and is only reached *after* 3 rounds of real `REJECTED` remediation on a task
  that a subagent actually completed and a reviewer actually evaluated. An infra error
  during dispatch never produced a task verdict at all, so this escalation ladder doesn't
  apply.
- **Do not preemptively stash uncommitted in-progress work and write off the task for this
  run** just because one dispatch hit this error. That's very likely an overreaction: the
  correct recovery is almost always to simply retry the same dispatch once you're able to
  act again — the pause has already done its job by the time you get a turn.
- **Do not commit or push the crashed subagent's uncommitted changes directly.** That rule
  doesn't change: work only gets committed after it passes implementer → reviewer →
  `kiro-verify-completion`, regardless of why the previous attempt was interrupted.

## What to do

1. Leave (or briefly stash, if you need a clean tree to inspect something else first) the
   interrupted subagent's uncommitted changes — don't discard them outright. They may be
   substantially complete; the crash is an infrastructure event, not evidence the code
   itself is wrong.
2. Simply retry: dispatch a fresh subagent for the same task, following the normal
   Model/Effort/Execution-Mode rule.
3. If there is leftover uncommitted (or stashed) work from the interrupted attempt, tell
   the fresh subagent about it explicitly and instruct it to inspect and independently
   validate that work with full rigor before relying on it — the same standard as
   reviewing its own fresh draft, since the interrupted attempt never self-reviewed,
   validated, or reported a status. It's free to build on that work, fix parts of it, or
   discard it entirely and start clean; either is fine as long as the subagent stands
   fully behind whatever final diff it reports.
4. If the retry *also* fails on the same kind of infra error, that's still not a task
   verdict — retry again rather than escalating. Only treat repeated failures as a real
   blocker if you have concrete evidence it's not this same session-capacity pattern
   (e.g. a different, task-specific error message on retry).
