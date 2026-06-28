# Contributing

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
