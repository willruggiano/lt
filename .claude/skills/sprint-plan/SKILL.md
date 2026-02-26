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

1.  Run `br ready --type=design` to load the current design tasks into context.

2.  Parse the "Ready work" section to identify unblocked bead IDs.
    Do **not** gather additional context -- proceed immediately.

3.  Claim all ready beads in the default workspace (before creating any
    isolated workspaces):

    ```bash
    br update <id1> --claim
    br update <id2> --claim
    # ... one per ready bead
    ```

    This records the in-progress state in the default workspace's working copy.
    Sub-agents will inherit this state and must NOT run `br update` or `br close`
    themselves -- the team lead owns all bead mutations.

4.  For each ready bead ID, create a jj workspace:

    ```bash
    jj workspace add --name=<id> .beads/workspaces/<id>
    ```

    If a workspace already exists, prepend this to the teammate prompt:

        **Your workspace already exists!** A previous agent may have left it in an
        incomplete state. Run `br show <id>` and explore to find where they left off.
        Avoid redundant re-work. If the state looks broken, report back.

5.  Spawn a teammate using model **$0** (or opus if unspecified) per bead:
    (set working directory to the isolated jj workspace)

    <prompt_template>

    You are a design agent working on bead `<id>`.
    Your isolated workspace is: /path/to/isolated/workspace
    **You may not modify files outside of your isolated workspace.**
    **Do not leave your workspace.**

    **Step 1 -- Load the task:**

    ```bash
    br show <id>
    ```

    Read the output carefully. This is your complete specification.
    Your bead is already claimed -- do NOT run `br update` or `br close`.
    The team lead manages all bead state.

    **Step 2 -- Produce a design document:**
    Write at least one markdown design document to a sensible path such as
    `docs/plans/<id>.md`. Create additional files only if the task requires it.

    **Step 3 -- Commit and signal completion:**

    ```bash
    # Give your change a descriptive commit message:
    jj commit -m "design(<id>): <short description>"
    ```

    Then return your response to the team lead. Include a one-sentence summary
    of what you produced and any caveats and/or open questions.

    **On permissions/sandbox errors:** debug (quickly) as best you can, write a
    `debug.md` summarising what you tried and your hypothesis, then stop.
    Regardless, always provide your diagnosis and debugging traces in your
    response to the team lead.

    ACCEPTANCE CRITERIA:
    1. Your workspace MUST be ahead by exactly one change relative to its
       starting point.
    2. Do NOT touch .beads/issues.jsonl -- leave bead state to the team lead.

    </prompt_template>

6.  Run all sub-agents in parallel.

7.  After all agents complete, merge their workspaces into the default workspace:

    ```bash
    # List the heads, including up to two commits for each head.
    jj log -r 'ancestors(heads(all()), 2)'

    # Create a merge commit from @ (team lead) and all agent tips.
    # NOTE: @ must be included so the claimed-bead state is a parent.
    jj new @ <agent-rev1> <agent-rev2> ...
    ```

    Because sub-agents never touched .beads/issues.jsonl, there are no bead
    conflicts to resolve.

8.  If there are conflicts, resolve them manually:

    **Do NOT use `jj resolve` -- it requires an interactive TUI and will be
    blocked.** Instead, read jj's output carefully. jj will print instructions
    for how to proceed (typically: `jj new <rev>` to check out the conflicted
    commit, edit the conflicted files directly to remove conflict markers, then
    `jj squash` to fold the resolution back). Follow those instructions exactly.

9.  Close beads for all agents that completed successfully:

    ```bash
    br close <id1> <id2> ... --reason="..."
    ```

    Do NOT close beads for agents that reported failure or incomplete work.
    Create follow-up beads as needed.

10. Commit the merge:

    ```bash
    jj commit -m "merge(<id1>, <id2>, ...): integrate designs"
    ```

11. Report a brief summary of outcomes per bead.
