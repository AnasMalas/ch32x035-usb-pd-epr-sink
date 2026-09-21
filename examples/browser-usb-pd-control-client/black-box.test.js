"use strict";

const assert = require("node:assert/strict");
require("./black-box.js");

const blackBox = globalThis.PdBlackBox;
const summary = Uint8Array.from([1, 3, 2, 3, 7, 0, 0, 0, 9, 0, 1, 0, 1, 2]);
assert.deepEqual(blackBox.decodeSummary(summary), {
  abi: 1, valid: true, restored: true, dirty: false, writeError: false,
  eventCount: 2, hardReset: true, frozen: true, overwritten: false,
  generation: 7, nextSequence: 9, traceAbi: 1, activePage: 0, preparedPage: 1,
  profile: "deep numeric", traceLevel: "protocol",
});

const linkSummary = Uint8Array.from(summary);
linkSummary[3] |= 16;
assert.equal(blackBox.decodeSummary(linkSummary).traceLevel, "link");

const response = Uint8Array.from([0x50, 0x44, 0x42, 0x42, 0xb0, 14, ...summary]);
assert.equal(blackBox.parseResponse(response).kind, 0xb0);

const event = Uint8Array.from([
  1, 0, 1, 12,
  0xd2, 0x04, 0, 0,
  12, 5, 0, 255,
  0xff, 0xff, 8, 0,
]);
assert.deepEqual(blackBox.decodeEvent(event, 0), {
  index: 0,
  uptime: 1234,
  text: "hard-reset; phase=transmit-failure; reason=EPR-keepalive-failed",
});

assert.equal(blackBox.describeHeader(0x0b61), "GoodCRC, id=5, objects=0, header=0x0b61");

const detectorLow = Uint8Array.from([
  1, 0, 1, 12,
  0xd2, 0x04, 0, 0,
  0x88, 1, 0xff, 0xff,
  1, 0, 0, 0,
]);
assert.deepEqual(blackBox.decodeEvent(detectorLow, 1), {
  index: 1,
  uptime: 1234,
  text: "APP VBUS-detector-low; path=active-low-edge; qualified-before=true",
});

const powerFailSample = Uint8Array.from([
  1, 0, 1, 12,
  0xd3, 0x04, 0, 0,
  0x89, 0x0b, 0xff, 0xff,
  0x02, 0x40, 0x02, 0,
]);
assert.deepEqual(blackBox.decodeEvent(powerFailSample, 2), {
  index: 2,
  uptime: 1235,
  text: "APP power-fail-sample; detector-low=true; EXTI1-pending=true; EXTI1-armed=false; qualified-before=true; GPIOB-IN=0x4002; EXTI-pending=0x0002",
});

console.log("Black-box browser decoder tests passed");
