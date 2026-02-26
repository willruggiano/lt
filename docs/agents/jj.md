# jj (Jujutsu) - Agent Instructions

This project uses **jj** (Jujutsu) for version control, not git.
Use `jj [command] --help` for up-to-date usage help.

## Core Concepts

- **Working copy is always a commit.** Every edit you make is automatically part of the current change. There is no staging area.
- **Change ID vs Commit ID.** Every commit has a stable `change ID` (e.g. `vyukskqr`) that survives rebases/amends, and a `commit ID` (the SHA). Always prefer change IDs in commands.
- **`@`** refers to the current working-copy commit.
- Commits are mutable by default until explicitly made immutable (e.g. pushed to a remote). You can freely rewrite history.

## Essential Commands

| Task                                | Command                                                                   |
| ----------------------------------- | ------------------------------------------------------------------------- |
| Check status                        | `jj status` (or `jj st`)                                                  |
| View history                        | `jj log`                                                                  |
| Describe current change             | `jj describe -m "message"`                                                |
| Create a new change on top          | `jj new`                                                                  |
| Commit current change and start new | `jj commit -m "message"` (equivalent to `jj describe -m "..." && jj new`) |
| Diff working copy                   | `jj diff`                                                                 |
| Show a specific change              | `jj show <change_id>`                                                     |
| Undo last operation                 | `jj undo`                                                                 |
| Squash into parent                  | `jj squash`                                                               |
| Rebase onto a target                | `jj rebase -d <target>`                                                   |

## Typical Workflow

```bash
# Edit files (changes are automatically tracked)
jj describe -m "feat: add login endpoint"  # describe the current change
jj new                                     # start a new change for next task
```

Or using `commit` (describe + new in one step):

```bash
jj commit -m "feat: add login endpoint"
```

## Workspaces

```bash
# Create a workspace for parallel agents (use bead ids)
jj workspace add --name=bd-xxx --message="..." .beads/workspaces/bd-xxx
```

## Key Differences from Git

- **No `git add`** - all tracked file changes are included automatically.
- **No staging area** - the working copy IS the commit.
- **`jj undo`** is your safety net - it undoes any operation, including rebases or squashes.
- **Conflicts are stored in commits** - jj never stops a rebase due to conflicts; conflicts are recorded and can be resolved later with `jj resolve`.
- **Bookmarks instead of branches** - jj uses bookmarks instead of branches. Use `jj bookmark` to manage them.

## Revset Syntax

jj uses a powerful revset language to select commits:

- `@` - working copy
- `@-` - parent of working copy
- `all()` - all commits
- `trunk()` - the main branch tip
- `ancestors(@)` - all ancestors of `@`
- `-r <revset>` - many commands accept `-r` to target a specific revision

See [jj.appendix.md](./jj.appendix.md) for more detail on revsets and advanced workflows.
