"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const root = __dirname;
const html = fs.readFileSync(path.join(root, "index.html"), "utf8");
const css = fs.readFileSync(path.join(root, "styles.css"), "utf8");
const app = fs.readFileSync(path.join(root, "app.js"), "utf8");

const htmlIds = [...html.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]);
const idCounts = new Map();
for (const id of htmlIds) idCounts.set(id, (idCounts.get(id) ?? 0) + 1);
assert.deepEqual(
  [...idCounts.entries()].filter(([, count]) => count !== 1),
  [],
  "every console id must be unique",
);

const queriedIds = [...app.matchAll(/\$\("#([^"]+)"\)/g)].map((match) => match[1]);
for (const id of queriedIds) {
  assert.equal(idCounts.get(id), 1, `app.js hook #${id} must exist exactly once`);
}

for (const command of [
  "caps",
  "plans",
  "status",
  "source-info",
  "pps-status",
  "source-status",
  "enter-epr",
  "epr-caps",
  "exit-epr",
  "output-on",
  "output-off",
]) {
  assert.match(html, new RegExp(`data-command="${command}"`), `missing ${command} control`);
}

for (const required of [
  "navigator.serial.requestPort",
  "navigator.usb.requestDevice",
  "navigator.serial.getPorts",
  "navigator.usb.getDevices",
  "dataTerminalReady: true",
  "dataTerminalReady: false",
  "Condense stream: On",
  "Raw stream: Off",
  "epr-avs-nonstandard",
  "Inspect black box",
  "Start PPS telemetry",
  "Expand graphs",
  "Voltage unavailable",
  "RAM snapshot newer than flash",
  "power-fail capture armed",
]) {
  assert.ok(html.includes(required) || app.includes(required), `missing console behavior: ${required}`);
}

assert.match(html, /<section class="panel source-panel"/);
assert.match(html, /<section class="panel terminal-panel"/);
assert.match(html, /class="terminal-tools"[\s\S]*id="inspect-black-box"[\s\S]*id="terminal"/);
assert.match(html, /aria-live="polite"/);
assert.match(css, /@media \(max-width: 1260px\)/);
assert.match(css, /@media \(max-width: 760px\)/);
assert.match(css, /:focus-visible/);
assert.match(css, /\.capability-table \{ min-width: 560px/);
assert.match(css, /\.capability-table \.empty-row td \{[^}]*text-align: center/);
assert.match(css, /\.telemetry-chart \{[^}]*width: 100%;[^}]*height: 100%/);
assert.match(css, /\.telemetry-chart-shell\.is-expanded \{ height: 360px/);

console.log("Browser console structure tests passed");
