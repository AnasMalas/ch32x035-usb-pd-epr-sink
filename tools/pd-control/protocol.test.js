"use strict";

const assert = require("node:assert/strict");
require("./protocol.js");

const protocol = globalThis.PdControlProtocol;

assert.equal(protocol.boundedVoltageCeiling(50000), 48000);
assert.equal(protocol.boundedVoltageCeiling(50000, 50000), 50000);
assert.equal(protocol.boundedVoltageCeiling(48000, 28000), 28000);
assert.equal(protocol.boundedVoltageCeiling(21000, 50000), 5000);

function u32(value) {
  const number = value >>> 0;
  return [number & 0xff, (number >>> 8) & 0xff, (number >>> 16) & 0xff, (number >>> 24) & 0xff];
}

function eventLines(kind, payload, sequence = 0) {
  const decoder = new protocol.FrameDecoder();
  const decoded = decoder.push(protocol.encodeFrame(kind, sequence, Uint8Array.from(payload)));
  assert.equal(decoded.errors, 0);
  assert.equal(decoded.frames.length, 1);
  return protocol.translateFrame(decoded.frames[0]);
}

{
  const frame = protocol.encodeCommand("request 17200 2300 pps", 42);
  const decoder = new protocol.FrameDecoder();
  const first = decoder.push(frame.subarray(0, 5));
  assert.equal(first.frames.length, 0);
  const second = decoder.push(frame.subarray(5));
  assert.equal(second.frames.length, 1);
  assert.equal(second.frames[0].kind, 0x10);
  assert.equal(second.frames[0].sequence, 42);
  assert.deepEqual([...second.frames[0].payload], [...u32(17200), ...u32(2300), 2]);

  assert.deepEqual([...protocol.encodeCommand("request 17200 2300 pps", 37)], [
    0x50, 0x44, 0x01, 0x10, 0x25, 0x09, 0x30, 0x43,
    0x00, 0x00, 0xfc, 0x08, 0x00, 0x00, 0x02, 0xb4,
  ]);
}

{
  const raws = [
    0x0a91912c,
    0x0012d12c,
    0x0013c12c,
    0x0014b12c,
    0x001641f4,
    0xc9a43264,
    0x00000000,
    0x0018c1f4,
    0x001b41f4,
    0x001f01f4,
    0xd7c096f0,
  ];
  const lines = eventLines(0x83, [1, 1, raws.length, ...raws.flatMap(u32)]);
  assert.equal(lines.length, 12);
  assert.equal(lines[0], "Source caps: kind=EPR count=11 EPR=true");
  assert.equal(lines[6], "PDO6 PPS 5000-21000mV 5000mA limited=true valid raw=0xc9a43264");
  assert.equal(lines[7], "PDO7 padding padding raw=0x00000000");
  assert.equal(lines[10], "PDO10 fixed 48000mV 5000mA EPR=false valid raw=0x001f01f4");
  assert.equal(lines[11], "PDO11 EPR-AVS 15000-48000mV standard=15000-48000mV PDP=240000mW peak=1 valid raw=0xd7c096f0");
}

assert.equal(
  protocol.pdoLine(1, 9, 0xd230328c),
  "PDO9 EPR-AVS 5000-28000mV standard=15000-28000mV PDP=140000mW peak=0 compatible raw=0xd230328c",
);

assert.equal(
  protocol.pdoLine(1, 8, 0xd3e8968c),
  "PDO8 EPR-AVS 15000-50000mV standard=15000-48000mV PDP=140000mW peak=0 compatible raw=0xd3e8968c",
);

assert.match(protocol.pdoLine(1, 8, 0xd3ea968c), /malformed/);

{
  const payload = [
    2,
    1,
    10,
    0,
    1,
    0,
    ...u32(48000),
    ...u32(48000),
    0,
    0,
    ...u32(0xffffffff),
    ...u32(5000),
    ...u32(2910),
    0,
    4,
    ...u32(0xa1448d23),
    ...u32(0x001f01f4),
  ];
  assert.deepEqual(eventLines(0x84, payload), [
    "Contract ready PDO10 fixed=48000mV EPR=1",
    "Contract ready req=max src=5000mA usable=2910mA confidence=advertised limit=sink-power mismatch=false",
  ]);
}

{
  const uid = [0xcd, 0xab, 0x0b, 0x92, 0x7a, 0xbd, 0x52, 0xfb];
  assert.deepEqual(eventLines(0x81, [...uid, 3, ...u32(48000), ...u32(5000), ...u32(140000)]), [
    "Device id=cdab0b927abd52fb",
    "Device limits: max=48000mV current=5000mA power=140000mW PPS=true EPR=true",
  ]);
}

{
  const damaged = protocol.encodeFrame(0x86, 0, Uint8Array.from([240, 240, 1]));
  damaged[6] ^= 1;
  const good = protocol.encodeFrame(0x86, 1, Uint8Array.from([240, 240, 1]));
  const decoder = new protocol.FrameDecoder();
  const decoded = decoder.push(Uint8Array.from([...damaged, ...good]));
  assert.equal(decoded.errors, 1);
  assert.equal(decoded.frames.length, 1);
  assert.deepEqual(protocol.translateFrame(decoded.frames[0]), [
    "Source_Info: present=240 W, maximum=240 W, reported=1 W",
  ]);
}

assert.throws(() => protocol.encodeCommand("request 17200 2300 magic", 1), /preference/i);
assert.throws(() => protocol.encodeCommand("pdo 10 adjust 48000 0", 1), /range/i);

console.log("USB-control browser protocol tests passed");
