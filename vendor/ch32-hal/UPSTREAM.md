# Vendored ch32-hal

This directory is a narrow in-tree fork of:

- repository: <https://github.com/ch32-rs/ch32-hal>
- commit: `71a2985804962e248efc09e3bb4f21d4459b50e8`
- license: MIT OR Apache-2.0

Only the crate files needed by this workspace are retained. Keeping the
descendant in-tree makes the CH32X035 behavior reproducible without depending
on a mutable remote branch.

Local changes relevant to this project:

1. allocate 34 bytes for RX DMA because the peripheral writes the 30-byte PD
   message plus its 4-byte CRC;
2. reject malformed receive lengths and undersized destination buffers;
3. reject TX messages over 30 bytes instead of copying 30 bytes and telling the
   peripheral to transmit the original, longer length;
4. expose Hard Reset and buffer faults distinctly and clean up async transfer
   state so failed frames cannot be mistaken for successful traffic;
5. sample the active CC line at the PD 3.x SinkTxOK/SinkTxNG threshold before a
   sink-initiated AMS, then restore the normal receive threshold;
6. add a compact CH32X035 USBFS CDC-ACM device implementation used by the
   optional command-console example, including the programmed factory UID as
   its serial identity;
7. gate unrelated USB driver modules by the peripheral selected for the target
   chip and make small warning-cleanup changes needed by the pinned toolchain.

The product-independent RX/TX safety fixes and generally useful USB support are
candidates for later upstream submissions. The public API of this descendant
is supported only to the extent exercised by the CH32X035 sink and reference
firmware in this repository.
