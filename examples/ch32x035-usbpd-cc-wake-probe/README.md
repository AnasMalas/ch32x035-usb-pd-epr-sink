# CH32X035 USB-PD CC wake probe

This disposable diagnostic firmware checks the undocumented practical
behavior of the CH32X035 USB-PD port wake interrupt (`IE_PD_IO`). It does not
negotiate USB-PD, sense VBUS, or drive a load-control output.

Do not use this as sink firmware or as a safety mechanism. Power the MCU from
the debugger or another independent low-voltage supply so unplugging the
USB-C Source does not remove power from the probe itself. The board must have
the normal external Type-C sink pull-downs on PC14/CC1 and PC15/CC2.

The probe:

1. finds the attached CC orientation with the 0.22 V comparator threshold;
2. arms the separate `USBPD_WKUP` interrupt for a low active-CC level;
3. counts every wake interrupt, including transient levels that recover before
   the task runs;
4. reports a sustained-low observation and samples the comparator for the
   following millisecond; and
5. waits for reattachment and repeats.

Output uses WCH's SDI debug-print channel, so use a WCH-Link or WCH-LinkE and
its terminal/log viewer. This first probe intentionally avoids assigning an
unknown dev-board GPIO as a scope marker.

Build for the exact MCU marking. For example:

```powershell
cargo build -p ch32x035-usbpd-cc-wake-probe --release --locked --no-default-features --features ch32x035f8u6
```

All six CH32X035 package features accepted by the reference firmware are
available. Only one MCU feature may be selected at a time.

Useful observations to record are:

- whether attachment remains quiet for several minutes while the Source sends
  its unanswered Source Capabilities;
- the wake-count delta on unplug (one is the expected simple result);
- whether the comparator remains low through the 1 ms sample window;
- whether reattachment arms and wakes correctly on repeated cable rotations;
  and
- whether any interrupt loop or missed wake appears.

This experiment establishes interrupt and re-arm behavior only. A production
early-detach detector still needs testing at the normal receive threshold while
real PD traffic is active, a conservative debounce policy, and the independent
VBUS/hardware cutoff paths described by the reference firmware.
