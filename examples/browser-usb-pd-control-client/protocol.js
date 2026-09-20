"use strict";

(() => {
  const MAGIC = [0x50, 0x44];
  const VERSION = 1;
  const MAX_PAYLOAD_LENGTH = 56;
  const NONE_U32 = 0xffffffff;
  const VOLTAGE_CEILINGS = Object.freeze([5000, 21000, 28000, 48000, 50000]);

  const COMMAND = Object.freeze({
    device: 0x01,
    caps: 0x02,
    plans: 0x03,
    "source-info": 0x04,
    "enter-epr": 0x05,
    "epr-caps": 0x06,
    "exit-epr": 0x07,
    status: 0x08,
    help: 0x09,
    "source-status": 0x0a,
    "pps-status": 0x0b,
    "output-on": 0x0c,
    "output-off": 0x0d,
    request: 0x10,
    pdo: 0x11,
  });

  const EVENT = Object.freeze({
    commandResult: 0x80,
    device: 0x81,
    lifecycle: 0x82,
    capabilities: 0x83,
    plan: 0x84,
    controllerError: 0x85,
    sourceInfo: 0x86,
    requestResult: 0x87,
    hardReset: 0x88,
    epr: 0x89,
    capabilityPlansStarted: 0x8a,
    capabilityPlanUnavailable: 0x8b,
    integrationError: 0x8c,
    help: 0x8d,
    sourceAlert: 0x8e,
    sourceStatus: 0x8f,
    ppsStatus: 0x90,
    statusQueryFailed: 0x91,
    contractTransition: 0x92,
  });

  const PREFERENCE = Object.freeze({
    auto: 0,
    fixed: 1,
    pps: 2,
    "spr-avs": 3,
    "epr-avs": 4,
    avs: 4,
    "epr-avs-nonstandard": 5,
  });

  function crc8(bytes) {
    let crc = 0;
    for (const byte of bytes) {
      crc ^= byte;
      for (let bit = 0; bit < 8; bit += 1) {
        crc = crc & 0x80 ? ((crc << 1) ^ 0x07) & 0xff : (crc << 1) & 0xff;
      }
    }
    return crc;
  }

  function boundedVoltageCeiling(value, deviceMaximum = 48000) {
    const requested = Number(value);
    const selected = VOLTAGE_CEILINGS.includes(requested) ? requested : 5000;
    if (selected <= deviceMaximum) return selected;
    return VOLTAGE_CEILINGS.filter((candidate) => candidate <= deviceMaximum)
      .sort((left, right) => right - left)[0] ?? 5000;
  }

  function encodeFrame(kind, sequence, payload = new Uint8Array(), version = VERSION) {
    const body = payload instanceof Uint8Array ? payload : Uint8Array.from(payload);
    if (body.length > MAX_PAYLOAD_LENGTH) throw new Error("USB-control payload is too long.");
    const frame = new Uint8Array(body.length + 7);
    frame[0] = MAGIC[0];
    frame[1] = MAGIC[1];
    frame[2] = version;
    frame[3] = kind;
    frame[4] = sequence & 0xff;
    frame[5] = body.length;
    frame.set(body, 6);
    frame[frame.length - 1] = crc8(frame.subarray(2, frame.length - 1));
    return frame;
  }

  class FrameDecoder {
    constructor() {
      this.buffer = [];
    }

    reset() {
      this.buffer.length = 0;
    }

    push(chunk) {
      this.buffer.push(...chunk);
      const frames = [];
      let errors = 0;

      while (this.buffer.length > 0) {
        let magic = -1;
        for (let index = 0; index + 1 < this.buffer.length; index += 1) {
          if (this.buffer[index] === MAGIC[0] && this.buffer[index + 1] === MAGIC[1]) {
            magic = index;
            break;
          }
        }
        if (magic < 0) {
          this.buffer.splice(0, Math.max(0, this.buffer.length - 1));
          break;
        }
        if (magic > 0) this.buffer.splice(0, magic);
        if (this.buffer.length < 6) break;

        const payloadLength = this.buffer[5];
        if (payloadLength > MAX_PAYLOAD_LENGTH) {
          this.buffer.shift();
          errors += 1;
          continue;
        }
        const frameLength = payloadLength + 7;
        if (this.buffer.length < frameLength) break;

        const checksum = crc8(this.buffer.slice(2, frameLength - 1));
        if (checksum !== this.buffer[frameLength - 1]) {
          this.buffer.shift();
          errors += 1;
          continue;
        }

        const bytes = this.buffer.splice(0, frameLength);
        frames.push({
          version: bytes[2],
          kind: bytes[3],
          sequence: bytes[4],
          payload: Uint8Array.from(bytes.slice(6, frameLength - 1)),
        });
      }

      if (this.buffer.length > 512) {
        this.buffer.splice(0, this.buffer.length - 1);
        errors += 1;
      }
      return { frames, errors };
    }
  }

  function u32Bytes(value) {
    const number = Number(value) >>> 0;
    return [number & 0xff, (number >>> 8) & 0xff, (number >>> 16) & 0xff, (number >>> 24) & 0xff];
  }

  function parseU32(word, label, { allowZero = false } = {}) {
    if (!/^\d+$/.test(word ?? "")) throw new Error(`${label} must be a whole number.`);
    const value = Number(word);
    if (!Number.isSafeInteger(value) || value > NONE_U32 || (!allowZero && value === 0)) {
      throw new Error(`${label} is outside the supported range.`);
    }
    return value;
  }

  function requireEnd(words, index) {
    if (index !== words.length) throw new Error("Unexpected command argument.");
  }

  function encodeCommand(commandLine, sequence) {
    const words = commandLine.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (words.length === 0) throw new Error("Enter a command first.");

    const aliases = Object.freeze({
      identity: "device",
      capabilities: "caps",
      "pd-status": "source-status",
      voltage: "request",
    });
    const name = aliases[words[0]] ?? words[0];

    if (Object.hasOwn(COMMAND, name) && !["request", "pdo"].includes(name)) {
      requireEnd(words, 1);
      return encodeFrame(COMMAND[name], sequence);
    }

    if (name === "request") {
      const voltage = parseU32(words[1], "Voltage");
      let current = NONE_U32;
      let preference = PREFERENCE.auto;
      let index = 2;
      if (index < words.length) {
        const first = words[index];
        if (Object.hasOwn(PREFERENCE, first)) {
          preference = PREFERENCE[first];
          index += 1;
        } else if (first === "max") {
          index += 1;
          if (index < words.length) {
            if (!Object.hasOwn(PREFERENCE, words[index])) throw new Error("Unknown supply preference.");
            preference = PREFERENCE[words[index]];
            index += 1;
          }
        } else {
          current = parseU32(first, "Current");
          index += 1;
          if (index < words.length) {
            if (!Object.hasOwn(PREFERENCE, words[index])) throw new Error("Unknown supply preference.");
            preference = PREFERENCE[words[index]];
            index += 1;
          }
        }
      }
      requireEnd(words, index);
      return encodeFrame(COMMAND.request, sequence, Uint8Array.from([
        ...u32Bytes(voltage),
        ...u32Bytes(current),
        preference,
      ]));
    }

    if (name === "pdo") {
      const position = parseU32(words[1], "PDO position");
      if (position > 0xff) throw new Error("PDO position is outside the supported range.");
      const demand = words[2] ?? "max";
      let demandCode;
      let voltage = 0;
      let current = NONE_U32;
      let index;
      if (demand === "max") {
        demandCode = 0;
        index = 3;
      } else if (demand === "current") {
        demandCode = 1;
        current = parseU32(words[3], "Current");
        index = 4;
      } else if (demand === "adjust") {
        demandCode = 2;
        voltage = parseU32(words[3], "Voltage");
        index = 4;
        if (index < words.length) {
          if (words[index] !== "max") current = parseU32(words[index], "Current");
          index += 1;
        }
      } else {
        throw new Error("Demand must be max, current, or adjust.");
      }
      requireEnd(words, index);
      return encodeFrame(COMMAND.pdo, sequence, Uint8Array.from([
        position,
        demandCode,
        ...u32Bytes(voltage),
        ...u32Bytes(current),
      ]));
    }

    throw new Error("Unknown command. Open the command reference for supported syntax.");
  }

  function readU32(payload, offset) {
    if (offset + 4 > payload.length) throw new Error("Truncated USB-control event.");
    return (payload[offset]
      + payload[offset + 1] * 0x100
      + payload[offset + 2] * 0x10000
      + payload[offset + 3] * 0x1000000) >>> 0;
  }

  function expectLength(payload, expected) {
    if (payload.length !== expected) throw new Error("Invalid USB-control event length.");
  }

  function hex32(value) {
    return `0x${(value >>> 0).toString(16).padStart(8, "0")}`;
  }

  function temperatureName(value) {
    return ["unsupported", "normal", "warning", "over-temperature"][value & 0x03];
  }

  function ppsStatusLine(payload) {
    expectLength(payload, 4);
    const voltageUnits = payload[0] + payload[1] * 0x100;
    const voltage = voltageUnits === 0xffff ? "unsupported" : `${voltageUnits * 20}mV`;
    const current = payload[2] === 0xff ? "unsupported" : `${payload[2] * 50}mA`;
    const mode = payload[3] & 0x08 ? "CL" : "CV";
    return `PPS_Status: voltage=${voltage} current=${current} mode=${mode} temperature=${temperatureName(payload[3] >> 1)}`;
  }

  function sourceStatusLine(raw, ppsModeValid = false) {
    expectLength(raw, 7);
    const internal = raw[0] === 0 ? "unsupported" : raw[0] === 1 ? "below-2C" : `${raw[0]}C`;
    const input = ["internal", "DC", "invalid", "AC"][(raw[1] >> 1) & 0x03];
    const events = [];
    if (raw[3] & 0x02) events.push("OCP");
    if (raw[3] & 0x04) events.push("OTP");
    if (raw[3] & 0x08) events.push("OVP");
    const limits = [];
    if (raw[5] & 0x02) limits.push("cable");
    if (raw[5] & 0x04) limits.push("other-ports");
    if (raw[5] & 0x08) limits.push("external-power");
    if (raw[5] & 0x10) limits.push("event");
    if (raw[5] & 0x20) limits.push("temperature");
    const states = ["unsupported", "S0", "modern-standby", "S3", "S4", "S5", "G3", "invalid"];
    const indicators = ["off", "on", "blinking", "breathing", "invalid", "invalid", "invalid", "invalid"];
    const mode = ppsModeValid ? (raw[3] & 0x10 ? "CL" : "CV") : "n/a";
    return [
      "Source_Status:",
      `mode=${mode}`,
      `internal=${internal}`,
      `input=${input}`,
      `battery=${Boolean(raw[1] & 0x08)}`,
      `non-battery=${Boolean(raw[1] & 0x10)}`,
      `temperature=${temperatureName(raw[4] >> 1)}`,
      `events=${events.join(",") || "none"}`,
      `limits=${limits.join(",") || "none"}`,
      `state=${states[raw[6] & 0x07]}`,
      `indicator=${indicators[(raw[6] >> 3) & 0x07]}`,
    ].join(" ");
  }

  function sourceAlertLine(payload) {
    expectLength(payload, 4);
    const raw = readU32(payload, 0);
    const events = [];
    if ((raw & 0x80000000) !== 0 && (raw & 0x0f) === 5) events.push("reduced-capabilities");
    if (raw & 0x40000000) events.push("OVP");
    if (raw & 0x20000000) events.push("input-change");
    if (raw & 0x10000000) events.push("condition-change");
    if (raw & 0x08000000) events.push("OTP");
    if (raw & 0x04000000) events.push("OCP");
    if (raw & 0x02000000) events.push("battery");
    return `PD Alert: events=${events.join(",") || "none"} raw=${hex32(raw)}`;
  }

  function statusQueryFailureLine(payload) {
    expectLength(payload, 2);
    const query = ["Source_Status", "PPS_Status", "Source_Info"][payload[0]] ?? `status-${payload[0]}`;
    const reason = ["unsupported", "rejected", "deferred", "timeout", "unavailable-for-PD-revision"][payload[1]] ?? `reason-${payload[1]}`;
    return `${query} query failed: ${reason}`;
  }

  function contractTransitionLine(payload) {
    expectLength(payload, 17);
    const kinds = [
      "initial/no-confirmed-contract",
      "identical-refresh",
      "same-voltage-sufficient-current",
      "same-voltage-reduced-current",
      "same-voltage-unknown-current",
      "voltage-change",
    ];
    const fromVoltage = readU32(payload, 1);
    const fromCurrent = readU32(payload, 5);
    const from = fromVoltage === NONE_U32 || fromCurrent === NONE_U32
      ? "none"
      : `${fromVoltage}mV/${fromCurrent}mA`;
    const inhibitRecommended = [0, 3, 4, 5].includes(payload[0]);
    const loadSafety = inhibitRecommended ? "inhibit-recommended" : "continuity-compatible";
    return `Contract transition: ${kinds[payload[0]] ?? `kind-${payload[0]}`} ${from} -> ${readU32(payload, 9)}mV/${readU32(payload, 13)}mA load-safety=${loadSafety}`;
  }

  function validityName(validity) {
    return ["valid", "compatible", "padding", "malformed", "unsupported"][validity] ?? "malformed";
  }

  function pdoLine(kind, position, rawValue) {
    const raw = rawValue >>> 0;
    const hex = hex32(raw);
    if (raw === 0) {
      const validity = kind === 1 && position >= 2 && position <= 7 ? "padding" : "malformed";
      return `PDO${position} padding ${validity} raw=${hex}`;
    }

    const pdoType = (raw >>> 30) & 0x03;
    if (pdoType === 0) {
      const voltage = ((raw >>> 10) & 0x03ff) * 50;
      const current = (raw & 0x03ff) * 10;
      const epr = Boolean(raw & 0x00800000);
      let validity = "valid";
      if (position === 1 && voltage !== 5000) validity = "malformed";
      else if (current === 0 || current > 5000) validity = "malformed";
      else if ((position <= 7 && voltage > 20000)
        || (position >= 8 && (kind !== 1 || ![28000, 36000, 48000].includes(voltage)))) validity = "malformed";
      else if (voltage === 0) validity = "malformed";
      return `PDO${position} fixed ${voltage}mV ${current}mA EPR=${epr} ${validity} raw=${hex}`;
    }

    if (pdoType === 1 || pdoType === 2) {
      const validity = position === 1 || position > 7 ? "malformed" : "unsupported";
      return `PDO${position} unsupported type=${pdoType} apdo=255 ${validity} raw=${hex}`;
    }

    const apdoType = (raw >>> 28) & 0x03;
    if (apdoType === 0) {
      const minimum = ((raw >>> 8) & 0xff) * 100;
      const maximum = ((raw >>> 17) & 0xff) * 100;
      const current = (raw & 0x7f) * 50;
      const limited = Boolean(raw & 0x08000000);
      let validity = "valid";
      if (position === 1 || position > 7) validity = "malformed";
      else if (minimum < 3300 || maximum > 21000 || minimum > maximum) validity = "malformed";
      else if (current === 0) validity = "malformed";
      else if (![3300, 5000].includes(minimum)
        || ![11000, 16000, 21000].includes(maximum)
        || current > 5000) validity = "compatible";
      return `PDO${position} PPS ${minimum}-${maximum}mV ${current}mA limited=${limited} ${validity} raw=${hex}`;
    }

    if (apdoType === 1) {
      const minimum = ((raw >>> 8) & 0xff) * 100;
      const maximum = ((raw >>> 17) & 0x01ff) * 100;
      const pdp = (raw & 0xff) * 1000;
      const peak = (raw >>> 26) & 0x03;
      let validity = "valid";
      if (position < 8 || kind !== 1) validity = "malformed";
      else if (minimum < 5000 || maximum > 50000 || minimum > maximum) validity = "malformed";
      else if (pdp === 0 || pdp > 240000) validity = "malformed";
      else if (minimum < 15000 || maximum > 48000) validity = "compatible";
      const standardMinimum = Math.max(minimum, 15000);
      const standardMaximum = Math.min(maximum, 48000);
      const standardRange = standardMinimum <= standardMaximum
        ? `${standardMinimum}-${standardMaximum}mV`
        : "none";
      return `PDO${position} EPR-AVS ${minimum}-${maximum}mV standard=${standardRange} PDP=${pdp}mW peak=${peak} ${validity} raw=${hex}`;
    }

    if (apdoType === 2) {
      const current15 = ((raw >>> 10) & 0x03ff) * 10;
      const current20 = (raw & 0x03ff) * 10;
      const maximum = current20 === 0 ? 15000 : 20000;
      const peak = (raw >>> 26) & 0x03;
      const validity = position === 1 || position > 7
        || current15 === 0 || current15 > 5000 || current20 > 5000
        ? "malformed"
        : "valid";
      return `PDO${position} SPR-AVS 9000-${maximum}mV ${current15}mA@15V ${current20}mA@20V peak=${peak} ${validity} raw=${hex}`;
    }

    return `PDO${position} unsupported type=3 apdo=${apdoType} unsupported raw=${hex}`;
  }

  function translateDevice(payload) {
    expectLength(payload, 21);
    const uid = payload.subarray(0, 8);
    const unavailable = uid.every((byte) => byte === 0) || uid.every((byte) => byte === 0xff);
    const identity = unavailable
      ? "unavailable"
      : [...uid].map((byte) => byte.toString(16).padStart(2, "0")).join("");
    const flags = payload[8];
    return [
      `Device id=${identity}`,
      `Device limits: max=${readU32(payload, 9)}mV current=${readU32(payload, 13)}mA power=${readU32(payload, 17)}mW PPS=${Boolean(flags & 1)} EPR=${Boolean(flags & 2)} TELEMETRY=${flags & 4 ? "rich" : "basic"}`,
    ];
  }

  function translateLifecycle(payload) {
    expectLength(payload, 9);
    const code = payload[0];
    const detail = readU32(payload, 1);
    const extra = readU32(payload, 5);
    const lines = {
      0: "USB control ready; protocol=1",
      1: "CC detected; waiting for VBUS",
      2: "Attached; PD starts at 5 V",
      3: "SinkTxOK; resume",
      4: "SinkTxNG; deferred",
      5: "PD stopped",
      6: "PD stopped: detach",
      7: `PD stopped: PHY; retry=${detail}ms`,
      8: `PD stopped: timeout; passive retry=${detail}ms`,
      9: `PD stopped: protocol; retry=${detail}ms`,
      10: `PD stopped: policy; retry=${detail}ms`,
      11: "PD reset failed",
      12: "Detached; contract lost; load off",
      13: `Protocol lost; load off; EPR=${detail}/${extra}`,
      14: "Reset recovery complete; Source_Capabilities received",
    };
    return [lines[code] ?? `Lifecycle event=${code} detail=${detail} extra=${extra}`];
  }

  function translateCapabilities(payload) {
    if (payload.length < 3) throw new Error("Invalid capability event length.");
    const kind = payload[0];
    const epr = Boolean(payload[1]);
    const count = payload[2];
    expectLength(payload, 3 + count * 4);
    const kindName = kind === 0 ? "SPR" : kind === 1 ? "EPR" : `unknown-${kind}`;
    const lines = [`Source caps: kind=${kindName} count=${count} EPR=${epr}`];
    for (let index = 0; index < count; index += 1) {
      lines.push(pdoLine(kind, index + 1, readU32(payload, 3 + index * 4)));
    }
    return lines;
  }

  function translatePlan(payload) {
    if (payload.length < 2) throw new Error("Invalid plan event length.");
    const stage = payload[0];
    const prefixes = ["Plan", "Requesting", "Contract ready"];
    const prefix = prefixes[stage] ?? `Plan stage ${stage}`;
    if (payload[1] === 0) {
      expectLength(payload, 2);
      return [prefix === "Contract ready" ? "No confirmed contract" : `${prefix} unavailable`];
    }
    expectLength(payload, 38);
    const position = payload[2];
    const flags = payload[4];
    const voltageKind = payload[5];
    const requestedVoltage = readU32(payload, 6);
    const encodedVoltage = readU32(payload, 10);
    const step = payload[14] + payload[15] * 0x100;
    const requestedCurrent = readU32(payload, 16);
    const sourceCurrent = readU32(payload, 20);
    const usableCurrent = readU32(payload, 24);
    const confidence = ["advertised", "derived-from-PDO-PDP", "derived-from-Source_Info", "power-limited-upper-bound"][payload[28]] ?? `unknown-${payload[28]}`;
    const limit = ["source", "protocol", "board", "cable", "sink-power", "user"][payload[29]] ?? `unknown-${payload[29]}`;
    const rdo = readU32(payload, 30);
    const epr = flags & 1 ? 1 : 0;
    const mismatch = Boolean(flags & 2);
    if (stage === 3) {
      return [`Contract refresh confirmed PDO${position} ${encodedVoltage}mV usable=${usableCurrent}mA`];
    }
    const voltageLine = voltageKind === 0
      ? `${prefix} PDO${position} fixed=${encodedVoltage}mV EPR=${epr}`
      : `${prefix} PDO${position} requested=${requestedVoltage}mV encoded=${encodedVoltage}mV step=${step}mV EPR=${epr}`;
    const requested = requestedCurrent === NONE_U32 ? "max" : `${requestedCurrent}mA`;
    const lines = [
      voltageLine,
      `${prefix} req=${requested} src=${sourceCurrent}mA usable=${usableCurrent}mA confidence=${confidence} limit=${limit} mismatch=${mismatch}`,
    ];
    if (prefix === "Requesting") lines.push(`RDO=${hex32(rdo)}`);
    return lines;
  }

  function translateControllerError(payload) {
    expectLength(payload, 9);
    const reasons = {
      0: "no-caps",
      1: "epr-busy",
      2: "epr-unavailable",
      3: "epr-not-configured",
      4: "epr-pdp",
      5: "not-in-epr",
      6: "epr-exit-refused",
      7: "epr-entry-refused",
      16: "pdo-missing",
      17: "pdo-malformed",
      18: "pdo-unsupported",
      19: "demand-kind",
      20: "voltage-unavailable",
      21: "voltage-outside",
      22: "voltage-limit",
      23: "caps-mode",
      24: "epr-required",
      25: "zero-operating",
    };
    const code = payload[0];
    return [`Rejected ${reasons[code] ?? `controller-${code}`} detail=${readU32(payload, 1)} extra=${readU32(payload, 5)}`];
  }

  function translateEpr(payload) {
    expectLength(payload, 3);
    const code = payload[0];
    const detail = payload[1];
    const extra = payload[2];
    const lines = {
      0: `EPR entry failed reason=${detail}; auto off`,
      1: `EPR discovery: enter attempt=${detail}/${extra}; preserve confirmed contract`,
      2: "EPR discovery unavailable; auto off",
      3: "EPR auto off; staying SPR; manual retry available",
      4: "EPR manual enter; preserve confirmed contract",
    };
    return [lines[code] ?? `EPR event=${code} detail=${detail} extra=${extra}`];
  }

  function translateFrame(frame) {
    if (frame.version !== VERSION) return [`Unsupported USB-control protocol version ${frame.version}`];
    const payload = frame.payload;
    switch (frame.kind) {
      case EVENT.commandResult:
        expectLength(payload, 1);
        return [["Queued", "Busy; retry command", "Invalid; type help", "Unsupported command"][payload[0]] ?? `Command result=${payload[0]}`];
      case EVENT.device:
        return translateDevice(payload);
      case EVENT.lifecycle:
        return translateLifecycle(payload);
      case EVENT.capabilities:
        return translateCapabilities(payload);
      case EVENT.plan:
        return translatePlan(payload);
      case EVENT.controllerError:
        return translateControllerError(payload);
      case EVENT.sourceInfo:
        expectLength(payload, 3);
        return [`Source_Info: present=${payload[0]} W, maximum=${payload[1]} W, reported=${payload[2]} W`];
      case EVENT.requestResult:
        expectLength(payload, 1);
        return [payload[0] === 0 ? "Request rejected; old contract active" : payload[0] === 1 ? "Request deferred; retry armed" : `Request result=${payload[0]}`];
      case EVENT.hardReset:
        if (payload.length !== 5 && payload.length !== 6) {
          throw new Error(`Expected 5 or 6 payload bytes, received ${payload.length}.`);
        }
        {
          const causes = [
            "source-signaled",
            "invalid-source-capabilities",
            "soft-reset-failed",
            "source-capabilities-timeout",
            "request-response-timeout",
            "power-transition-failure",
            "epr-capabilities-timeout",
            "epr-protocol-error",
            "epr-keepalive-failed",
          ];
          const cause = payload.length === 6 ? (causes[payload[5]] ?? `cause-${payload[5]}`) : "legacy-unspecified";
          return [`Hard reset ${payload[0] === 0 ? "received" : payload[0] === 1 ? "sent" : `direction-${payload[0]}`}; cause=${cause}; load off; recovery=${readU32(payload, 1)}ms`];
        }
      case EVENT.epr:
        return translateEpr(payload);
      case EVENT.capabilityPlansStarted:
        expectLength(payload, 1);
        return [`Capability plans: count=${payload[0]} (live contract unchanged)`];
      case EVENT.capabilityPlanUnavailable:
        expectLength(payload, 2);
        return [`Plan PDO${payload[0]} unavailable ${validityName(payload[1])}`];
      case EVENT.integrationError:
        expectLength(payload, 1);
        return [[
          "Source capabilities rejected by integration",
          "Request rejected by integration",
          "Plans unavailable in text console; use usb-epr",
        ][payload[0]] ?? `Integration error=${payload[0]}`];
      case EVENT.help:
        expectLength(payload, 0);
        return [
          "Commands:",
          "device caps plans source-info status source-status pps-status",
          "output-on output-off (external application load only)",
          "enter-epr epr-caps exit-epr help",
          "request mV [mA|max] [auto|fixed|pps|spr-avs|epr-avs]",
          "request mV [mA|max] epr-avs-nonstandard (explicit opt-in)",
          "pdo N [max(=maxV APDO)|current mA|adjust mV [mA|max]]",
        ];
      case EVENT.sourceAlert:
        return [sourceAlertLine(payload)];
      case EVENT.sourceStatus:
        expectLength(payload, 8);
        return [sourceStatusLine(payload.subarray(1), Boolean(payload[0]))];
      case EVENT.ppsStatus:
        return [ppsStatusLine(payload)];
      case EVENT.statusQueryFailed:
        return [statusQueryFailureLine(payload)];
      case EVENT.contractTransition:
        return [contractTransitionLine(payload)];
      default:
        return [`Unknown USB-control event kind ${frame.kind}`];
    }
  }

  const api = Object.freeze({
    VERSION,
    VOLTAGE_CEILINGS,
    FrameDecoder,
    boundedVoltageCeiling,
    crc8,
    encodeFrame,
    encodeCommand,
    pdoLine,
    ppsStatusLine,
    sourceAlertLine,
    sourceStatusLine,
    statusQueryFailureLine,
    contractTransitionLine,
    translateFrame,
  });

  globalThis.PdControlProtocol = api;
})();
