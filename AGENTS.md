# AGENTS.md

Instructions for coding agents (and any other contributor) working in this repository.

## Terminology

Read the [wiki Glossary](https://github.com/solutionscay/skiff/wiki/Glossary) before
changing anything under `apps/desktop/src` that touches the sidebar, the session tree, or
keyboard navigation — `sidebar.ts`, `keyboard.ts`, `keys.ts`, `view.ts`, `layout.ts`,
`terminal.ts`. It defines the terms this codebase uses for its own concepts (rail,
sidebar/tree, pane, split, group, region, roving item, and the current keybinding scheme),
so new code and comments stay consistent with the existing ones instead of inventing
competing names for the same thing.

## Design

Read the [wiki Design page](https://github.com/solutionscay/skiff/wiki/Design) before
changing anything the user sees or presses: styles, menus, dialogs, keys, labels or
themes. Claude Code and Codex load the same rules as the `skiff-design` skill
(`.agents/skills/skiff-design`).

## README hero

The README hero is the website home hero: the same headline, tagline and media. The
media is the site's video poster, by its public URL, linked to the site. When you change
one hero, change the other in the same task. Check with `python3 scripts/hero-sync.py`
(add `--live` to read the live site).

## Keys

Before you add or change a keyboard shortcut, load the `skiff-keys` skill
(`.agents/skills/skiff-keys`). It lists where each key lives and what to keep in step.

## Testing

A full test run uses too much CPU and takes too long. Run it only at release time.

- During a task, run only the tests for the code you changed:
  - the tests of one crate, filtered to the changed module or test file:
    `cargo test -p CRATE TEST_NAME`;
  - `npx tsc --noEmit -p apps/desktop`;
  - the page tests for the changed pages:
    `cd apps/desktop && node --experimental-strip-types --test tests/NAME.test.ts`.
- Do not run `cargo test --workspace` during a task.
- Run the full suite only when the operator asks for a release. Run it one time, on the
  release commit: `cargo test --workspace` on v2, all page tests (`pnpm test` in
  `apps/desktop`) and the host-boundary check.
- Set `CARGO_BUILD_JOBS=8` for every cargo command.
- Use one worktree target for each task. Do not build a second target for v2 unless the
  change touches code that is only on v2.
- Do not add tests by default. Add one test only when it guards the exact bug or rule of
  the task.
- The host-boundary check (`python3 scripts/host-boundary/check.py`) runs on v2 merges, as
  before. It is fast.

## Development

See the README's [Development](README.md#development) section for build and run
instructions, and its [Layout](README.md#layout) section for what lives where.
