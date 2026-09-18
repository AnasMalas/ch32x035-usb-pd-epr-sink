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
  "Condense stream: On",
  "Raw stream: Off",
  "epr-avs-nonstandard",
  "name=\"voltage-ceiling\" value=\"50000\"",
]) {
  assert.ok(html.includes(required) || app.includes(required), `missing console behavior: ${required}`);
}

assert.match(html, /<section class="panel source-panel"/);
assert.match(html, /<section class="panel terminal-panel"/);
assert.match(html, /aria-live="polite"/);
assert.match(css, /@media \(max-width: 1260px\)/);
assert.match(css, /@media \(max-width: 760px\)/);
assert.match(css, /:focus-visible/);

console.log("Browser console structure tests passed");
