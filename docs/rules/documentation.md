---
paths:
  - "**/*.md"
---

# Documentation conventions

- Internal references between documents use wiki-link style and always include
  the file extension: `[[architecture.md]]`,
  `[[posture.md#1. Think Before Coding]]`. Use `[[page.md|display text]]` only
  when the file name is not the desired display.
- External URLs use standard markdown link syntax:
  `[label](https://example.com)`.
- Fenced code blocks always declare a language identifier (`rust`, `sql`, `sh`,
  `text`, ...).
- One H1 per document at the top. Section headings start at H2 and do not skip
  levels.
- Never duplicate content. If another doc already says it, link to it. Sources
  of truth:
  - [[architecture.md]] for system design.
  - [[posture.md]] for engineering posture and working principles.
  - [[contributing.md]] for setup, conventions, and tracking.
  - `docs/rules/` for code rules.
- One topic per document. Be concise and precise; cut sentences that do not add
  information.
- No breadcrumbs or tombstones. Don't annotate code or prose with pointers to
  where something now lives or that it is generated elsewhere (e.g. "generated
  in `x_gen.rs`", "bindings live in …", "moved to …"). File layout and the
  `// Code generated … DO NOT EDIT.` header already convey this; such notes
  carry no information and go stale. When you move or delete code, delete its
  comment too. (A wiki-link to a doc that explains _why_ is different, and is
  encouraged above.)

Where mechanically checkable, these conventions are enforced by the
`markdownlint-cli2` pre-commit hook (see [[nix.md]]), plus a custom rule for the
wiki-link form. "Never duplicate content" and "one topic per document" remain
review matters.
