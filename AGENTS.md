# Repository agent instructions

## Start here

Read [`docs/PROJECT_CONTEXT.md`](docs/PROJECT_CONTEXT.md) before planning or
changing the project. It records the shipped baseline, architecture map,
safety invariants, deliberate product boundaries, and the continuation
workflow for cloud agents.

For work that changes capability claims, privilege boundaries, packaging,
releases, or UI behavior, also read the branch-specific document named in the
context file before editing. Completion means the implementation, capability
matrix, project context, and verification evidence agree.

Rufust ships on Linux and Windows. Platform code lives in `windows` and Linux
modules beside shared code; read [`docs/WINDOWS.md`](docs/WINDOWS.md) before
changing Windows behavior, and keep both builds passing (`cargo clippy
--workspace --all-targets -- -D warnings` on each). A change to shared code is
not done until it works on both.

## UI work

Use the `frontend-design` skill and a bounded UI review subagent when those
capabilities are available. Preserve the compact Device Workbench direction,
original Rufus icon assets, keyboard access, true modal blocking, and verified
light/dark layouts. Visually test the smallest supported window and a maximized
window before declaring UI work complete, on Linux and on Windows when the
change can render differently there.

## Commit attribution

When Codex materially changes the repository, preserve the official GitHub
identity as the commit author when the execution path supports it:

`Codex <267193182+codex@users.noreply.github.com>`

If another account must author the commit, append this trailer instead:

`Co-authored-by: Codex <267193182+codex@users.noreply.github.com>`

When Grok materially changes the repository, preserve xAI's documented git
identity as the commit author when the execution path supports it:

`Grok <grok-export@users.noreply.github.com>`

If another account must author the commit, append this trailer instead:

`Co-authored-by: Grok <grok-export@users.noreply.github.com>`

Omit attribution for read-only reviews and commits that contain only
user-authored changes.
