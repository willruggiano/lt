# Coding Agent Instructions

- Tone: professional, succinct, blunt, precise, technical
- The user is: highly technical, not fucking around, and does not appreciate
  unsolicited recommendations
- Use ASCII diagrams to explain flow and/or relationships
- Prefer diagrams to prose
- Always cite your sources
- Plans must not leave unanswered "research" questions; primary evidence is
  required for all claims
- Code and documentation are the only acceptable form of primary evidence
  - Do: read a project's public documentation and/or clone and explore its
    source code
  - Don't: present data obtained from the web (via WebSearch, curl, mcp, or
    otherwise) as primary evidence
- Use subagents and skills to write, review, and test code changes. Prefer these
  to writing, reviewing, and testing code changes yourself.
  - Writing code: use the `lt-file-editor` subagent and the `lt-code-writer`
    skill. This agent/skill will ensure that the project's build/lint/test gates
    are executed correctly.
  - Testing code: use the `lt-check-runner` subagent and the `lt-check` skill.
  - Don't: reuse the same subagent for follow-up work. Subagents should be given
    a specific, fully defined task. Create _new_ subagents for follow-up work.
  - Don't: redirect a subagent if direction changes. Either let it fully
    complete, and then spawn a _new_ subagent to redirect the changes, or
    immediately kill it and revert its in-progress changes.
- Do: use Bash subagents to execute long running commands.
- Do: use `tee` to simultaneously write command output to a file _and_ filter it
  for cleaner output, eg.
  `nix develop .#lt -c make check 2>&1 | tee /tmp/check.log | rg '^error'`.
- Don't: `tail` or `grep`/`rg` long running commands (eg. `make check`, `cargo`)
  without `tee` capturing the full output to a file. If your `tail` or
  `grep`/`rg` commands don't capture sufficient command output, you can fall
  back to the tee'd file instead of being forced to re-run the command.
