---
name: sprint-plan
description: Sprint Plan - prime beads context then create an agent team to produce design documents for ready tasks in parallel
argument-hint: [model]
user-invocable: true
---

# Sprint Plan

Model to use for sub-agents: **$0** (default: opus if not specified)

Prime the beads context, then create an agent team to produce design/planning
documents for ready tasks in parallel.

**Ensure you have a clean working copy before starting.**

## Assemble the team

1. Run `bp` (beads prime) to load the current issue graph into context.

2. Parse the "Ready work" section to identify unblocked bead IDs.
   Do **not** gather additional context -- proceed immediately.

3. For each ready bead ID, create a jj workspace:

```
jj workspace add --name=<id> .campfire/workspaces/<id>
```

If a workspace already exists, prepend this to the teammate prompt:

    **Your workspace already exists!** A previous agent may have left it in an
    incomplete state. Run `bp <id>` and explore to find where they left off.
    Avoid redundant re-work. If the state looks broken, report back.

4. Spawn a teammate using model **$0** (or opus if unspecified) per bead:
   (set working directory to the isolated jj workspace)

   <prompt_template>

   You are a design agent working on bead `<id>`.

   Your working directory is already set to your isolated jj workspace. Do not leave it.

   **Step 1 -- Load the task:**

   ```
   br show <id> --no-auto-flush
   ```

   Read the output carefully. This is your complete specification.

   **Step 2 -- Produce a design document:**
   Write at least one markdown design document to a sensible path such as
   `docs/plans/<id>.md`. Create additional files only if the task requires it.

   **Step 3 -- Update beads and commit:**

   ```
   br update <id> --no-auto-flush --status=in_progress
   # ... do your work ...
   br close <id> --no-auto-flush --reason="<one sentence describing what was produced>"
   br sync --flush-only
   jj diff --summary   # .beads/issues.jsonl MUST appear as modified
   jj commit -m "design(<id>): <short description>"
   ```

   If `.beads/issues.jsonl` is not modified, stop and report back -- do not
   commit without it.

   **On permissions/sandbox errors:** debug what you can, write a `debug.md`
   summarising what you tried and your hypothesis, commit it with
   `jj commit -m "debug(<id>): ..."`, then stop. Regardless, always provide your
   diagnosis and debugging traces in your response in such cases.

   </prompt_template>

5. Run all sub-agents in parallel (single message, multiple Task tool calls).

6. After all agents complete, merge workspaces and reconcile beads:

```bash
jj log -r 'heads(all())'
jj new <rev1> <rev2> ...
python3 scripts/resolve-beads-merge.py
br sync --import-only
br ready --no-auto-flush
jj describe -m "merge: integrate <id1>, <id2>, ... designs"
```

7. Report a brief summary of outcomes per bead.
