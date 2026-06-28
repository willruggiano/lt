# Contributing

## Strictness

- Maintain **maximum strictness** across the entire codebase
- Do not ignore lint errors using code comments without justification _and_ user
  approval
- When a bug or coding error could be prevented with a stricter configuration or
  an additional lint rule, add it
- Use [[jscpd/SKILL.md]] (`/jscpd`) for copy/paste detection

## Conventions

- **Code conventions**: path-scoped rules under `docs/rules/`. They auto-load
  when files matching their `paths:` enter context.
- **System design**: [[architecture.md]].
- **Engineering posture and working principles**: [[posture.md]].

## Commits

- Conventional commit style: `<type>(<scope>): <subject>`.
- Commits that close a Linear issue end with a `Closes: ENG-XXX` trailer; one
  trailer per issue closed.
- For partial progress on an issue (no close), use `Refs: ENG-XXX` instead. Use
  one trailer per issue referenced.
