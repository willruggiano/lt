---
name: sprint
description: Sprint - prime beads context then create an agent team to implement ready tasks in parallel
argument-hint: [model, guidance]
user-invocable: true
---

# Sprint

Create an agent team to implement all currently ready beads in parallel.
One teammate per ready bead. Use **$0** (default: sonnet if not specified) for
each teammate.

Guidance (may be empty): $1

**Ensure you have a clean working copy before starting.**

## Assemble the team

1.  Run `bp` (beads prime) to load the current issue graph into context.
    The output will look something like this:

    ```
    beads quickstart: /path/to/docs/agents/beads.md
    all beads docs: /nix/store/...-beads-docs
    (use your grep, ls, and/or read tools to explore the docs)

    Dependency graph: 2 issues in 2 component(s)

    Component 1 (1 issues, roots: bd-23y):
      bd-23y: investigate and design synchronization [P2] [open] (root)

    Component 2 (1 issues, roots: bd-6f3):
      bd-6f3: investigate and design authorization [P2] [open] (root)

    Ready work (2 issues with no blockers):

    1. [* P2] [design] bd-6f3: investigate and design authorization
    2. [* P2] [design] bd-23y: investigate and design synchronization


    $ jj log -n10
    @  mmmxyqst user@example.com 2026-02-25 18:15:48 b60b79c3
    |  (empty) (no description set)
    o  nlsmrurv user@example.com 2026-02-25 18:15:48 main 2793421e
    |  chore: add design beads for biscuit/jj integration
    ...


    $ jj status
    The working copy has no changes.
    Working copy  (@) : mmmxyqst b60b79c3 (empty) (no description set)
    Parent commit (@-): nlsmrurv 2793421e main | chore: add design beads for biscuit/jj integration
    ```

2.  The "Ready work" section identifies the available beads.
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

4.  For each ready bead, create a jj workspace:

    ```bash
    jj workspace add --name=<id> .beads/workspaces/<id>
    ```

    If a workspace already exists, prepend this to the teammate prompt:

        **Your workspace already exists!** A previous agent may have left it in an
        incomplete state. Continue from where they left off, avoiding redundant
        work. **If the state looks broken, stop immediately and report back.**

5.  Spawn **one teammate per bead**, using model **$0** (or "sonnet" if unspecified):
    (set working directory to the isolated jj workspace)

    <prompt_template>

    You are a coding agent working on bead `<id>`.
    Your isolated workspace is: /path/to/isolated/workspace
    **You may not modify files outside of your isolated workspace.**
    **Do not leave your workspace.**

    **Step 1 -- Prime context:**

    ```bash
    br show <id>
    ```

    Read the output carefully. Your bead is already claimed -- do NOT run
    `br update` or `br close`. The team lead manages all bead state.
    Read all referenced files in the bead description.

    **Step 2 -- Implement the task:**
    Write the code, tests, or other artifacts required to satisfy the bead.
    Stick to exactly what the bead asks for -- no extra features or refactoring.

    **Step 3 -- Commit and signal completion:**

    ```bash
    # Give your change a descriptive commit message:
    jj describe -m "<scope>(<id>): <short description>"
    ```

    Then return your response to the team lead. Include a one-sentence summary
    of what you produced and any caveats, open questions, and/or follow-up work
    that needs to be done.

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

8.  If there are code conflicts, resolve them manually:

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

10. Run linters, formatters, and any other checks. Fix issues directly in the
    merge commit (the working copy is still open).

11. Commit the merge:

    ```bash
    jj commit -m "merge(<id1>, <id2>, ...): <short summary>"
    ```

12. Report a brief summary of outcomes per bead.
