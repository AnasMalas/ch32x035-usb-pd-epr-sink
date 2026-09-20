"use strict";

(() => {
  const lookup = (table, value, fallback) => table[value] ?? `${fallback}(${value})`;
  const u16 = (bytes, offset) => bytes[offset] | (bytes[offset + 1] << 8);
  const u32 = (bytes, offset) => (
    bytes[offset]
    | (bytes[offset + 1] << 8)
    | (bytes[offset + 2] << 16)
    | (bytes[offset + 3] << 24)
  ) >>> 0;

  const hardResetReasons = Object.freeze({
    0: "source-signaled", 1: "invalid-source-capabilities", 2: "soft-reset-failed",
    3: "source-capabilities-timeout", 4: "request-response-timeout", 5: "power-transition-failure",
    6: "EPR-capabilities-timeout", 7: "EPR-protocol-error", 8: "EPR-keepalive-failed", 255: "unspecified",
  });
  const applicationEvents = Object.freeze({
    1: "hard-reset", 2: "PHY-reset-failed", 3: "PHY-unstable", 4: "partner-timeout",
    5: "protocol-recovery", 6: "terminal", 7: "EPR-entry-failed",
  });
  const txReasons = Object.freeze({
    1: "driver-discarded", 2: "GoodCRC-timeout", 3: "hard-reset", 4: "detached",
    5: "retries-exceeded", 6: "acknowledge-mismatch", 255: "other",
  });
  const protocolErrors = Object.freeze({
    1: "RX-discarded", 2: "RX-detached", 3: "RX-soft-reset", 4: "RX-hard-reset",
    5: "RX-timeout", 6: "RX-unsupported", 7: "RX-parse", 8: "RX-acknowledge-mismatch",
    9: "TX-discarded", 10: "TX-detached", 11: "TX-hard-reset", 12: "TX-validation",
    13: "TX-retries-exceeded", 14: "unexpected-message",
  });
  const hardResetPhases = Object.freeze({
    1: "received", 2: "transmit-start", 3: "transmit-retry", 4: "transmit-complete", 5: "transmit-failure",
  });
  const keepAlivePhases = Object.freeze({
    1: "request", 2: "acknowledged", 3: "timeout", 4: "unexpected-response", 5: "protocol-failure",
  });
  const paths = Object.freeze({ 0: "software", 1: "hardware" });
  const eventNames = Object.freeze({
    1: "TX-start", 2: "GoodCRC-wait", 3: "GoodCRC-received", 4: "TX-hardware-retry",
    5: "TX-retry", 6: "TX-success", 7: "TX-failure", 8: "RX-message",
    9: "GoodCRC-transmitted", 10: "RX-retransmission", 11: "protocol-error",
    12: "hard-reset", 13: "EPR-keepalive",
  });
  const controlMessages = Object.freeze({
    1: "GoodCRC", 2: "GotoMin", 3: "Accept", 4: "Reject", 5: "Ping", 6: "PS_RDY",
    7: "Get_Source_Cap", 8: "Get_Sink_Cap", 9: "DR_Swap", 10: "PR_Swap", 11: "VCONN_Swap",
    12: "Wait", 13: "Soft_Reset", 14: "Data_Reset", 15: "Data_Reset_Complete", 16: "Not_Supported",
    17: "Get_Source_Cap_Extended", 18: "Get_Status", 19: "FR_Swap", 20: "Get_PPS_Status",
    21: "Get_Country_Codes", 22: "Get_Sink_Cap_Extended", 23: "Get_Source_Info", 24: "Get_Revision",
  });
  const dataMessages = Object.freeze({
    1: "Source_Capabilities", 2: "Request", 3: "BIST", 4: "Sink_Capabilities",
    5: "Battery_Status", 6: "Alert", 7: "Get_Country_Info", 8: "Enter_USB",
    9: "EPR_Request", 10: "EPR_Mode", 11: "Source_Info", 12: "Revision",
  });
  const extendedMessages = Object.freeze({
    1: "Source_Capabilities_Extended", 2: "Status", 3: "Get_Battery_Cap", 4: "Get_Battery_Status",
    5: "Battery_Capabilities", 6: "Get_Manufacturer_Info", 7: "Manufacturer_Info",
    8: "Security_Request", 9: "Security_Response", 10: "Firmware_Update_Request",
    11: "Firmware_Update_Response", 12: "PPS_Status", 13: "Country_Info", 14: "Country_Codes",
    15: "Sink_Capabilities_Extended", 16: "Extended_Control", 17: "EPR_Source_Capabilities",
    18: "EPR_Sink_Capabilities",
  });

  function parseResponse(bytes) {
    if (bytes.length < 6 || bytes[0] !== 0x50 || bytes[1] !== 0x44 || bytes[2] !== 0x42 || bytes[3] !== 0x42) {
      throw new Error("Invalid black-box response magic.");
    }
    const length = bytes[5];
    if (length > 16 || bytes.length !== 6 + length) throw new Error("Invalid black-box response length.");
    return { kind: bytes[4], payload: bytes.slice(6) };
  }

  function decodeSummary(payload) {
    if (payload.length !== 14) throw new Error("Invalid black-box summary.");
    const stateFlags = payload[1];
    const logFlags = payload[3];
    return {
      abi: payload[0],
      valid: Boolean(stateFlags & 1),
      restored: Boolean(stateFlags & 2),
      dirty: Boolean(stateFlags & 4),
      writeError: Boolean(stateFlags & 8),
      eventCount: payload[2],
      hardReset: Boolean(logFlags & 1),
      frozen: Boolean(logFlags & 2),
      overwritten: Boolean(logFlags & 4),
      generation: u32(payload, 4),
      nextSequence: u16(payload, 8),
      traceAbi: payload[10],
      activePage: payload[11],
      preparedPage: payload[12],
      profile: payload[13] === 2 ? "deep numeric" : "high-level",
    };
  }

  function describeHeader(header) {
    if (header === 0xffff) return "header=n/a";
    const type = header & 0x1f;
    const count = (header >>> 12) & 7;
    const extended = Boolean(header & 0x8000);
    const message = extended
      ? lookup(extendedMessages, type, "extended")
      : count === 0
        ? lookup(controlMessages, type, "control")
        : lookup(dataMessages, type, "data");
    return `${message}, id=${(header >>> 9) & 7}, objects=${count}, header=0x${header.toString(16).padStart(4, "0")}`;
  }

  function decodeEvent(payload, index) {
    if (payload.length !== 16 || payload[2] !== 1 || payload[3] !== 12) {
      throw new Error(`Invalid black-box event page ${index + 1}.`);
    }
    const event = payload.slice(4);
    const uptime = u32(event, 0);
    const kind = event[4];
    const code = event[5];
    const messageId = event[6];
    const counter = event[7];
    const header = u16(event, 8);
    const detail = u16(event, 10);
    let text;

    if (kind & 0x80) {
      const applicationKind = kind & 0x7f;
      if (applicationKind === 1) {
        text = `APP hard-reset ${(code & 0x80) ? "sent" : "received"}; reason=${lookup(hardResetReasons, code & 0x7f, "reason")}; recovery=${detail}ms; VBUS-present=${Boolean(messageId & 1)}`;
      } else {
        const context = (header | (detail << 16)) >>> 0;
        text = `APP ${lookup(applicationEvents, applicationKind, "event")}; code=${code}; context=${context}`;
      }
    } else {
      const name = lookup(eventNames, kind, "event");
      let description = describeHeader(header);
      if ([2, 3, 4, 6, 9].includes(kind)) description = `path=${lookup(paths, code, "path")}`;
      else if (kind === 5 || kind === 7) description = `reason=${lookup(txReasons, code, "reason")}`;
      else if (kind === 11) description = `error=${lookup(protocolErrors, code, "error")}; detail=${detail}`;
      else if (kind === 12) description = `phase=${lookup(hardResetPhases, code, "phase")}; reason=${lookup(hardResetReasons, detail, "reason")}`;
      else if (kind === 13) description = `phase=${lookup(keepAlivePhases, code, "phase")}`;
      const wire = kind >= 1 && kind <= 10 ? `; ${describeHeader(header)}` : "";
      const retry = counter !== 255 ? `; counter=${counter}` : "";
      text = `${name}; ${description}${retry}${wire}`;
    }
    return { index, uptime, text };
  }

  globalThis.PdBlackBox = Object.freeze({ parseResponse, decodeSummary, decodeEvent, describeHeader });
})();
