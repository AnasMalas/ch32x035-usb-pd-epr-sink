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
8. make general DMA/BDMA initialization optional while leaving it enabled by
   default; peripheral-local USBFS and USB-PD DMA are unaffected.
9. pin `ch32-metapac` commit
   `88920cd5a36a13aef475ce0814aaa17294ee9bc9`, whose memory-script renderer
   emits the zero-address boot alias required by CH32X035 application images;
   the previous revision linked G8U6 code at its unusable `0x08000000` alias.
10. move USB-PD reception into the interrupt handler: every accepted SOP frame
    is queued, answered with GoodCRC after a 30 us inter-frame gap, and the
    receiver is re-armed without task involvement. RX, TX, and GoodCRC use
    separate DMA buffers; Hard Reset detection stays armed except during the
    sink's own transmissions; transmissions and stalled GoodCRCs are bounded
    by a 5 ms watchdog; and SinkTxOK is not sampled while the line is busy.
    Previously the PD task sent GoodCRC and re-armed RX, so any executor
    latency beyond about 0.5 ms produced late acknowledgements and a deaf
    receiver.

The fork keeps broadly reusable RX/TX and USB changes separable from
sink-specific policy so they can be proposed upstream independently. The local
public API is supported only to the extent exercised by the CH32X035 sink and
reference firmware in this repository.
