"use strict";

const $ = (selector) => document.querySelector(selector);
const controlProtocol = globalThis.PdControlProtocol;
const blackBoxProtocol = globalThis.PdBlackBox;

const ui = {
  connectButton: $("#connect-button"),
  connectionDot: $("#connection-dot"),
  connectionLabel: $("#connection-label"),
  browserNotice: $("#browser-notice"),
  deviceDescription: $("#device-description"),
  deviceId: $("#device-id"),
  autoReconnect: $("#auto-reconnect"),
  safeFiveButton: $("#safe-five-button"),
  portState: $("#port-state"),
  portDetail: $("#port-detail"),
  contractVoltage: $("#contract-voltage"),
  contractSupply: $("#contract-supply"),
  usableCurrent: $("#usable-current"),
  currentConfidence: $("#current-confidence"),
  sourceKind: $("#source-kind"),
  sourceSummary: $("#source-summary"),
  livePps: $("#live-pps"),
  ppsModeIndicator: $("#pps-mode-indicator"),
  ppsMode: $("#pps-mode"),
  ppsModeDetail: $("#pps-mode-detail"),
  ppsMeasurement: $("#pps-measurement"),
  sourceTemperature: $("#source-temperature"),
  sourceInput: $("#source-input"),
  sourceEvents: $("#source-events"),
  sourceLimits: $("#source-limits"),
  telemetryNote: $("#telemetry-note"),
  telemetryToggle: $("#telemetry-toggle"),
  telemetryAvailability: $("#telemetry-availability"),
  ppsMaintenanceState: $("#pps-maintenance-state"),
  telemetryChartShell: $("#telemetry-chart-shell"),
  telemetryChart: $("#telemetry-chart"),
  voltageChartLabel: $("#voltage-chart-label"),
  currentChartLabel: $("#current-chart-label"),
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
  plansButton: $("#plans-button"),
  inspectBlackBox: $("#inspect-black-box"),
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
  condenseLog: $("#condense-log"),
  rawLog: $("#raw-log"),
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
  connectionApi: null,
  usbClaimedInterfaces: [],
  usbInEndpoint: null,
  usbOutEndpoint: null,
  usbPacketSize: 64,
  transport: null,
  detectionBuffer: [],
  controlDecoder: new controlProtocol.FrameDecoder(),
  textDecoder: new TextDecoder(),
  nextSequence: 1,
  lineBuffer: "",
  log: [],
  rawLog: [],
  nextLogSequence: 1,
  pdos: new Map(),
  expectedPdoCount: 0,
  sourceKind: null,
  sourceEprCapable: false,
  inEpr: false,
  contractPosition: null,
  contractSupplyType: null,
  contractEpr: false,
  ppsRefreshTimer: null,
  ppsQueryPending: false,
  ppsPollingPausedUntil: 0,
  condenseConsole: true,
  rawStream: false,
  collapsedConsoleRows: new Map(),
  usbId: null,
  deviceUid: null,
  deviceMaxVoltageMillivolts: null,
  richTelemetrySupported: null,
  maxVoltageMillivolts: 5000,
  telemetrySamples: [],
  telemetryAvailable: { voltage: false, current: false },
  blackBoxPending: null,
  blackBoxBusy: false,
  reconnectTimer: null,
  reconnectAttempt: 0,
  reconnectInFlight: false,
  manualDisconnect: false,
};

const encoder = new TextEncoder();
const MAX_LOG_LINES = 1500;
const MAX_RAW_LOG_LINES = 1000;
const BOARD_SETTING_PREFIX = "usb-pd-control.board.";
const AUTO_RECONNECT_ENABLED_KEY = "usb-pd-control.auto-reconnect.enabled";
const AUTO_RECONNECT_TARGET_KEY = "usb-pd-control.auto-reconnect.target";
const ALLOWED_VOLTAGE_CEILINGS = new Set(controlProtocol.VOLTAGE_CEILINGS);
const DEVELOPMENT_USB_FILTERS = Object.freeze([
  Object.freeze({ vendorId: 0x1a86, productId: 0xfe0c }),
]);
const IS_ANDROID = /Android/i.test(navigator.userAgent);

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

function displayStatusToken(value) {
  return value.replaceAll("-", " ");
}

function displayStatusList(value) {
  return value === "none"
    ? "None"
    : value.split(",").map(displayStatusToken).join(", ");
}

function setPpsMode(mode, detail) {
  ui.ppsModeIndicator.classList.toggle("is-cv", mode === "CV");
  ui.ppsModeIndicator.classList.toggle("is-cl", mode === "CL");
  ui.ppsMode.textContent = mode === "CV"
    ? "CV reported"
    : mode === "CL"
      ? "Current limit reported"
      : "Unknown";
  ui.ppsModeDetail.textContent = detail;
}

function clearSourceTelemetry() {
  setPpsMode(null, "No PPS status received");
  ui.ppsMeasurement.textContent = "—";
  ui.sourceTemperature.textContent = "—";
  ui.sourceInput.textContent = "No general status received";
  ui.sourceEvents.textContent = "None reported";
  ui.sourceLimits.textContent = "No active limits reported";
  ui.ppsMaintenanceState.textContent = "PPS contract maintenance: waiting for a PPS contract";
  state.telemetrySamples.length = 0;
  state.telemetryAvailable = { voltage: false, current: false };
  drawTelemetryChart();
  ui.telemetryNote.textContent = state.richTelemetrySupported === false
    ? "This firmware keeps control, contract, and safety events but omits capability and live source telemetry to save flash."
    : "A compliant PPS source sends an Alert when it changes between CV and CL; the firmware then reads general Status automatically. Optional polling helps with sources that do not send that Alert.";
}

function latestTelemetryValue(key) {
  for (let index = state.telemetrySamples.length - 1; index >= 0; index -= 1) {
    const value = state.telemetrySamples[index][key];
    if (Number.isFinite(value)) return value;
  }
  return null;
}

function drawTelemetryChart() {
  const canvas = ui.telemetryChart;
  const context = canvas.getContext("2d");
  if (!context) return;
  const ratio = Math.max(1, window.devicePixelRatio || 1);
  const width = Math.max(260, Math.floor(canvas.clientWidth || canvas.parentElement?.clientWidth || 520));
  const height = 58;
  if (canvas.width !== Math.floor(width * ratio) || canvas.height !== Math.floor(height * ratio)) {
    canvas.width = Math.floor(width * ratio);
    canvas.height = Math.floor(height * ratio);
  }
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  context.clearRect(0, 0, width, height);
  context.strokeStyle = "#253038";
  context.lineWidth = 1;
  for (const y of [14.5, 29.5, 44.5]) {
    context.beginPath();
    context.moveTo(0, y);
    context.lineTo(width, y);
    context.stroke();
  }

  function drawSeries(key, color, available) {
    const values = state.telemetrySamples.map((sample) => sample[key]);
    const finite = values.filter(Number.isFinite);
    if (finite.length === 0) return;
    let minimum = Math.min(...finite);
    let maximum = Math.max(...finite);
    if (minimum === maximum) {
      const padding = Math.max(1, Math.abs(minimum) * 0.02);
      minimum -= padding;
      maximum += padding;
    }
    context.strokeStyle = available ? color : "#556168";
    context.lineWidth = 1.4;
    context.beginPath();
    let drawing = false;
    values.forEach((value, index) => {
      if (!Number.isFinite(value)) {
        drawing = false;
        return;
      }
      const x = values.length <= 1 ? width - 1 : (index / (values.length - 1)) * (width - 1);
      const y = 52 - ((value - minimum) / (maximum - minimum)) * 46;
      if (!drawing) context.moveTo(x, y);
      else context.lineTo(x, y);
      drawing = true;
    });
    context.stroke();
  }

  drawSeries("voltage", "#5fc9e8", state.telemetryAvailable.voltage);
  drawSeries("current", "#52d391", state.telemetryAvailable.current);

  const voltage = latestTelemetryValue("voltage");
  const current = latestTelemetryValue("current");
  ui.voltageChartLabel.textContent = state.telemetryAvailable.voltage && voltage !== null
    ? `Voltage ${displayMillivolts(voltage)}`
    : "Voltage unavailable";
  ui.currentChartLabel.textContent = state.telemetryAvailable.current && current !== null
    ? `Current ${displayMilliamps(current)}`
    : "Current unavailable";
  ui.voltageChartLabel.classList.toggle("is-unavailable", !state.telemetryAvailable.voltage);
  ui.currentChartLabel.classList.toggle("is-unavailable", !state.telemetryAvailable.current);
  ui.telemetryChartShell.classList.toggle(
    "is-unavailable",
    !state.telemetryAvailable.voltage && !state.telemetryAvailable.current,
  );
  const availability = [
    state.telemetryAvailable.voltage ? `voltage ${displayMillivolts(voltage)}` : "voltage unavailable",
    state.telemetryAvailable.current ? `current ${displayMilliamps(current)}` : "current unavailable",
  ].join("; ");
  canvas.setAttribute("aria-label", `Source-reported PPS history: ${availability}.`);
}

function recordTelemetrySample(voltage, current) {
  state.telemetryAvailable = {
    voltage: Number.isFinite(voltage),
    current: Number.isFinite(current),
  };
  state.telemetrySamples.push({ time: Date.now(), voltage, current });
  if (state.telemetrySamples.length > 60) state.telemetrySamples.shift();
  drawTelemetryChart();
}

function refreshTelemetryAvailability() {
  let message;
  if (!state.connected) message = "Connect a device to enable PPS telemetry.";
  else if (state.transport === null) message = "Waiting for the device control protocol.";
  else if (state.richTelemetrySupported === false) message = "This firmware profile omits rich source telemetry.";
  else if (state.contractSupplyType !== "PPS") message = "Available after a PPS contract is active.";
  else if (state.ppsRefreshTimer !== null) message = "Polling source-reported PPS voltage and current once per second.";
  else message = "Ready to poll source-reported PPS voltage and current once per second.";
  ui.telemetryAvailability.textContent = message;
  ui.telemetryToggle.title = message;
}

function transportLabel() {
  if (state.transport === "usb-control") return "Compact USB control";
  if (state.transport === "dev-text-console") return "Development text console";
  return "USB CDC detecting protocol";
}

function connectionApiLabel() {
  if (state.connectionApi === "web-serial") return "Web Serial";
  if (state.connectionApi === "webusb-cdc") return "WebUSB CDC";
  return null;
}

function refreshDeviceDescription() {
  const board = state.deviceUid ? displayBoardId(state.deviceUid) : null;
  const id = [board, state.usbId].filter(Boolean).join(" · ") || "device identity pending";
  const connection = connectionApiLabel();
  ui.deviceDescription.textContent = `${transportLabel()}${connection ? ` over ${connection}` : ""} · ${id}`;
  ui.deviceId.textContent = id;
}

function setVoltageCeiling(value, { persist = false } = {}) {
  const deviceMaximum = state.deviceMaxVoltageMillivolts ?? 48000;
  const ceiling = controlProtocol.boundedVoltageCeiling(value, deviceMaximum);
  state.maxVoltageMillivolts = ceiling;
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

async function pollPpsStatus() {
  if (
    state.ppsQueryPending
    || !state.connected
    || state.transport === null
    || state.richTelemetrySupported === false
    || state.contractSupplyType !== "PPS"
    || Date.now() < state.ppsPollingPausedUntil
  ) return;

  state.ppsQueryPending = true;
  try {
    await sendCommand("pps-status", { echo: false });
  } catch (error) {
    state.ppsQueryPending = false;
    if (state.connected) addLog(`Live PPS refresh stopped: ${error.message}`, "system", "error");
  }
}

function configurePpsPolling() {
  if (state.ppsRefreshTimer !== null) {
    clearInterval(state.ppsRefreshTimer);
    state.ppsRefreshTimer = null;
  }
  const running = ui.livePps.checked
    && state.connected
    && state.transport !== null
    && state.richTelemetrySupported !== false
    && state.contractSupplyType === "PPS";
  ui.telemetryToggle.textContent = running ? "Stop PPS telemetry" : "Start PPS telemetry";
  ui.telemetryToggle.setAttribute("aria-pressed", String(running));
  refreshTelemetryAvailability();
  if (!running) return;
  void pollPpsStatus();
  state.ppsRefreshTimer = setInterval(() => void pollPpsStatus(), 1000);
}

function updateActionAvailability() {
  const ready = state.connected && state.transport !== null;
  const deviceMaximum = state.deviceMaxVoltageMillivolts ?? 48000;
  for (const input of ui.voltageCeilingInputs) {
    input.disabled = !ready || Number(input.value) > deviceMaximum;
  }
  ui.safeFiveButton.disabled = !ready;
  document.querySelectorAll(".command-button, .request-submit").forEach((button) => {
    button.disabled = !ready;
  });
  const richTelemetry = state.richTelemetrySupported !== false;
  document.querySelectorAll('[data-command="caps"], [data-command="plans"], [data-command="source-info"], [data-command="source-status"], [data-command="pps-status"]').forEach((button) => {
    button.disabled = !ready || !richTelemetry;
  });
  ui.rawCommand.disabled = !ready;
  ui.rawCommandForm.querySelector("button").disabled = !ready;
  ui.enterEprButton.disabled = !ready || state.inEpr || !state.sourceEprCapable;
  ui.eprCapsButton.disabled = !ready || !state.inEpr;
  ui.exitEprButton.disabled = !ready || !state.inEpr;
  ui.plansButton.disabled = !ready || state.transport === "dev-text-console" || !richTelemetry;
  ui.plansButton.title = !richTelemetry
    ? "This firmware omits rich telemetry to save flash."
    : state.transport === "dev-text-console"
      ? "The text-console profile omits this flash-heavy preview; flash usb-epr to use it."
      : "";
  ui.livePps.disabled = !ready || !richTelemetry || state.contractSupplyType !== "PPS";
  ui.telemetryToggle.disabled = !ready || !richTelemetry || state.contractSupplyType !== "PPS";
  ui.inspectBlackBox.disabled = !ready || state.transport !== "usb-control" || state.blackBoxBusy;
  ui.inspectBlackBox.title = !ready
    ? "Connect a device first."
    : state.transport !== "usb-control"
      ? "Persistent black-box inspection requires a compact USB-control firmware profile."
      : "Reads the high-level or deep recorder selected by the attached firmware.";
  refreshTelemetryAvailability();
}

function setConnected(connected) {
  state.connected = connected;
  ui.connectionDot.classList.toggle("connected", connected);
  ui.connectionLabel.textContent = connected ? (state.transport ? "Connected" : "Detecting device") : "Disconnected";
  ui.connectButton.textContent = connected ? "Disconnect" : "Connect device";

  if (!connected) {
    cancelBlackBoxRead();
    state.connectionApi = null;
    state.usbClaimedInterfaces.length = 0;
    state.usbInEndpoint = null;
    state.usbOutEndpoint = null;
    state.usbPacketSize = 64;
    state.transport = null;
    state.detectionBuffer.length = 0;
    state.controlDecoder.reset();
    state.textDecoder = new TextDecoder();
    state.nextSequence = 1;
    state.usbId = null;
    state.deviceUid = null;
    state.deviceMaxVoltageMillivolts = null;
    state.richTelemetrySupported = null;
    state.ppsQueryPending = false;
    setVoltageCeiling(5000);
    ui.deviceDescription.textContent = "Connect USB-control or development text-console firmware to begin.";
    ui.deviceId.textContent = "No device selected";
    ui.deviceId.removeAttribute("title");
  }
  updateActionAvailability();
  configurePpsPolling();
}

function clearDeviceState() {
  state.ppsQueryPending = false;
  state.ppsPollingPausedUntil = 0;
  state.pdos.clear();
  state.expectedPdoCount = 0;
  state.sourceKind = null;
  state.sourceEprCapable = false;
  state.inEpr = false;
  state.contractPosition = null;
  state.contractSupplyType = null;
  state.contractEpr = false;
  ui.portState.textContent = state.connected ? "Connected" : "Offline";
  ui.portDetail.textContent = state.connected ? "Waiting for attachment" : "No USB connection";
  ui.contractVoltage.textContent = "—";
  ui.contractSupply.textContent = "No confirmed contract";
  ui.usableCurrent.textContent = "—";
  ui.currentConfidence.textContent = "Awaiting negotiation";
  ui.sourceKind.textContent = "—";
  ui.sourceSummary.textContent = "Not received";
  clearSourceTelemetry();
  renderCapabilities();
  updateActionAvailability();
  configurePpsPolling();
}

function formatUsbId(value) {
  return value === undefined ? "----" : value.toString(16).padStart(4, "0").toUpperCase();
}

function readAutoReconnectPreference() {
  try {
    return localStorage.getItem(AUTO_RECONNECT_ENABLED_KEY) !== "false";
  } catch (_) {
    return true;
  }
}

function savedAutoReconnectTarget() {
  try {
    const target = JSON.parse(localStorage.getItem(AUTO_RECONNECT_TARGET_KEY) || "null");
    return target && typeof target.api === "string" ? target : null;
  } catch (_) {
    return null;
  }
}

function rememberAutoReconnectTarget(api, vendorId, productId) {
  if (!ui.autoReconnect.checked) return;
  try {
    localStorage.setItem(AUTO_RECONNECT_TARGET_KEY, JSON.stringify({ api, vendorId, productId }));
  } catch (_) {
    // Browser storage can be unavailable for local files; event-based reconnect still works.
  }
}

function forgetAutoReconnectTarget() {
  try {
    localStorage.removeItem(AUTO_RECONNECT_TARGET_KEY);
  } catch (_) {
    // Nothing else to clear when browser storage is unavailable.
  }
}

function cancelReconnectTimer() {
  if (state.reconnectTimer !== null) clearTimeout(state.reconnectTimer);
  state.reconnectTimer = null;
}

function timeStamp(date = new Date()) {
  return date.toLocaleTimeString([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function logMarker(direction) {
  return direction === "outgoing" ? ">" : direction === "system" ? "•" : "<";
}

function formatLogEntry(entry, timestamp, updateCount = 1) {
  const suffix = updateCount > 1 ? ` ×${updateCount}` : "";
  return `${timestamp} ${logMarker(entry.direction)} ${entry.text}${suffix}`;
}

function visibleLogEntries() {
  if (!state.rawStream) return state.log;
  return [...state.log, ...state.rawLog].sort((left, right) => left.sequence - right.sequence);
}

function collapsedConsoleKey(entry) {
  if (entry.direction !== "incoming") return null;
  if (entry.text === "Queued") return "hidden";
  if (/^Contract refresh confirmed\b/.test(entry.text)) return "contract-refresh";
  if (/^PPS_Status:/.test(entry.text)) return "pps-status";
  if (/^Source_Status:/.test(entry.text)) return "source-status";
  if (/^PD Alert:/.test(entry.text)) return "source-alert";
  return null;
}

function createLogRow(entry, updateCount = 1) {
  const row = document.createElement("div");
  row.className = `terminal-line ${entry.direction} ${entry.severity}${entry.rawTransport ? " raw-transport" : ""}`;

  const time = document.createElement("span");
  time.className = "terminal-time";
  time.textContent = timeStamp(entry.time);

  const marker = document.createElement("span");
  marker.className = "terminal-direction";
  marker.textContent = logMarker(entry.direction);

  const content = document.createElement("span");
  content.className = "terminal-text";
  content.textContent = entry.text;
  if (updateCount > 1) {
    const count = document.createElement("span");
    count.className = "terminal-update-count";
    count.textContent = `×${updateCount}`;
    content.append(count);
  }

  row.append(time, marker, content);
  row.dataset.copyLine = formatLogEntry(entry, time.textContent, updateCount);
  return row;
}

function appendVisibleLog(entry) {
  if (entry.rawTransport && !state.rawStream) return;
  if (!state.condenseConsole || entry.rawTransport) {
    ui.terminal.append(createLogRow(entry));
    return;
  }

  const key = collapsedConsoleKey(entry);
  if (key === "hidden") return;
  if (key) {
    const previous = state.collapsedConsoleRows.get(key);
    const updateCount = (previous?.updateCount ?? 0) + 1;
    previous?.row.remove();
    const row = createLogRow(entry, updateCount);
    state.collapsedConsoleRows.set(key, { row, updateCount });
    ui.terminal.append(row);
    return;
  }
  ui.terminal.append(createLogRow(entry));
}

function renderConsole() {
  ui.terminal.replaceChildren();
  state.collapsedConsoleRows.clear();
  for (const entry of visibleLogEntries()) appendVisibleLog(entry);
  if (ui.autoScroll.checked) ui.terminal.scrollTop = ui.terminal.scrollHeight;
}

function addLog(text, direction = "incoming", severity = "normal", { rawTransport = false } = {}) {
  const entry = {
    time: new Date(),
    text,
    direction,
    severity,
    rawTransport,
    sequence: state.nextLogSequence++,
  };
  const target = rawTransport ? state.rawLog : state.log;
  const maximum = rawTransport ? MAX_RAW_LOG_LINES : MAX_LOG_LINES;
  target.push(entry);
  if (target.length > maximum) {
    target.splice(0, target.length - maximum);
    if (!rawTransport || state.rawStream) renderConsole();
  } else {
    appendVisibleLog(entry);
  }
  if (ui.autoScroll.checked) {
    ui.terminal.scrollTop = ui.terminal.scrollHeight;
  }
}

function addRawTransportLog(bytes, direction) {
  if (bytes.length === 0) return;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join(" ");
  const api = state.connectionApi === "webusb-cdc" ? "WebUSB" : "Serial";
  addLog(`RAW ${api} ${bytes.length} B · ${hex}`, direction, "normal", { rawTransport: true });
}

function littleEndianU32Bytes(value) {
  const raw = value >>> 0;
  return Uint8Array.from([
    raw & 0xff,
    (raw >>> 8) & 0xff,
    (raw >>> 16) & 0xff,
    (raw >>> 24) & 0xff,
  ]);
}

function translateDevelopmentLine(line) {
  let match = line.match(/^PDO(\d+) raw=(0x[0-9a-f]{8})$/i);
  if (match) {
    const kind = state.sourceKind === "EPR" ? 1 : 0;
    return controlProtocol.pdoLine(kind, Number(match[1]), Number.parseInt(match[2], 16));
  }

  match = line.match(/^PPS_Status: raw=(0x[0-9a-f]{8})$/i);
  if (match) {
    return controlProtocol.ppsStatusLine(littleEndianU32Bytes(Number.parseInt(match[1], 16)));
  }

  match = line.match(/^Status: pps=(true|false) raw=([0-9a-f]{14})$/i);
  if (match) {
    const raw = Uint8Array.from(match[2].match(/../g).map((byte) => Number.parseInt(byte, 16)));
    return controlProtocol.sourceStatusLine(raw, match[1].toLowerCase() === "true");
  }

  match = line.match(/^Alert: raw=(0x[0-9a-f]{8})$/i);
  if (match) {
    return controlProtocol.sourceAlertLine(littleEndianU32Bytes(Number.parseInt(match[1], 16)));
  }

  match = line.match(/^Status query failed: kind=(\d+) reason=(\d+)$/);
  if (match) {
    return controlProtocol.statusQueryFailureLine(Uint8Array.from([
      Number(match[1]),
      Number(match[2]),
    ]));
  }

  return line;
}

function appendSerialText(chunk) {
  state.lineBuffer += state.textDecoder.decode(chunk, { stream: true }).replaceAll("\r", "");
  const lines = state.lineBuffer.split("\n");
  state.lineBuffer = lines.pop() ?? "";
  for (const line of lines) {
    if (line.length > 0) {
      parseDeviceLine(translateDevelopmentLine(line));
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
    if (state.richTelemetrySupported !== false) {
      await sendCommand("caps", { echo: false });
      await new Promise((resolve) => setTimeout(resolve, 80));
      await sendCommand("source-status", { echo: false });
    }
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

function cancelBlackBoxRead(message = "Black-box read cancelled.") {
  const pending = state.blackBoxPending;
  if (!pending) return;
  clearTimeout(pending.timer);
  state.blackBoxPending = null;
  pending.reject(new Error(message));
}

function blackBoxPrefixLength(bytes) {
  const magic = [0x50, 0x44, 0x42, 0x42];
  for (let length = Math.min(3, bytes.length); length > 0; length -= 1) {
    const start = bytes.length - length;
    if (magic.slice(0, length).every((value, index) => bytes[start + index] === value)) return length;
  }
  return 0;
}

function consumeBlackBoxData(chunk) {
  const pending = state.blackBoxPending;
  if (!pending) return chunk;
  const bytes = [...pending.buffer, ...chunk];
  pending.buffer.length = 0;
  let start = -1;
  for (let index = 0; index + 3 < bytes.length; index += 1) {
    if (bytes[index] === 0x50 && bytes[index + 1] === 0x44 && bytes[index + 2] === 0x42 && bytes[index + 3] === 0x42) {
      start = index;
      break;
    }
  }
  if (start < 0) {
    const retained = blackBoxPrefixLength(bytes);
    if (retained) pending.buffer.push(...bytes.slice(-retained));
    return Uint8Array.from(bytes.slice(0, bytes.length - retained));
  }
  if (bytes.length - start < 6) {
    pending.buffer.push(...bytes.slice(start));
    return Uint8Array.from(bytes.slice(0, start));
  }
  const length = bytes[start + 5];
  if (length > 16) {
    cancelBlackBoxRead(`Invalid black-box response length ${length}.`);
    return Uint8Array.from(bytes);
  }
  const frameLength = 6 + length;
  if (bytes.length - start < frameLength) {
    pending.buffer.push(...bytes.slice(start));
    return Uint8Array.from(bytes.slice(0, start));
  }
  const response = blackBoxProtocol.parseResponse(Uint8Array.from(bytes.slice(start, start + frameLength)));
  clearTimeout(pending.timer);
  state.blackBoxPending = null;
  pending.resolve(response);
  return Uint8Array.from([...bytes.slice(0, start), ...bytes.slice(start + frameLength)]);
}

function requestBlackBoxPage(page) {
  if (state.blackBoxPending) return Promise.reject(new Error("A black-box page read is already in flight."));
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      if (state.blackBoxPending?.page !== page) return;
      state.blackBoxPending = null;
      reject(new Error(`Timed out reading black-box page ${page}. Flash a black-box firmware profile first.`));
    }, 2000);
    state.blackBoxPending = { page, buffer: [], resolve, reject, timer };
    writeTransport(Uint8Array.of(0x42, 0x42, page)).catch((error) => {
      if (state.blackBoxPending?.page !== page) return;
      clearTimeout(timer);
      state.blackBoxPending = null;
      reject(error);
    });
  });
}

async function inspectBlackBox() {
  if (state.transport !== "usb-control") throw new Error("Black-box inspection requires compact USB control.");
  state.blackBoxBusy = true;
  updateActionAvailability();
  addLog("Reading persistent black box…", "system");
  try {
    const summaryResponse = await requestBlackBoxPage(0);
    if (summaryResponse.kind !== 0xb0) throw new Error("The device returned an invalid black-box summary kind.");
    const summary = blackBoxProtocol.decodeSummary(summaryResponse.payload);
    const events = [];
    for (let index = 0; index < summary.eventCount; index += 1) {
      const response = await requestBlackBoxPage(index + 1);
      if (response.kind !== 0xb1) throw new Error(`The device returned an invalid event page ${index + 1}.`);
      events.push(blackBoxProtocol.decodeEvent(response.payload, index));
    }
    const flags = [
      summary.restored ? "restored" : null,
      summary.frozen ? "frozen" : null,
      summary.hardReset ? "hard reset captured" : null,
      summary.overwritten ? "old records overwritten" : null,
      summary.writeError ? "storage write error" : null,
    ].filter(Boolean);
    addLog(
      `Black box: ${summary.profile} profile; ${summary.eventCount} event${summary.eventCount === 1 ? "" : "s"}; generation ${summary.generation}${flags.length ? `; ${flags.join("; ")}` : ""}`,
      "system",
    );
    if (events.length === 0) addLog("Black box contains no recorded events.", "incoming");
    for (const event of events) {
      addLog(`Black box ${event.index + 1}/${events.length}: t=${event.uptime}ms ${event.text}`, "incoming");
    }
    setFeedback(`Read the ${summary.profile} black box.`);
  } catch (error) {
    addLog(`Black-box inspection failed: ${error.message}`, "system", "error");
    setFeedback(error.message, true);
  } finally {
    state.blackBoxBusy = false;
    updateActionAvailability();
  }
}

function appendSerialData(chunk) {
  addRawTransportLog(chunk, "incoming");
  const applicationData = consumeBlackBoxData(chunk);
  if (applicationData.length === 0) return;
  if (state.transport === "usb-control") {
    appendControlData(applicationData);
    return;
  }
  if (state.transport === "dev-text-console") {
    appendSerialText(applicationData);
    return;
  }

  state.detectionBuffer.push(...applicationData);
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

function refreshContractSupply() {
  if (state.contractPosition === null) return;
  const pdoType = state.pdos.get(state.contractPosition)?.type;
  if (pdoType) state.contractSupplyType = pdoType;
  const label = state.contractSupplyType ?? "PDO";
  ui.contractSupply.textContent = `${label} · PDO ${state.contractPosition}${state.contractEpr ? " · EPR" : ""}`;
  updateActionAvailability();
  configurePpsPolling();
}

function parseDeviceLine(line) {
  const isError = /^(Rejected|Invalid|Busy|PD stopped|Hard reset|.+ query failed:)/.test(line);
  addLog(line, "incoming", isError ? "error" : "normal");
  if (/^Busy\b/.test(line)) state.ppsQueryPending = false;

  const identity = line.match(/^Device id=([0-9a-f]{16}|[0-9a-f]{24}|unavailable)$/i);
  if (identity) {
    applyDeviceIdentity(identity[1]);
    return;
  }

  const deviceLimits = line.match(/^Device limits: max=(\d+)mV current=(\d+)mA power=(\d+)mW PPS=(true|false) EPR=(true|false)(?: TELEMETRY=(rich|basic))?$/);
  if (deviceLimits) {
    state.deviceMaxVoltageMillivolts = Number(deviceLimits[1]);
    state.richTelemetrySupported = deviceLimits[6] !== "basic";
    if (!state.richTelemetrySupported) {
      ui.telemetryNote.textContent = "This firmware keeps control, contract, and safety events but omits capability and live source telemetry to save flash.";
    }
    const requestedCeiling = state.deviceUid ? savedVoltageCeiling(state.deviceUid) : state.maxVoltageMillivolts;
    setVoltageCeiling(requestedCeiling);
    updateActionAvailability();
    renderCapabilities();
    return;
  }

  if (/^Attached\b/.test(line)) {
    state.ppsQueryPending = false;
    state.contractPosition = null;
    state.contractSupplyType = null;
    state.contractEpr = false;
    clearSourceTelemetry();
    ui.portState.textContent = "Attached";
    ui.portDetail.textContent = "PD negotiation active";
    configurePpsPolling();
  } else if (/^Detached\b/.test(line)) {
    state.ppsQueryPending = false;
    state.inEpr = false;
    state.sourceEprCapable = false;
    state.contractPosition = null;
    state.contractSupplyType = null;
    state.contractEpr = false;
    clearSourceTelemetry();
    ui.portState.textContent = "Detached";
    ui.portDetail.textContent = "Waiting for source";
    ui.contractVoltage.textContent = "—";
    ui.contractSupply.textContent = "No confirmed contract";
    ui.usableCurrent.textContent = "—";
    ui.currentConfidence.textContent = "Contract cleared";
    updateActionAvailability();
    configurePpsPolling();
  } else if (/^(Hard reset|Protocol lost|PD stopped)/.test(line)) {
    state.ppsQueryPending = false;
    state.inEpr = false;
    state.sourceEprCapable = false;
    state.contractPosition = null;
    state.contractSupplyType = null;
    state.contractEpr = false;
    clearSourceTelemetry();
    ui.portState.textContent = "Recovering";
    ui.portDetail.textContent = line;
    ui.contractVoltage.textContent = "—";
    ui.contractSupply.textContent = "Contract lost";
    updateActionAvailability();
    configurePpsPolling();
  } else if (/^Requesting\b/.test(line)) {
    state.ppsPollingPausedUntil = Date.now() + 2000;
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
    if (pdo.position === state.contractPosition) refreshContractSupply();
    renderCapabilities();
    return;
  }

  const contract = line.match(/^Contract ready PDO(\d+)\s+(.+)$/);
  if (contract) {
    const position = Number(contract[1]);
    const detail = contract[2];
    const fixed = detail.match(/fixed=(\d+)mV/);
    const adjustable = detail.match(/encoded=(\d+)mV/);
    const pdoType = state.pdos.get(position)?.type ?? (fixed ? "Fixed" : "Adjustable");
    const voltage = Number((fixed ?? adjustable)?.[1]);
    state.inEpr = /EPR=1/.test(detail);
    state.contractPosition = position;
    state.contractSupplyType = pdoType;
    state.contractEpr = state.inEpr;
    ui.portState.textContent = "Ready";
    ui.portDetail.textContent = `Explicit contract on PDO ${position}`;
    ui.contractVoltage.textContent = displayMillivolts(voltage);
    refreshContractSupply();
    state.ppsPollingPausedUntil = Date.now() + 500;
    ui.ppsMaintenanceState.textContent = state.contractSupplyType === "PPS"
      ? "PPS contract maintenance: awaiting first refresh"
      : "PPS contract maintenance: inactive on this contract";
    configurePpsPolling();
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
    return;
  }

  const ppsStatus = line.match(
    /^PPS_Status: voltage=(unsupported|\d+mV) current=(unsupported|\d+mA) mode=(CV|CL) temperature=([^\s]+)$/,
  );
  if (ppsStatus) {
    state.ppsQueryPending = false;
    const voltageMillivolts = ppsStatus[1] === "unsupported" ? null : Number.parseInt(ppsStatus[1], 10);
    const currentMilliamps = ppsStatus[2] === "unsupported" ? null : Number.parseInt(ppsStatus[2], 10);
    const voltage = voltageMillivolts === null
      ? "Voltage unavailable"
      : displayMillivolts(voltageMillivolts);
    const current = currentMilliamps === null
      ? "current unavailable"
      : displayMilliamps(currentMilliamps);
    ui.ppsMeasurement.textContent = `${voltage} · ${current}`;
    recordTelemetrySample(voltageMillivolts, currentMilliamps);
    ui.sourceTemperature.textContent = `${displayStatusToken(ppsStatus[4])} · PPS`;
    setPpsMode(ppsStatus[3], `Source PPS_Status bit · temperature ${displayStatusToken(ppsStatus[4])}`);
    ui.telemetryNote.textContent = "These values and CV/CL mode are reported by the source. Some sources omit fields or report an unreliable mode bit; use a meter when the distinction matters.";
    return;
  }

  const sourceStatus = line.match(
    /^Source_Status: mode=(CV|CL|n\/a) internal=([^\s]+) input=([^\s]+) battery=(true|false) non-battery=(true|false) temperature=([^\s]+) events=([^\s]+) limits=([^\s]+) state=([^\s]+) indicator=([^\s]+)$/,
  );
  if (sourceStatus) {
    if (sourceStatus[1] === "CV" || sourceStatus[1] === "CL") {
      setPpsMode(sourceStatus[1], "Source general Status bit · updated after Alert or manual read");
    } else if (state.contractSupplyType !== "PPS") {
      setPpsMode(null, "The active contract is not PPS");
    }
    ui.sourceTemperature.textContent = `${displayStatusToken(sourceStatus[2])} · ${displayStatusToken(sourceStatus[6])}`;
    const inputKinds = [
      displayStatusToken(sourceStatus[3]),
      sourceStatus[4] === "true" ? "battery" : null,
      sourceStatus[5] === "true" ? "non-battery" : null,
    ].filter(Boolean);
    ui.sourceInput.textContent = `${inputKinds.join(" · ")} · state ${displayStatusToken(sourceStatus[9])}`;
    ui.sourceEvents.textContent = sourceStatus[7] === "none"
      ? "None reported"
      : displayStatusList(sourceStatus[7]);
    ui.sourceLimits.textContent = sourceStatus[8] === "none"
      ? "No active limits reported"
      : `Power limited by ${displayStatusList(sourceStatus[8]).toLowerCase()}`;
    ui.telemetryNote.textContent = `General Status received · requested indicator ${displayStatusToken(sourceStatus[10])}. Event flags describe the latest event and may clear after they are read.`;
    return;
  }

  const alert = line.match(/^PD Alert: events=([^\s]+) raw=(0x[0-9a-f]{8})$/i);
  if (alert) {
    ui.sourceEvents.textContent = alert[1] === "none"
      ? "Alert with no event bits"
      : `Alert: ${displayStatusList(alert[1])}`;
    ui.telemetryNote.textContent = "Source Alert received. The firmware automatically requests general Status for non-battery source changes.";
    return;
  }

  const queryFailure = line.match(/^(PPS_Status|Source_Status) query failed: (.+)$/);
  if (queryFailure) {
    if (queryFailure[1] === "PPS_Status") {
      state.ppsQueryPending = false;
      if (queryFailure[2] === "unsupported") {
        ui.livePps.checked = false;
        recordTelemetrySample(null, null);
        configurePpsPolling();
      }
    }
    ui.telemetryNote.textContent = `${queryFailure[1]} was ${queryFailure[2]}; the existing power contract remains active.`;
    return;
  }

  const refresh = line.match(/^Contract refresh confirmed(?: PDO(\d+) (\d+)mV usable=(\d+)mA)?$/);
  if (refresh) {
    ui.ppsMaintenanceState.textContent = state.contractSupplyType === "PPS"
      ? refresh[2]
        ? `PPS contract maintained at ${timeStamp()} · ${displayMillivolts(Number(refresh[2]))} · ${displayMilliamps(Number(refresh[3]))} usable`
        : `PPS contract maintained at ${timeStamp()}`
      : `Contract refreshed at ${timeStamp()}`;
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

async function writeTransport(bytes) {
  if (state.connectionApi === "web-serial") {
    if (!state.writer) throw new Error("The serial writer is unavailable.");
    await state.writer.write(bytes);
  } else if (state.connectionApi === "webusb-cdc") {
    if (!state.port?.opened || state.usbOutEndpoint === null) {
      throw new Error("The WebUSB CDC output endpoint is unavailable.");
    }
    const result = await state.port.transferOut(state.usbOutEndpoint, bytes);
    if (result.status !== "ok" || result.bytesWritten !== bytes.length) {
      throw new Error(`WebUSB write failed with status ${result.status}.`);
    }
  } else {
    throw new Error("The USB transport is unavailable.");
  }
  addRawTransportLog(bytes, "outgoing");
}

async function sendCommand(command, { echo = true } = {}) {
  const trimmed = command.trim();
  if (!state.connected || !state.connectionApi) {
    throw new Error("Connect the device first.");
  }
  if (!state.transport) {
    throw new Error("Waiting for the device control protocol.");
  }
  const requestedVoltage = commandRequestedVoltage(trimmed);
  if (requestedVoltage === "unknown-pdo") {
    throw new Error("Load the full capability list before making a direct maximum-PDO request.");
  }
  if (Number.isFinite(requestedVoltage) && requestedVoltage > state.maxVoltageMillivolts) {
    throw new Error(
      `This request exceeds the board's ${displayMillivolts(state.maxVoltageMillivolts)} GUI ceiling.`,
    );
  }
  const isPpsStatus = trimmed.toLowerCase() === "pps-status";
  if (isPpsStatus) state.ppsQueryPending = true;
  if (
    requestedVoltage !== null
    || /^(?:enter-epr|exit-epr|epr-caps)\b/i.test(trimmed)
  ) {
    state.ppsPollingPausedUntil = Date.now() + 2000;
  }
  try {
    let bytes;
    if (state.transport === "usb-control") {
      const sequence = state.nextSequence;
      state.nextSequence = sequence === 0xff ? 1 : sequence + 1;
      bytes = controlProtocol.encodeCommand(trimmed, sequence);
    } else {
      bytes = encoder.encode(`${trimmed}\n`);
    }
    await writeTransport(bytes);
  } catch (error) {
    if (isPpsStatus) state.ppsQueryPending = false;
    throw error;
  }
  if (echo) addLog(trimmed, "outgoing");
}

function initializeConnectedTransport(api, id) {
  cancelReconnectTimer();
  state.reconnectAttempt = 0;
  state.reconnectInFlight = false;
  state.manualDisconnect = false;
  state.connectionApi = api;
  state.keepReading = true;
  state.transport = null;
  state.detectionBuffer.length = 0;
  state.controlDecoder.reset();
  state.textDecoder = new TextDecoder();
  state.nextSequence = 1;
  state.lineBuffer = "";
  state.deviceUid = null;
  state.deviceMaxVoltageMillivolts = null;
  state.richTelemetrySupported = null;
  state.usbId = id;
  setConnected(true);
  clearDeviceState();
  refreshDeviceDescription();
  ui.portState.textContent = "Connected";
  ui.portDetail.textContent = "Detecting compact control or text console";
  addLog(`Connected through ${connectionApiLabel()} to ${id}`, "system");
}

function markTransportLost(message) {
  if (message) addLog(message, "system", "error");
  state.keepReading = false;
  try {
    state.writer?.releaseLock();
  } catch (_) {
    // A removed serial device may already have invalidated the writer.
  }
  state.writer = null;
  state.port = null;
  state.readTask = null;
  setConnected(false);
  clearDeviceState();
  scheduleAutoReconnect();
}

async function readWebSerial() {
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
    if (state.keepReading) markTransportLost(`Web Serial connection lost: ${error.message}`);
  }
}

async function readWebUsb() {
  try {
    while (state.keepReading && state.port?.opened && state.usbInEndpoint !== null) {
      const result = await state.port.transferIn(state.usbInEndpoint, state.usbPacketSize);
      if (result.status === "stall") {
        await state.port.clearHalt("in", state.usbInEndpoint);
        continue;
      }
      if (result.status !== "ok") throw new Error(`USB read status ${result.status}`);
      if (result.data?.byteLength) {
        const bytes = new Uint8Array(
          result.data.buffer,
          result.data.byteOffset,
          result.data.byteLength,
        );
        appendSerialData(Uint8Array.from(bytes));
      }
    }
  } catch (error) {
    if (state.keepReading) markTransportLost(`WebUSB connection lost: ${error.message}`);
  }
}

function findUsbInterface(configuration, classCode, subclassCode = null) {
  for (const deviceInterface of configuration.interfaces) {
    for (const alternate of deviceInterface.alternates) {
      if (
        alternate.interfaceClass === classCode
        && (subclassCode === null || alternate.interfaceSubclass === subclassCode)
      ) {
        return { deviceInterface, alternate };
      }
    }
  }
  return null;
}

async function connectWebSerial(authorizedPort = null) {
  const port = authorizedPort ?? await navigator.serial.requestPort();
  state.port = port;
  await port.open({ baudRate: 115200, bufferSize: 512 });
  state.writer = port.writable.getWriter();
  const info = port.getInfo();
  const id = `VID ${formatUsbId(info.usbVendorId)} · PID ${formatUsbId(info.usbProductId)}`;
  initializeConnectedTransport("web-serial", id);
  rememberAutoReconnectTarget("web-serial", info.usbVendorId, info.usbProductId);
  state.readTask = readWebSerial();
}

async function connectWebUsb(authorizedDevice = null) {
  const device = authorizedDevice ?? await navigator.usb.requestDevice({ filters: DEVELOPMENT_USB_FILTERS });
  state.port = device;
  await device.open();
  if (!device.configuration) await device.selectConfiguration(1);

  const communication = findUsbInterface(device.configuration, 0x02, 0x02);
  const data = findUsbInterface(device.configuration, 0x0a);
  if (!communication || !data) throw new Error("The selected device is not a CDC-ACM serial device.");

  const interfaceNumbers = [
    communication.deviceInterface.interfaceNumber,
    data.deviceInterface.interfaceNumber,
  ];
  for (const interfaceNumber of interfaceNumbers) {
    if (state.usbClaimedInterfaces.includes(interfaceNumber)) continue;
    await device.claimInterface(interfaceNumber);
    state.usbClaimedInterfaces.push(interfaceNumber);
  }

  if (data.alternate.alternateSetting !== data.deviceInterface.alternate.alternateSetting) {
    await device.selectAlternateInterface(
      data.deviceInterface.interfaceNumber,
      data.alternate.alternateSetting,
    );
  }

  const input = data.alternate.endpoints.find(
    (endpoint) => endpoint.type === "bulk" && endpoint.direction === "in",
  );
  const output = data.alternate.endpoints.find(
    (endpoint) => endpoint.type === "bulk" && endpoint.direction === "out",
  );
  if (!input || !output) throw new Error("The CDC data interface has no bulk input/output endpoint pair.");

  const communicationNumber = communication.deviceInterface.interfaceNumber;
  const lineCoding = Uint8Array.from([0x00, 0xc2, 0x01, 0x00, 0x00, 0x00, 0x08]);
  const lineResult = await device.controlTransferOut({
    requestType: "class",
    recipient: "interface",
    request: 0x20,
    value: 0,
    index: communicationNumber,
  }, lineCoding);
  if (lineResult.status !== "ok") throw new Error(`CDC line-coding request failed: ${lineResult.status}.`);

  const readyResult = await device.controlTransferOut({
    requestType: "class",
    recipient: "interface",
    request: 0x22,
    value: 1,
    index: communicationNumber,
  });
  if (readyResult.status !== "ok") throw new Error(`CDC ready request failed: ${readyResult.status}.`);

  state.usbInEndpoint = input.endpointNumber;
  state.usbOutEndpoint = output.endpointNumber;
  state.usbPacketSize = input.packetSize || 64;
  const id = `VID ${formatUsbId(device.vendorId)} · PID ${formatUsbId(device.productId)}`;
  initializeConnectedTransport("webusb-cdc", id);
  rememberAutoReconnectTarget("webusb-cdc", device.vendorId, device.productId);
  state.readTask = readWebUsb();
}

function targetMatches(target, vendorId, productId) {
  return (target.vendorId === undefined || target.vendorId === vendorId)
    && (target.productId === undefined || target.productId === productId);
}

function scheduleAutoReconnect(delay = Math.min(8000, 500 * (2 ** state.reconnectAttempt))) {
  if (!ui.autoReconnect.checked || state.manualDisconnect || state.connected || state.reconnectInFlight) return;
  cancelReconnectTimer();
  state.reconnectTimer = setTimeout(() => void tryAutoReconnect(), delay);
}

async function tryAutoReconnect() {
  if (!ui.autoReconnect.checked || state.manualDisconnect || state.connected || state.reconnectInFlight) return;
  const target = savedAutoReconnectTarget();
  if (!target) return;
  cancelReconnectTimer();
  state.reconnectInFlight = true;
  ui.connectionLabel.textContent = "Reconnecting…";
  try {
    if (target.api === "web-serial" && "serial" in navigator) {
      const ports = await navigator.serial.getPorts();
      const port = ports.find((candidate) => {
        const info = candidate.getInfo();
        return targetMatches(target, info.usbVendorId, info.usbProductId);
      });
      if (!port) throw new Error("The authorized serial device is not present.");
      await connectWebSerial(port);
    } else if (target.api === "webusb-cdc" && "usb" in navigator) {
      const devices = await navigator.usb.getDevices();
      const device = devices.find((candidate) => targetMatches(target, candidate.vendorId, candidate.productId));
      if (!device) throw new Error("The authorized USB device is not present.");
      await connectWebUsb(device);
    } else {
      throw new Error("The saved connection transport is unavailable in this browser.");
    }
    addLog("Automatically reconnected to the previously authorized device.", "system");
    setFeedback("Device reconnected automatically.");
  } catch (error) {
    await closeTransport();
    setConnected(false);
    clearDeviceState();
    state.reconnectAttempt += 1;
    ui.connectionLabel.textContent = "Waiting for device";
  } finally {
    state.reconnectInFlight = false;
  }
  if (!state.connected && state.reconnectAttempt < 5) scheduleAutoReconnect();
}

async function closeTransport() {
  const api = state.connectionApi;
  const port = state.port;
  state.keepReading = false;

  if (api === "web-serial") {
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
    try {
      state.writer?.releaseLock();
    } catch (_) {
      // The browser may already have invalidated the stream.
    }
    state.writer = null;
  }

  try {
    await port?.close();
  } catch (_) {
    // Physical removal can close the device before this path runs.
  }

  state.port = null;
  state.readTask = null;
}

async function connectPort() {
  if (state.connected) {
    await disconnectPort();
    return;
  }

  state.manualDisconnect = false;
  cancelReconnectTimer();
  try {
    if (IS_ANDROID) {
      if (!("usb" in navigator)) {
        throw new Error("This Android browser does not expose WebUSB. Use current Google Chrome.");
      }
      await connectWebUsb();
    } else if ("serial" in navigator) {
      await connectWebSerial();
    } else if ("usb" in navigator) {
      await connectWebUsb();
    } else {
      throw new Error("This browser exposes neither Web Serial nor WebUSB.");
    }
  } catch (error) {
    if (error.name === "NotFoundError" && IS_ANDROID) {
      const message =
        "No matching WebUSB device was selected. Dismiss other Android USB-app prompts, reconnect the board through OTG, and try again.";
      addLog(`Connection failed: ${message}`, "system", "error");
      setFeedback(message, true);
    } else if (error.name !== "NotFoundError") {
      addLog(`Connection failed: ${error.message}`, "system", "error");
      setFeedback(error.message, true);
    }
    await closeTransport();
    setConnected(false);
    clearDeviceState();
  }
}

async function disconnectPort() {
  state.manualDisconnect = true;
  cancelReconnectTimer();
  forgetAutoReconnectTarget();
  await closeTransport();
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
ui.autoReconnect.addEventListener("change", () => {
  try {
    localStorage.setItem(AUTO_RECONNECT_ENABLED_KEY, String(ui.autoReconnect.checked));
  } catch (_) {
    // Keep the preference for this page lifetime when storage is unavailable.
  }
  if (!ui.autoReconnect.checked) {
    cancelReconnectTimer();
    forgetAutoReconnectTarget();
    setFeedback("Automatic reconnect disabled.");
  } else {
    state.manualDisconnect = false;
    setFeedback("Automatic reconnect enabled for previously authorized devices.");
    if (!state.connected) void tryAutoReconnect();
  }
});
ui.livePps.addEventListener("change", configurePpsPolling);
ui.telemetryToggle.addEventListener("click", () => {
  ui.livePps.checked = state.ppsRefreshTimer === null;
  configurePpsPolling();
  setFeedback(ui.livePps.checked
    ? "Live PPS telemetry started; only one source query can be in flight."
    : "Live PPS telemetry stopped. Mandatory PPS contract maintenance remains active.");
});
ui.condenseLog.addEventListener("click", () => {
  state.condenseConsole = !state.condenseConsole;
  ui.condenseLog.setAttribute("aria-pressed", String(state.condenseConsole));
  ui.condenseLog.textContent = `Condense stream: ${state.condenseConsole ? "On" : "Off"}`;
  renderConsole();
});
ui.rawLog.addEventListener("click", () => {
  state.rawStream = !state.rawStream;
  ui.rawLog.setAttribute("aria-pressed", String(state.rawStream));
  ui.rawLog.textContent = `Raw stream: ${state.rawStream ? "On" : "Off"}`;
  renderConsole();
});
ui.inspectBlackBox.addEventListener("click", () => void inspectBlackBox());

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
    const voltage = toMilli(ui.voltageInput.value, "Voltage", { maximum: 50 });
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
      const voltage = toMilli(ui.pdoVoltage.value, "Voltage", { maximum: 50 });
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
  state.rawLog.length = 0;
  state.collapsedConsoleRows.clear();
  ui.terminal.replaceChildren();
});

function exportLogEntries() {
  return visibleLogEntries();
}

ui.copyLog.addEventListener("click", async () => {
  const text = exportLogEntries().map((entry) => formatLogEntry(entry, timeStamp(entry.time))).join("\n");
  try {
    await navigator.clipboard.writeText(text);
    setFeedback("Console copied to the clipboard.");
  } catch (error) {
    setFeedback(`Could not copy the console: ${error.message}`, true);
  }
});

ui.saveLog.addEventListener("click", () => {
  const text = exportLogEntries().map((entry) => formatLogEntry(entry, entry.time.toISOString())).join("\n");
  const blob = new Blob([`${text}\n`], { type: "text/plain;charset=utf-8" });
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = `usb-pd-session-${new Date().toISOString().replaceAll(":", "-")}.log`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 0);
});

ui.terminal.addEventListener("copy", (event) => {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) return;
  const range = selection.getRangeAt(0);
  const lines = [...ui.terminal.querySelectorAll(".terminal-line")]
    .filter((row) => {
      try {
        return range.intersectsNode(row);
      } catch {
        return false;
      }
    })
    .map((row) => row.dataset.copyLine);
  if (lines.length === 0 || !event.clipboardData) return;
  event.clipboardData.setData("text/plain", lines.join("\n"));
  event.preventDefault();
});
window.addEventListener("resize", drawTelemetryChart);

const androidRequiresHttps = IS_ANDROID && location.protocol !== "https:";
if (androidRequiresHttps) {
  ui.browserNotice.textContent =
    "Android WebUSB requires this page to be served over HTTPS. The standalone local file supports desktop Chrome or Edge through Web Serial.";
  ui.browserNotice.classList.remove("hidden");
  ui.connectButton.disabled = true;
} else if (IS_ANDROID && !("usb" in navigator)) {
  ui.browserNotice.textContent = "This Android browser does not expose WebUSB. Use current Google Chrome.";
  ui.browserNotice.classList.remove("hidden");
  ui.connectButton.disabled = true;
} else if (!("serial" in navigator) && !("usb" in navigator)) {
  ui.browserNotice.classList.remove("hidden");
  ui.connectButton.disabled = true;
}

if ("serial" in navigator) {
  navigator.serial.addEventListener("connect", () => {
    if (!state.connected) void tryAutoReconnect();
  });
  navigator.serial.addEventListener("disconnect", (event) => {
    if (
      state.connectionApi === "web-serial"
      && event.target === state.port
      && state.connected
      && state.keepReading
    ) {
      markTransportLost("USB serial device removed");
    }
  });
}

if ("usb" in navigator) {
  navigator.usb.addEventListener("connect", () => {
    if (!state.connected) void tryAutoReconnect();
  });
  navigator.usb.addEventListener("disconnect", (event) => {
    if (
      state.connectionApi === "webusb-cdc"
      && event.device === state.port
      && state.connected
      && state.keepReading
    ) {
      markTransportLost("WebUSB device removed");
    }
  });
}

ui.autoReconnect.checked = readAutoReconnectPreference();
setConnected(false);
clearDeviceState();
updatePdoFields();
updateActionAvailability();
drawTelemetryChart();
addLog(
  IS_ANDROID
    ? "USB-PD Sink Control is ready. Android transport is WebUSB CDC."
    : "USB-PD Sink Control is ready. Desktop standalone prefers Web Serial.",
  "system",
);
if (ui.autoReconnect.checked && savedAutoReconnectTarget()) scheduleAutoReconnect(250);
