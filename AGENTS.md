# lt

Dear coding agent,

Be so kind and use **only ASCII** in generated text.
Do not use em-dashes.
Do not use unicode arrows, carots, etc.
Draw diagrams and tables in either markdown (tables), mermaid (diagrams),
or ascii (tables or diagrams).

Please use [beads-rust](./docs/agents/beads.md) for tracking issues.
Read the instructions file for a beads quickstart.
Use it liberally.
Use it in planning mode.
Use it to create TODOs while you are working on a change.
Use it as the _source of truth_ for the development roadmap.

Your Human counterparts use [Nix] for declarative development environments
_which includes_ sandboxing you, the coding agent, by restricting what files you
may read, write, and execute. Do not waste time trying to debug why a tool or
executable is not available to you. If you suspect your environment is
insufficient, ask a human for guidance (you may propose a fix if you have one,
eg. adding a package to the Nix developer shell).

When spawning sub-agents, give them their own `jj workspace` in:
.beads/workspaces/<bead-id>
Read the [jj quickstart](./docs/agents/jj.md) for more guidance.

Each workspace contains its own `.beads/` database. Sub-agents must:

- Run `br` from **inside their workspace**, never from the project root
- Close **only their assigned bead** - not beads belonging to other agents

After all workspaces are merged, use `scripts/resolve-beads-merge.py` to
resolve `.beads/issues.jsonl` conflicts. See [beads docs](./docs/agents/beads.md)
for the full merge integration workflow.

Please be advised that you already, as you read this, exist in a carefully
curated sandbox environment. If you find that a tool you need to do your job is
not available, first think about whether you truly need that tool - perhaps the
user has explicitly chosen to withold it from you - and failing that you may ask
the user to fix the situation (provide a [Nix]-specific recommendation if applicable).

[Nix]: ./docs/agents/nix.md
