"use strict";

const assert = require("node:assert/strict");
require("./black-box.js");

const blackBox = globalThis.PdBlackBox;
const summary = Uint8Array.from([1, 3, 2, 3, 7, 0, 0, 0, 9, 0, 1, 0, 1, 2]);
assert.deepEqual(blackBox.decodeSummary(summary), {
  abi: 1, valid: true, restored: true, dirty: false, writeError: false,
  eventCount: 2, hardReset: true, frozen: true, overwritten: false,
  generation: 7, nextSequence: 9, traceAbi: 1, activePage: 0, preparedPage: 1,
  profile: "deep numeric",
});

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

console.log("Black-box browser decoder tests passed");
