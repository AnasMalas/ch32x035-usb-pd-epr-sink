# Vendored usbpd

This directory is an in-tree fork of `elagil/usbpd`, crate `usbpd` 2.0.0:

- repository: <https://github.com/elagil/usbpd>
- commit: `3c9d3953a156793c52685be56dc70732abc84b7d`
- license declared by upstream: MIT

The descendant is intentionally local because this project needs sink policy,
EPR, recovery, and real-source interoperability changes that have not all
landed upstream. The supported behavior is covered by the host-side tests in
`tests/protocol`.

Local change groups:

1. distinguish physical detach, Hard Reset, discarded frames, and unstable PHY
   behavior, then propagate those outcomes through the sink lifecycle;
2. bound discard, retry, chunk, and unresponsive-partner loops so a failed or
   noisy PHY cannot spin forever;
3. gate PD 3.x sink-initiated AMS traffic on SinkTxOK while allowing source AMS
   traffic and PD 2.0 partners to proceed;
4. correct partner-initiated Soft Reset ordering and reset protocol counters at
   the required point;
5. harden header, payload, extended-message, chunk, EPR-mode, and capability
   parsing against malformed or truncated wire data;
6. support complete SPR/EPR capability discovery across eleven PDO positions,
   Source_Info, EPR Sink Capabilities, and Sink Capabilities Extended;
7. support fixed, PPS, SPR AVS, and EPR AVS request flows plus EPR entry, exit,
   keepalive, capability retrieval, and Hard Reset recovery;
8. preserve deferred requests across source-owned traffic, report rejection
   reasons, distinguish sent versus received Hard Reset, and provide bounded
   compatibility behavior for observed noncanonical source offers.

The fork keeps generally applicable protocol corrections separable from
project-specific sink policy so reusable fixes can be proposed upstream
independently. Sink behavior documented by this repository remains maintained
here.
