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

- fix(ch32-hal): the USBPD interrupt handler now owns reception. It queues
  each accepted SOP frame, sends GoodCRC after a 30 us gap, and re-arms RX
  itself, so executor latency can no longer delay GoodCRC or leave the
  receiver deaf. RX, TX and GoodCRC use separate DMA buffers. Hard Reset
  detection stays armed except during the sink's own transmissions.
  Transmissions and stalled GoodCRCs are bounded by a 5 ms watchdog, and
  SinkTxOK is not sampled while the line is busy. The CH32 driver now
  reports `HAS_AUTO_GOOD_CRC`. Driver-trace ABI is now version 2.
  Hardware verification is still pending.
- fix(usbpd): while waiting for GoodCRC, a late GoodCRC for an earlier
  message and a partner retransmission are ignored. A new partner message
  discards the local one (6.12.2.2) and is delivered next instead of forcing
  a Soft_Reset. Its MessageID is consumed, and an answer to the discarded
  message counts as its acknowledgement. Receives are polled before their
  timers, and stray GoodCRCs never reach the policy engine. The protocol
  error and TX reason `TransmitDiscarded` / `DiscardedByReceive` are new
  (codes 15 and 7).
- fix(usbpd): SinkTxNG is re-sampled every 10 ms instead of every 1 ms. Each
  sample briefly moves the receive comparator threshold and corrupts any
  frame on the wire.
- feat(pd-sink): `SinkConfig::sink_ams_guard_ms` sets a quiet interval
  after each exchange before a Sink-initiated AMS. The reference firmware
  uses 20 ms.
- docs: add agent instructions (`AGENTS.md`, `CLAUDE.md`) and this changelog.
