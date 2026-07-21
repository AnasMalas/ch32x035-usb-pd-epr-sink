# Vendored usbpd-traits

This directory is an in-tree fork of `elagil/usbpd`, crate `usbpd-traits`
2.0.0 at commit `3c9d3953a156793c52685be56dc70732abc84b7d`.

Upstream declares the crate MIT licensed. The local driver traits add an
explicit physical `Detached` result distinct from retryable discarded/noisy
frames. This addition lets the policy engine invalidate a power contract and
cancel blocked I/O without interpreting cable removal as ordinary protocol
noise.
