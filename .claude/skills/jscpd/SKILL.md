---
name: jscpd
description:
  Copy-paste detector for 220+ languages. Detect duplicated code and measure
  duplication percentages.
---

# jscpd

Copy-paste detector for programming source code, supports 220+ languages. Use
this skill to run jscpd and understand its output.

## Quick Start

```bash
npx jscpd --no-tips --no-color <path>
```

## AI Reporter Output Format (default via .jscpd.json)

The `ai` reporter produces compact, token-efficient output designed for agent
consumption:

```
Clones:
src/ foo.ts:10-25 ~ bar.ts:42-57
src/utils/helpers.ts:100-120 ~ src/utils/other.ts:5-25
---
3 clones · 4.2% duplication
```

Each line represents one clone pair:

- **Same file**: `path/file.ts 10-25 ~ 45-60` (shared path shown once)
- **Same directory**: `shared/prefix/ file-a.ts:10-25 ~ file-b.ts:42-57` (common
  prefix factored out)
- **Different paths**: `path/a.ts:10-25 ~ path/b.ts:42-57`

## Options

`npx jscpd --help` for the full options list

## Configuration File

The configuration file is: `.jscpd.json`
