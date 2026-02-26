<!-- br-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads-rust](https://github.com/Dicklesworthstone/beads_rust) (`br`) for issue tracking.
Issues are stored in `.beads/` and tracked in vcs.

### Essential Commands

```bash
# View ready issues (unblocked, not deferred)
br ready

# List and search
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br search "keyword"   # Full-text search

# Create and update
br create --title="..." --description="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason="Completed"
br close <id1> <id2>  # Close multiple issues at once

# Sync with vcs
br sync --status      # Check sync status
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Workspace isolation

Every agent works in an isolated `jj workspace` under `.beads/workspaces/<bead-id>/`.
That workspace contains its own `.beads/` directory - a **private copy** of the issue
database. This is intentional: it lets multiple agents work in parallel without
contending on a shared database.

**Scope rules - what you may and may not touch:**

| Action                                                      | Allowed?                                    |
| ----------------------------------------------------------- | ------------------------------------------- |
| Close your assigned bead                                    | ✅ yes                                      |
| Mark your assigned bead `in_progress`                       | ✅ yes                                      |
| Create sub-beads for discovered follow-up work              | ✅ yes                                      |
| Close or update a bead assigned to another agent            | ❌ no - note it, don't touch it             |
| Run `br` against the project root instead of your workspace | ❌ no - always `cd` to your workspace first |

If your work incidentally covers another bead's scope, leave a comment on that bead
(`br comment <id> "..."`) but leave the status change to the merge step.

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (claim/in-progress: `br update <id> --claim`, close/complete: `br close <id>`)
- Create new issues with `br create` when you identify follow up work
- Use concise titles, set appropriate priority/type, verbose descriptions
- Never run `br` from the project root while working in an isolated workspace

<!-- end-br-agent-instructions -->
