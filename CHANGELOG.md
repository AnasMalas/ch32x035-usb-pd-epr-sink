# Changelog

This file records notable changes to the project. No version has been
released yet, so every entry is under Unreleased. Changes made before
2026-09-27 are recorded only in the Git history.

Rules for entries:

- Add one bullet per change, newest first, in the same commit as the change.
- Start each bullet with the commit type used in this repository's history
  (`fix:`, `feat:`, `diag:`, `docs:`, and so on). Then name the affected
  layer and describe the effect that users will notice.
- Add the flash-size difference when you measured one, and say what the
  change invalidates if that is not obvious.
- The file is append-only. To correct a bullet, add a new one instead of
  editing the old one. `.gitattributes` sets `merge=union` on this file so
  that bullets added on parallel branches merge without conflicts. Check for
  duplicates after a merge.

## Unreleased

- docs: add agent instructions (`AGENTS.md`, `CLAUDE.md`) and this changelog.
