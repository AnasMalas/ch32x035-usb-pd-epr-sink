# Agent instructions

These rules apply to every coding agent working in this repository (Codex,
Claude Code, and others). `CLAUDE.md` only imports this file, so put new rules
here.

[CONTRIBUTING.md](CONTRIBUTING.md) is the governing document for layer
ownership, the repository check, evidence standards, and dependency updates.
Read it before you change code. This file adds only what agents working in
parallel need.

## At the start of every task

1. Read the `Unreleased` section of [CHANGELOG.md](CHANGELOG.md). Then run
   `git log --oneline -20` and `git status`, because other agents may have
   landed changes since your context was captured.
2. The code is authoritative over any summary, including this file. Re-read a
   file before you cite line numbers from it.
3. Work on your own branch or worktree based on `main`. Never revert,
   reformat, or clean up changes you did not make.

## Rules

- Keep capabilities. Do not fix a problem by removing PPS, EPR, or AVS
  support, suppressing a reset the standard requires, or special-casing one
  source model.
- Downstream firmware pins this repository to exact commits. Never rewrite
  (amend, rebase, or force-push) or delete a branch whose commits may be
  pinned downstream. Doing so breaks that firmware's build once the commits
  are garbage-collected.
- Flash size is usually what limits downstream firmware first. For any change
  under `crates/` or `vendor/`, build the `usb-epr`,
  `usb-epr-deep-black-box`, and `usb-epr-text` profiles before and after the
  change with the firmware example's `build.ps1` and `size.ps1`. Record the
  size difference in your changelog entry.
- Label each claim as hardware-confirmed, host-tested, or inferred. A host
  test does not prove CH32 timing, register behavior, or load cutoff.
- This repository is public. Keep private product details, personal paths,
  serial numbers, and unpublished captures out of commits.
- Do not flash hardware or open serial ports unless the user asks for it in
  the current session.

## Before you finish

- Run `.\scripts\check.ps1 -RequireNode`. If you cannot run part of it, say
  which part and why.
- In the same commit as the change, add a bullet under `## Unreleased` in
  [CHANGELOG.md](CHANGELOG.md), following the rules in that file.
- When behavior history remains relevant, move it into the architecture,
  interoperability, or `vendor/*/UPSTREAM.md` documents, as
  [docs/README.md](docs/README.md) requires. Do not add handoff or
  session-note files.
