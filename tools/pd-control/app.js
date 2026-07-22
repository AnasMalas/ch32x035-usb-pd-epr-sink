"use strict";

const $ = (selector) => document.querySelector(selector);
const controlProtocol = globalThis.PdControlProtocol;

const ui = {
  connectButton: $("#connect-button"),
  connectionDot: $("#connection-dot"),
  connectionLabel: $("#connection-label"),
  browserNotice: $("#browser-notice"),
  deviceDescription: $("#device-description"),
  deviceId: $("#device-id"),
  safeFiveButton: $("#safe-five-button"),
  portState: $("#port-state"),
  portDetail: $("#port-detail"),
  contractVoltage: $("#contract-voltage"),
  contractSupply: $("#contract-supply"),
  usableCurrent: $("#usable-current"),
  currentConfidence: $("#current-confidence"),
  sourceKind: $("#source-kind"),
  sourceSummary: $("#source-summary"),
  voltageForm: $("#voltage-form"),
  voltageInput: $("#voltage-input"),
  currentInput: $("#current-input"),
  preferenceInput: $("#preference-input"),
  voltageCeilingInputs: document.querySelectorAll('input[name="voltage-ceiling"]'),
  voltageCeilingNote: $("#voltage-ceiling-note"),
  requestFeedback: $("#request-feedback"),
  enterEprButton: $("#enter-epr-button"),
  eprCapsButton: $("#epr-caps-button"),
  exitEprButton: $("#exit-epr-button"),
  pdoForm: $("#pdo-form"),
  pdoPosition: $("#pdo-position"),
  pdoDemand: $("#pdo-demand"),
  pdoVoltageField: $("#pdo-voltage-field"),
  pdoVoltage: $("#pdo-voltage"),
  pdoCurrentField: $("#pdo-current-field"),
  pdoCurrent: $("#pdo-current"),
  capabilityCount: $("#capability-count"),
  capabilityBody: $("#capability-body"),
  terminal: $("#terminal"),
  autoScroll: $("#auto-scroll"),
  copyLog: $("#copy-log"),
  saveLog: $("#save-log"),
  clearLog: $("#clear-log"),
  rawCommandForm: $("#raw-command-form"),
  rawCommand: $("#raw-command"),
};

const state = {
  port: null,
  reader: null,
  writer: null,
  readTask: null,
  keepReading: false,
  connected: false,
  transport: null,
  detectionBuffer: [],
  controlDecoder: new controlProtocol.FrameDecoder(),
  textDecoder: new TextDecoder(),
  nextSequence: 1,
  lineBuffer: "",
  log: [],
  pdos: new Map(),
  expectedPdoCount: 0,
  sourceKind: null,
  sourceEprCapable: false,
  inEpr: false,
  usbId: null,
  deviceUid: null,
  maxVoltageMillivolts: 5000,
};

const encoder = new TextEncoder();
const MAX_LOG_LINES = 1500;
const BOARD_SETTING_PREFIX = "usb-pd-control.board.";
const ALLOWED_VOLTAGE_CEILINGS = new Set([5000, 28000, 48000]);

function setFeedback(message, isError = false) {
  ui.requestFeedback.textContent = message;
  ui.requestFeedback.classList.toggle("error", isError);
}

function boardSettingKey(uid) {
  return `${BOARD_SETTING_PREFIX}${uid}.max-voltage-mv`;
}

function normalizeDeviceIdentity(uid) {
  let normalized = String(uid).trim().toLowerCase();
  if (/^[0-9a-f]{24}$/.test(normalized) && /^(?:0{8}|f{8})$/.test(normalized.slice(16))) {
    normalized = normalized.slice(0, 16);
  }
  if (!/^(?:[0-9a-f]{16}|[0-9a-f]{24})$/.test(normalized)) return null;
  if (/^0+$/.test(normalized) || /^f+$/.test(normalized)) return null;
  return normalized;
}

function savedVoltageCeiling(uid) {
  try {
    const currentKey = boardSettingKey(uid);
    const currentValue = Number(localStorage.getItem(currentKey));
    if (ALLOWED_VOLTAGE_CEILINGS.has(currentValue)) return currentValue;

    // Migrate the first test firmware's 96-bit-looking X035 key. Its final
    // ESIG word was unprogrammed, so the useful bootloader-compatible UID is
    // the first 16 hexadecimal characters.
    if (uid.length === 16) {
      const legacyKey = boardSettingKey(`${uid}ffffffff`);
      const legacyValue = Number(localStorage.getItem(legacyKey));
      if (ALLOWED_VOLTAGE_CEILINGS.has(legacyValue)) {
        localStorage.setItem(currentKey, String(legacyValue));
        localStorage.removeItem(legacyKey);
        return legacyValue;
      }
    }
    return 5000;
  } catch (_) {
    return 5000;
  }
}

function persistVoltageCeiling() {
  if (!state.deviceUid) return false;
  try {
    localStorage.setItem(boardSettingKey(state.deviceUid), String(state.maxVoltageMillivolts));
    return true;
  } catch (_) {
    return false;
  }
}

function displayBoardId(uid) {
  return uid ? `Board ${uid.slice(-8).toUpperCase()}` : "Board identity pending";
}

function transportLabel() {
  if (state.transport === "usb-control") return "Compact USB control";
  if (state.transport === "dev-text-console") return "Development text console";
  return "USB CDC detecting protocol";
}

function refreshDeviceDescription() {
  const board = state.deviceUid ? displayBoardId(state.deviceUid) : null;
  const id = [board, state.usbId].filter(Boolean).join(" · ") || "device identity pending";
  ui.deviceDescription.textContent = `${transportLabel()} · ${id}`;
  ui.deviceId.textContent = id;
}

function setVoltageCeiling(value, { persist = false } = {}) {
  const ceiling = Number(value);
  state.maxVoltageMillivolts = ALLOWED_VOLTAGE_CEILINGS.has(ceiling) ? ceiling : 5000;
  for (const input of ui.voltageCeilingInputs) {
    input.checked = Number(input.value) === state.maxVoltageMillivolts;
  }

  const maximumVolts = state.maxVoltageMillivolts / 1000;
  ui.voltageInput.max = String(maximumVolts);
  ui.pdoVoltage.max = String(maximumVolts);
  if (Number(ui.voltageInput.value) > maximumVolts) ui.voltageInput.value = String(maximumVolts);
  if (Number(ui.pdoVoltage.value) > maximumVolts) ui.pdoVoltage.value = String(maximumVolts);

  const stored = persist && persistVoltageCeiling();
  if (state.deviceUid) {
    ui.voltageCeilingNote.textContent = stored
      ? `Saved locally for ${displayBoardId(state.deviceUid)}. This is a GUI guard, not a hardware rating.`
      : `Applied to ${displayBoardId(state.deviceUid)}. Browser storage was unavailable, so it is session-only.`;
  } else {
    ui.voltageCeilingNote.textContent = "Defaults to 5 V until this MCU identifies itself; unsaved changes are session-only.";
  }
  renderCapabilities();
}

function applyDeviceIdentity(rawUid) {
  const uid = normalizeDeviceIdentity(rawUid);
  if (!uid) {
    state.deviceUid = null;
    refreshDeviceDescription();
    ui.deviceId.removeAttribute("title");
    setVoltageCeiling(5000);
    ui.voltageCeilingNote.textContent = "The MCU did not provide a usable identity. This connection stays at the safe 5 V default and is not cached.";
    return;
  }

  state.deviceUid = uid;
  const board = displayBoardId(state.deviceUid);
  refreshDeviceDescription();
  ui.deviceId.title = `MCU unique ID ${state.deviceUid.toUpperCase()}`;
  setVoltageCeiling(savedVoltageCeiling(state.deviceUid));
  ui.voltageCeilingNote.textContent = `Loaded the saved GUI ceiling for ${board}. This is not a hardware rating.`;
}

function updateActionAvailability() {
  const ready = state.connected && state.transport !== null;
  for (const input of ui.voltageCeilingInputs) input.disabled = !ready;
  ui.safeFiveButton.disabled = !ready;
  document.querySelectorAll(".command-button, .request-submit").forEach((button) => {
    button.disabled = !ready;
  });
  ui.rawCommand.disabled = !ready;
  ui.rawCommandForm.querySelector("button").disabled = !ready;
  ui.enterEprButton.disabled = !ready || state.inEpr || !state.sourceEprCapable;
  ui.eprCapsButton.disabled = !ready || !state.inEpr;
  ui.exitEprButton.disabled = !ready || !state.inEpr;
}

function setConnected(connected) {
  state.connected = connected;
  ui.connectionDot.classList.toggle("connected", connected);
  ui.connectionLabel.textContent = connected ? (state.transport ? "Connected" : "Detecting device") : "Disconnected";
  ui.connectButton.textContent = connected ? "Disconnect" : "Connect device";

  if (!connected) {
    state.transport = null;
    state.detectionBuffer.length = 0;
    state.controlDecoder.reset();
    state.textDecoder = new TextDecoder();
    state.nextSequence = 1;
    state.usbId = null;
    state.deviceUid = null;
    setVoltageCeiling(5000);
    ui.deviceDescription.textContent = "Connect USB-control or development text-console firmware to begin.";
    ui.deviceId.textContent = "No device selected";
    ui.deviceId.removeAttribute("title");
  }
  updateActionAvailability();
}

function clearDeviceState() {
  state.pdos.clear();
  state.expectedPdoCount = 0;
  state.sourceKind = null;
  state.sourceEprCapable = false;
  state.inEpr = false;
  ui.portState.textContent = state.connected ? "Connected" : "Offline";
  ui.portDetail.textContent = state.connected ? "Waiting for attachment" : "No serial connection";
  ui.contractVoltage.textContent = "—";
  ui.contractSupply.textContent = "No confirmed contract";
  ui.usableCurrent.textContent = "—";
  ui.currentConfidence.textContent = "Awaiting negotiation";
  ui.sourceKind.textContent = "—";
  ui.sourceSummary.textContent = "Not received";
  renderCapabilities();
  updateActionAvailability();
}

function formatUsbId(value) {
  return value === undefined ? "----" : value.toString(16).padStart(4, "0").toUpperCase();
}

function timeStamp(date = new Date()) {
  return date.toLocaleTimeString([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function addLog(text, direction = "incoming", severity = "normal") {
  const entry = { time: new Date(), text, direction, severity };
  state.log.push(entry);
  if (state.log.length > MAX_LOG_LINES) {
    state.log.splice(0, state.log.length - MAX_LOG_LINES);
    while (ui.terminal.childElementCount > state.log.length) {
      ui.terminal.firstElementChild?.remove();
    }
  }

  const row = document.createElement("div");
  row.className = `terminal-line ${direction} ${severity}`;

  const time = document.createElement("span");
  time.className = "terminal-time";
  time.textContent = timeStamp(entry.time);

  const marker = document.createElement("span");
  marker.className = "terminal-direction";
  marker.textContent = direction === "outgoing" ? ">" : direction === "system" ? "•" : "<";

  const content = document.createElement("span");
  content.className = "terminal-text";
  content.textContent = text;

  row.append(time, marker, content);
  ui.terminal.append(row);
  if (ui.autoScroll.checked) {
    ui.terminal.scrollTop = ui.terminal.scrollHeight;
  }
}

function appendSerialText(chunk) {
  state.lineBuffer += state.textDecoder.decode(chunk, { stream: true }).replaceAll("\r", "");
  const lines = state.lineBuffer.split("\n");
  state.lineBuffer = lines.pop() ?? "";
  for (const line of lines) {
    if (line.length > 0) {
      parseDeviceLine(line);
    }
  }
}

function appendControlData(chunk) {
  const decoded = state.controlDecoder.push(chunk);
  if (decoded.errors > 0) {
    addLog(`Ignored ${decoded.errors} damaged USB-control frame${decoded.errors === 1 ? "" : "s"}.`, "system", "error");
  }
  for (const frame of decoded.frames) {
    try {
      for (const line of controlProtocol.translateFrame(frame)) parseDeviceLine(line);
    } catch (error) {
      addLog(`Could not decode USB-control event: ${error.message}`, "system", "error");
    }
  }
}

async function initializeTransport() {
  try {
    await sendCommand("device", { echo: false });
    await new Promise((resolve) => setTimeout(resolve, 40));
    await sendCommand("status", { echo: false });
    await new Promise((resolve) => setTimeout(resolve, 80));
    await sendCommand("caps", { echo: false });
  } catch (error) {
    if (state.connected) addLog(`Device initialization failed: ${error.message}`, "system", "error");
  }
}

function selectTransport(transport) {
  if (state.transport) return;
  state.transport = transport;
  ui.connectionLabel.textContent = "Connected";
  refreshDeviceDescription();
  updateActionAvailability();
  addLog(
    transport === "usb-control"
      ? "Using compact USB control; the browser translates typed device events."
      : "Using the development text console compatibility transport.",
    "system",
  );
  setTimeout(() => void initializeTransport(), 0);
}

function appendSerialData(chunk) {
  if (state.transport === "usb-control") {
    appendControlData(chunk);
    return;
  }
  if (state.transport === "dev-text-console") {
    appendSerialText(chunk);
    return;
  }

  state.detectionBuffer.push(...chunk);
  const bytes = state.detectionBuffer;
  let controlStart = -1;
  for (let index = 0; index + 2 < bytes.length; index += 1) {
    if (bytes[index] === 0x50 && bytes[index + 1] === 0x44 && bytes[index + 2] === controlProtocol.VERSION) {
      controlStart = index;
      break;
    }
  }
  if (controlStart >= 0) {
    selectTransport("usb-control");
    const buffered = Uint8Array.from(bytes.slice(controlStart));
    state.detectionBuffer.length = 0;
    appendControlData(buffered);
    return;
  }

  const newline = bytes.indexOf(0x0a);
  const printableLine = newline >= 0 && bytes.slice(0, newline).every((byte) => byte === 0x0d || (byte >= 0x20 && byte <= 0x7e));
  if (printableLine) {
    selectTransport("dev-text-console");
    const buffered = Uint8Array.from(bytes);
    state.detectionBuffer.length = 0;
    appendSerialText(buffered);
  } else if (bytes.length > 256) {
    state.detectionBuffer.splice(0, bytes.length - 2);
    addLog("Waiting for a valid USB-control frame or text-console line.", "system", "error");
  }
}

function displayMillivolts(value) {
  if (!Number.isFinite(value)) return "—";
  const volts = value / 1000;
  return `${volts.toLocaleString(undefined, { minimumFractionDigits: volts % 1 === 0 ? 0 : 1, maximumFractionDigits: 3 })} V`;
}

function displayMilliamps(value) {
  if (!Number.isFinite(value)) return "—";
  const amps = value / 1000;
  return `${amps.toLocaleString(undefined, { minimumFractionDigits: amps % 1 === 0 ? 0 : 1, maximumFractionDigits: 3 })} A`;
}

function parsePdoLine(line) {
  const header = line.match(/^PDO(\d+)\s+(\S+)\s*(.*)$/);
  if (!header) return null;

  const position = Number(header[1]);
  const type = header[2];
  const detail = header[3];
  const fixedVoltage = detail.match(/^(-?\d+)mV\b/);
  const rangeVoltage = detail.match(/(\d+)-(\d+)mV\b/);
  const currentParts = [...detail.matchAll(/(\d+)mA(?:@(?:15V|20V))?/g)].map((match) => match[0]);
  const power = detail.match(/(?:PDP=)?(\d+)mW\b/);
  const validity = detail.match(/\b(valid|compatible|padding|malformed|unsupported)\s+raw=/)?.[1]
    ?? (type === "padding" ? "padding" : type === "unsupported" ? "unsupported" : "unknown");
  const raw = detail.match(/raw=(0x[0-9a-fA-F]+)/)?.[1] ?? "—";

  let voltage = "—";
  let maxMillivolts = null;
  if (rangeVoltage) {
    voltage = `${displayMillivolts(Number(rangeVoltage[1]))} – ${displayMillivolts(Number(rangeVoltage[2]))}`;
    maxMillivolts = Number(rangeVoltage[2]);
    if (type === "EPR-AVS" && validity === "compatible" && Number(rangeVoltage[1]) < 15000) {
      voltage += " (15 V+ standard)";
    }
  } else if (fixedVoltage) {
    voltage = displayMillivolts(Number(fixedVoltage[1]));
    maxMillivolts = Number(fixedVoltage[1]);
  }

  const capacity = currentParts.length > 0
    ? currentParts.join(" · ")
    : power
      ? `${(Number(power[1]) / 1000).toLocaleString()} W`
      : "—";

  return {
    position,
    type,
    voltage,
    maxMillivolts,
    capacity,
    validity,
    raw,
    requestable: validity === "valid" || validity === "compatible",
    line,
  };
}

function createCell(text, className = "") {
  const cell = document.createElement("td");
  cell.textContent = text;
  if (className) cell.className = className;
  return cell;
}

function renderCapabilities() {
  ui.capabilityBody.replaceChildren();
  const pdos = [...state.pdos.values()].sort((a, b) => a.position - b.position);
  const total = state.expectedPdoCount || pdos.length;
  ui.capabilityCount.textContent = total === 0 ? "No PDOs" : `${pdos.length} of ${total} PDO${total === 1 ? "" : "s"}`;

  if (pdos.length === 0) {
    const row = document.createElement("tr");
    row.className = "empty-row";
    const cell = createCell("Connect and refresh capabilities to populate this table.");
    cell.colSpan = 7;
    row.append(cell);
    ui.capabilityBody.append(row);
    return;
  }

  for (const pdo of pdos) {
    const row = document.createElement("tr");
    row.append(
      createCell(String(pdo.position), "pdo-index"),
      createCell(pdo.type),
      createCell(pdo.voltage),
      createCell(pdo.capacity),
    );

    const validityCell = document.createElement("td");
    const validity = document.createElement("span");
    validity.className = `validity validity-${pdo.validity}`;
    validity.textContent = pdo.validity === "compatible" ? "compatible / non-standard" : pdo.validity;
    validityCell.append(validity);
    row.append(validityCell, createCell(pdo.raw, "raw-value"));

    const actionCell = document.createElement("td");
    const action = document.createElement("button");
    action.className = "button button-quiet";
    action.type = "button";
    action.textContent = "Request max";
    const withinBoardCeiling = Number.isFinite(pdo.maxMillivolts)
      && pdo.maxMillivolts <= state.maxVoltageMillivolts;
    action.disabled = !state.connected || !pdo.requestable || !withinBoardCeiling;
    if (pdo.requestable && !withinBoardCeiling) {
      action.title = `This offer exceeds the board's ${displayMillivolts(state.maxVoltageMillivolts)} GUI ceiling.`;
    }
    action.addEventListener("click", async () => {
      ui.pdoPosition.value = String(pdo.position);
      try {
        await sendCommand(`pdo ${pdo.position} max`);
        setFeedback(`Requested the maximum plan for PDO ${pdo.position}.`);
      } catch (error) {
        setFeedback(error.message, true);
      }
    });
    actionCell.append(action);
    row.append(actionCell);
    ui.capabilityBody.append(row);
  }
}

function parseDeviceLine(line) {
  const isError = /^(Rejected|Invalid|Busy|PD stopped|Hard reset)/.test(line);
  addLog(line, "incoming", isError ? "error" : "normal");

  const identity = line.match(/^Device id=([0-9a-f]{16}|[0-9a-f]{24}|unavailable)$/i);
  if (identity) {
    applyDeviceIdentity(identity[1]);
    return;
  }

  if (/^Attached\b/.test(line)) {
    ui.portState.textContent = "Attached";
    ui.portDetail.textContent = "PD negotiation active";
  } else if (/^Detached\b/.test(line)) {
    state.inEpr = false;
    state.sourceEprCapable = false;
    ui.portState.textContent = "Detached";
    ui.portDetail.textContent = "Waiting for source";
    ui.contractVoltage.textContent = "—";
    ui.contractSupply.textContent = "No confirmed contract";
    ui.usableCurrent.textContent = "—";
    ui.currentConfidence.textContent = "Contract cleared";
    updateActionAvailability();
  } else if (/^(Hard reset|Protocol lost|PD stopped)/.test(line)) {
    state.inEpr = false;
    state.sourceEprCapable = false;
    ui.portState.textContent = "Recovering";
    ui.portDetail.textContent = line;
    ui.contractVoltage.textContent = "—";
    ui.contractSupply.textContent = "Contract lost";
    updateActionAvailability();
  } else if (/^Requesting\b/.test(line)) {
    ui.portState.textContent = "Negotiating";
    ui.portDetail.textContent = "Waiting for Accept and PS_RDY";
  } else if (/^Rejected\b/.test(line)) {
    setFeedback(line, true);
  }

  const source = line.match(/^Source caps: kind=(SPR|EPR) count=(\d+) EPR=(true|false)$/);
  if (source) {
    state.sourceKind = source[1];
    state.sourceEprCapable = source[3] === "true";
    state.inEpr = source[1] === "EPR";
    state.expectedPdoCount = Number(source[2]);
    state.pdos.clear();
    ui.sourceKind.textContent = source[1];
    ui.sourceSummary.textContent = `${source[2]} advertised objects · EPR ${source[3] === "true" ? "capable" : "unavailable"}`;
    renderCapabilities();
    updateActionAvailability();
    return;
  }

  const pdo = parsePdoLine(line);
  if (pdo) {
    state.pdos.set(pdo.position, pdo);
    renderCapabilities();
    return;
  }

  const contract = line.match(/^Contract ready PDO(\d+)\s+(.+)$/);
  if (contract) {
    const position = Number(contract[1]);
    const detail = contract[2];
    const fixed = detail.match(/fixed=(\d+)mV/);
    const adjustable = detail.match(/encoded=(\d+)mV/);
    const pdoType = state.pdos.get(position)?.type ?? "PDO";
    const voltage = Number((fixed ?? adjustable)?.[1]);
    state.inEpr = /EPR=1/.test(detail);
    ui.portState.textContent = "Ready";
    ui.portDetail.textContent = `Explicit contract on PDO ${position}`;
    ui.contractVoltage.textContent = displayMillivolts(voltage);
    ui.contractSupply.textContent = `${pdoType} · PDO ${position}${/EPR=1/.test(detail) ? " · EPR" : ""}`;
    updateActionAvailability();
    return;
  }

  const current = line.match(
    /^Contract ready req=(max|\d+mA) src=(\d+)mA usable=(\d+)mA confidence=([^\s]+) limit=([^\s]+) mismatch=(true|false)$/,
  );
  if (current) {
    ui.usableCurrent.textContent = displayMilliamps(Number(current[3]));
    ui.currentConfidence.textContent = `${current[4]} · limited by ${current[5]}${current[6] === "true" ? " · mismatch" : ""}`;
    return;
  }

  const sourceInfo = line.match(/^Source_Info: present=(\d+) W, maximum=(\d+) W, reported=(\d+) W$/);
  if (sourceInfo) {
    ui.sourceSummary.textContent = `${state.expectedPdoCount || state.pdos.size} objects · ${sourceInfo[1]} W present PDP`;
  }
}

function commandRequestedVoltage(command) {
  const normalized = command.trim().toLowerCase();
  const voltageRequest = normalized.match(/^(?:request|voltage)\s+(\d+)/);
  if (voltageRequest) return Number(voltageRequest[1]);

  const pdoRequest = normalized.match(/^pdo\s+(\d+)\s*(max|current|adjust)?(?:\s+(\d+))?/);
  if (!pdoRequest) return null;
  const position = Number(pdoRequest[1]);
  const demand = pdoRequest[2] || "max";
  if (demand === "adjust") return Number(pdoRequest[3] || 0);
  const pdo = state.pdos.get(position);
  return Number.isFinite(pdo?.maxMillivolts) ? pdo.maxMillivolts : "unknown-pdo";
}

async function sendCommand(command, { echo = true } = {}) {
  const trimmed = command.trim();
  if (!state.connected || !state.writer) {
    throw new Error("Connect the device first.");
  }
  if (!state.transport) {
    throw new Error("Waiting for the device control protocol.");
  }
  const requestedVoltage = commandRequestedVoltage(trimmed);
  if (requestedVoltage === "unknown-pdo" && state.maxVoltageMillivolts < 48000) {
    throw new Error("Load the full capability list before making a direct PDO request under a limited board ceiling.");
  }
  if (Number.isFinite(requestedVoltage) && requestedVoltage > state.maxVoltageMillivolts) {
    throw new Error(
      `This request exceeds the board's ${displayMillivolts(state.maxVoltageMillivolts)} GUI ceiling.`,
    );
  }
  if (state.transport === "usb-control") {
    const sequence = state.nextSequence;
    state.nextSequence = sequence === 0xff ? 1 : sequence + 1;
    await state.writer.write(controlProtocol.encodeCommand(trimmed, sequence));
  } else {
    await state.writer.write(encoder.encode(`${trimmed}\n`));
  }
  if (echo) addLog(trimmed, "outgoing");
}

async function readSerial() {
  try {
    while (state.keepReading && state.port?.readable) {
      state.reader = state.port.readable.getReader();
      try {
        while (state.keepReading) {
          const { value, done } = await state.reader.read();
          if (done) break;
          if (value) appendSerialData(value);
        }
      } finally {
        state.reader.releaseLock();
        state.reader = null;
      }
    }
  } catch (error) {
    if (state.keepReading) {
      addLog(`Serial connection lost: ${error.message}`, "system", "error");
    }
  } finally {
    if (state.keepReading) {
      state.keepReading = false;
      state.writer?.releaseLock();
      state.writer = null;
      state.port = null;
      setConnected(false);
      clearDeviceState();
    }
  }
}

async function connectPort() {
  if (state.connected) {
    await disconnectPort();
    return;
  }

  try {
    const port = await navigator.serial.requestPort();
    await port.open({ baudRate: 115200, bufferSize: 512 });
    state.port = port;
    state.writer = port.writable.getWriter();
    state.keepReading = true;
    state.transport = null;
    state.detectionBuffer.length = 0;
    state.controlDecoder.reset();
    state.textDecoder = new TextDecoder();
    state.nextSequence = 1;
    state.lineBuffer = "";
    state.deviceUid = null;
    setConnected(true);
    clearDeviceState();

    const info = port.getInfo();
    const id = `VID ${formatUsbId(info.usbVendorId)} · PID ${formatUsbId(info.usbProductId)}`;
    state.usbId = id;
    refreshDeviceDescription();
    ui.portState.textContent = "Connected";
    ui.portDetail.textContent = "Detecting compact control or text console";
    addLog(`Connected to ${id}`, "system");
    state.readTask = readSerial();
  } catch (error) {
    if (error.name !== "NotFoundError") {
      addLog(`Connection failed: ${error.message}`, "system", "error");
      setFeedback(error.message, true);
    }
    state.keepReading = false;
    try {
      await state.reader?.cancel();
    } catch (_) {
      // A failed or removed USB device can reject cancellation.
    }
    try {
      await state.readTask;
    } catch (_) {
      // The read loop reports its own serial errors.
    }
    state.writer?.releaseLock();
    state.writer = null;
    try {
      await state.port?.close();
    } catch (_) {
      // The browser may already have closed a failed port.
    }
    state.port = null;
    state.readTask = null;
    setConnected(false);
    clearDeviceState();
  }
}

async function disconnectPort() {
  state.keepReading = false;
  try {
    await state.reader?.cancel();
  } catch (_) {
    // A disconnected USB device can reject cancellation.
  }
  try {
    await state.readTask;
  } catch (_) {
    // The read loop reports its own serial errors.
  }
  state.writer?.releaseLock();
  state.writer = null;
  try {
    await state.port?.close();
  } catch (_) {
    // Physical removal can close the port before this path runs.
  }
  state.port = null;
  state.readTask = null;
  addLog("Disconnected", "system");
  setConnected(false);
  clearDeviceState();
}

function toMilli(value, label, { optional = false, maximum } = {}) {
  const text = String(value).trim();
  if (optional && text === "") return null;
  const parsed = Number(text);
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`${label} must be a positive number.`);
  if (maximum !== undefined && parsed > maximum) throw new Error(`${label} must not exceed ${maximum}.`);
  return Math.round(parsed * 1000);
}

function updatePdoFields() {
  const demand = ui.pdoDemand.value;
  ui.pdoVoltageField.classList.toggle("hidden", demand !== "adjust");
  ui.pdoCurrentField.classList.toggle("hidden", demand === "max");
}

ui.connectButton.addEventListener("click", connectPort);

ui.safeFiveButton.addEventListener("click", async () => {
  try {
    await sendCommand("request 5000 max fixed");
    setFeedback("Safe 5 V request queued.");
  } catch (error) {
    setFeedback(error.message, true);
  }
});

for (const input of ui.voltageCeilingInputs) {
  input.addEventListener("change", () => {
    if (!input.checked) return;
    setVoltageCeiling(Number(input.value), { persist: true });
    const scope = state.deviceUid ? ` for ${displayBoardId(state.deviceUid)}` : " for this session";
    setFeedback(`GUI request ceiling set to ${displayMillivolts(state.maxVoltageMillivolts)}${scope}.`);
  });
}

ui.voltageForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const voltage = toMilli(ui.voltageInput.value, "Voltage", { maximum: 48 });
    const current = toMilli(ui.currentInput.value, "Current", { optional: true, maximum: 5 });
    const preference = ui.preferenceInput.value;
    await sendCommand(`request ${voltage} ${current === null ? "max" : current} ${preference}`);
    setFeedback(`Requested ${displayMillivolts(voltage)}${current === null ? " at the available current" : ` with ${displayMilliamps(current)} requested`}.`);
  } catch (error) {
    setFeedback(error.message, true);
  }
});

document.querySelectorAll(".command-button").forEach((button) => {
  button.addEventListener("click", async () => {
    try {
      await sendCommand(button.dataset.command);
      setFeedback(`${button.textContent} queued.`);
    } catch (error) {
      setFeedback(error.message, true);
    }
  });
});

ui.pdoDemand.addEventListener("change", updatePdoFields);
ui.pdoForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const position = Number(ui.pdoPosition.value);
    if (!Number.isInteger(position) || position < 1 || position > 11) throw new Error("PDO position must be 1 through 11.");
    const demand = ui.pdoDemand.value;
    let command = `pdo ${position} max`;
    if (demand === "current") {
      const current = toMilli(ui.pdoCurrent.value, "Current", { maximum: 5 });
      command = `pdo ${position} current ${current}`;
    } else if (demand === "adjust") {
      const voltage = toMilli(ui.pdoVoltage.value, "Voltage", { maximum: 48 });
      const current = toMilli(ui.pdoCurrent.value, "Current", { optional: true, maximum: 5 });
      command = `pdo ${position} adjust ${voltage} ${current === null ? "max" : current}`;
    }
    await sendCommand(command);
    setFeedback(`Direct request for PDO ${position} queued.`);
  } catch (error) {
    setFeedback(error.message, true);
  }
});

ui.rawCommandForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const command = ui.rawCommand.value.trim();
  if (!command) return;
  try {
    await sendCommand(command);
    ui.rawCommand.value = "";
  } catch (error) {
    setFeedback(error.message, true);
  }
});

ui.clearLog.addEventListener("click", () => {
  state.log.length = 0;
  ui.terminal.replaceChildren();
});

ui.copyLog.addEventListener("click", async () => {
  const text = state.log.map((entry) => `${timeStamp(entry.time)} ${entry.direction === "outgoing" ? ">" : entry.direction === "system" ? "•" : "<"} ${entry.text}`).join("\n");
  try {
    await navigator.clipboard.writeText(text);
    setFeedback("Console copied to the clipboard.");
  } catch (error) {
    setFeedback(`Could not copy the console: ${error.message}`, true);
  }
});

ui.saveLog.addEventListener("click", () => {
  const text = state.log.map((entry) => `${entry.time.toISOString()} ${entry.direction === "outgoing" ? ">" : entry.direction === "system" ? "•" : "<"} ${entry.text}`).join("\n");
  const blob = new Blob([`${text}\n`], { type: "text/plain;charset=utf-8" });
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = `usb-pd-session-${new Date().toISOString().replaceAll(":", "-")}.log`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 0);
});

if (!("serial" in navigator)) {
  ui.browserNotice.classList.remove("hidden");
  ui.connectButton.disabled = true;
} else {
  navigator.serial.addEventListener("disconnect", (event) => {
    if (event.target === state.port && state.connected) {
      state.keepReading = false;
      state.writer?.releaseLock();
      state.writer = null;
      state.port = null;
      setConnected(false);
      clearDeviceState();
      addLog("USB device removed", "system", "error");
    }
  });
}

setConnected(false);
clearDeviceState();
updatePdoFields();
updateActionAvailability();
addLog("USB PD Control is ready. Connect a USB-control or development text-console device to begin.", "system");
