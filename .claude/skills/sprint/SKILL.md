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

    📋 Ready work (2 issues with no blockers):

    1. [● P2] [design] bd-6f3: investigate and design authorization
    2. [● P2] [design] bd-23y: investigate and design synchronization


    $ jj log -n10
    @  mmmxyqst user@example.com 2026-02-25 18:15:48 b60b79c3
    │  (empty) (no description set)
    ○  nlsmrurv user@example.com 2026-02-25 18:15:48 main 2793421e
    │  chore: add design beads for biscuit/jj integration
    ○  suwsuxst user@example.com 2026-02-25 13:46:46 e96fc67a
    │  chore: nix cleanup
    ○  opqwvtuq user@example.com 2026-02-25 03:04:20 1856be7a
    │  refactor: move default models into ProviderConfig constructors
    ○  xkmmuryw user@example.com 2026-02-25 02:49:34 a7a7759b
    │  fix: wire real LLM providers and show all task states in TUI
    ○  wnuokvks user@example.com 2026-02-25 01:57:26 7060517f
    │  fix: a few bugs, typos
    ○  mlxoywrs user@example.com 2026-02-25 01:21:23 6dccc901
    │  chore(bd-tb8): put state in .campfire by default
    ○  snmkwvus user@example.com 2026-02-25 01:08:20 d3cf63b3
    │  feat(bd-2br): wire TUI to orchestration loop
    ○  wzxomonm user@example.com 2026-02-25 00:51:20 f4cb10e9
    │  chore: ignore
    ○        qlqknxqo user@example.com 2026-02-25 00:49:44 91f5ca12
    ├─┬─┬─╮  merge: integrate bd-18q, bd-27v, bd-y9k, bd-27n implementations


    $ jj status
    The working copy has no changes.
    Working copy  (@) : mmmxyqst b60b79c3 (empty) (no description set)
    Parent commit (@-): nlsmrurv 2793421e main | chore: add design beads for biscuit/jj integration
    ```

2.  The "Ready work" section identifies the available beads.
    Do **not** gather additional context -- proceed immediately.

3.  For each ready bead, create a jj workspace:

    ```bash
    jj workspace add --name=<id> .beads/workspaces/<id>
    ```

    If a workspace already exists, prepend this to the teammate prompt:

        **Your workspace already exists!** A previous agent may have left it in an
        incomplete state. Continue from where they left off, avoiding redundant
        work. **If the state looks broken, stop immediately and report back.**

4.  Spawn **one teammate per bead**, using model **$0** (or "sonnet" if unspecified):
    (set working directory to the isolated jj workspace)

    <prompt_template>

    You are a senior software engineer working on bead `<id>`.
    Your isolated workspace is: /path/to/isolated/workspace
    **You may not modify files outside of your isolated workspace.**

    **Step 1 -- Claim task and prime context:**

    ```bash
    br update <id> --claim
    br show <id>
    ```

    Read the output carefully.
    Read all references files in the bead description.

    **Step 2 -- Implement the task:**
    Write the code, tests, or other artifacts required to satisfy the bead.
    Stick to exactly what the bead asks for -- no extra features or refactoring.

    **Step 3 -- Signal completion:**

    ```bash
    # Close the bead:
    br close <id> --reason="..."

    # .beads/issues.jsonl MUST appear as modified:
    jj diff --summary

    # Give your change a descriptive commit message:
    jj describe -m "<scope>(<id>): <short description>"
    ```

    **On permissions/sandbox errors:** debug as best you can (read-only), and
    then report back to the team lead for guidance.

    ACCEPTANCE CRITERIA:
    1. Your bead MUST be closed.
    2. Your workspace MUST be ahead by only a single change

    </prompt_template>

5.  Run all sub-agents in parallel.

6.  After all agents complete, merge workspaces and reconcile beads:

    ```bash
    # List workspaces to identify change ids:
    jj workspace list

    # Create a "merge commit":
    jj new <rev1> <rev2> ...

    # Resolve beads conflicts:
    python3 scripts/resolve-beads-merge.py

    # Give the merge commit a description:
    jj commit -m "merge: integrate <id1>, <id2>, ... implementations"
    ```

7.  If there are any merge conflicts, resolve them:

    ```bash
    # List workspaces to identify change ids:
    jj workspace list

    # Create a merge commit:
    jj new <rev1> <rev2> ...

    # Resolve beads conflicts:
    python3 scripts/resolve-beads-merge.py
    ```

8.  Commit:

    ```bash
    # Give the merge commit a description (including all closed beads):
    jj commit -m "merge(<id1>, <id2>, ...): ..."
    ```

9.  Report a brief summary of outcomes per bead.
