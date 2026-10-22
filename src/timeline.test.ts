import { test } from "node:test";
import assert from "node:assert/strict";
import {
  createClip, setTrim, validateTrim, moveClip, removeClip, toRenderPlan, totalDuration, MIN_CLIP_SEC,
} from "./timeline.ts";

const a = () => createClip({ path: "/v/a.mp4", fileName: "a.mp4", durationSec: 5 });
const b = () => createClip({ path: "/v/b.mp4", fileName: "b.mp4", durationSec: 4 });

test("new clip spans the full source", () => {
  const c = a();
  assert.equal(c.inPoint, 0);
  assert.equal(c.outPoint, 5);
});

test("valid trim is applied", () => {
  const r = setTrim(a(), 1, 3.5);
  assert.ok(r.ok);
  if (r.ok) assert.deepEqual([r.clip.inPoint, r.clip.outPoint], [1, 3.5]);
});

test("invalid trim ranges are rejected", () => {
  assert.ok(validateTrim(5, -0.1, 3));            // before start
  assert.ok(validateTrim(5, 1, 5.5));             // after end
  assert.ok(validateTrim(5, 3, 2));               // inverted
  assert.ok(validateTrim(5, 2, 2));               // zero length
  assert.ok(validateTrim(5, 2, 2 + MIN_CLIP_SEC / 2)); // too short
  assert.ok(validateTrim(5, NaN, 2));             // not a number
  assert.equal(validateTrim(5, 0, 5), null);
  assert.equal(setTrim(a(), 3, 2).ok, false);
});

test("moveClip reorders and ignores out-of-range moves", () => {
  const clips = [a(), b()];
  const swapped = moveClip(clips, 0, 1);
  assert.deepEqual(swapped.map((c) => c.fileName), ["b.mp4", "a.mp4"]);
  assert.equal(moveClip(clips, 0, -1), clips);
  assert.equal(moveClip(clips, 1, 1), clips);
  assert.deepEqual(clips.map((c) => c.fileName), ["a.mp4", "b.mp4"]); // original untouched
});

test("same source can appear twice with independent trims and ids", () => {
  const first = a();
  const second = a();
  assert.notEqual(first.id, second.id);
  const r = setTrim(second, 2, 4);
  assert.ok(r.ok);
  assert.equal(first.inPoint, 0);
});

test("removeClip removes only the given id", () => {
  const clips = [a(), b()];
  assert.deepEqual(removeClip(clips, clips[0].id).map((c) => c.fileName), ["b.mp4"]);
});

test("render plan follows timeline order and trims", () => {
  const ta = setTrim(a(), 1, 3);
  const tb = setTrim(b(), 0.5, 4);
  assert.ok(ta.ok && tb.ok);
  if (!ta.ok || !tb.ok) return;
  const plan = toRenderPlan(moveClip([ta.clip, tb.clip], 1, -1));
  assert.ok(plan.ok);
  if (plan.ok) {
    assert.deepEqual(plan.segments, [
      { order: 1, sourcePath: "/v/b.mp4", inPoint: 0.5, outPoint: 4 },
      { order: 2, sourcePath: "/v/a.mp4", inPoint: 1, outPoint: 3 },
    ]);
  }
  assert.equal(totalDuration([ta.clip, tb.clip]), 5.5);
});

test("render plan rejects empty or invalid timelines", () => {
  assert.equal(toRenderPlan([]).ok, false);
  const broken = { ...a(), inPoint: 4, outPoint: 1 };
  const plan = toRenderPlan([b(), broken]);
  assert.equal(plan.ok, false);
  if (!plan.ok) assert.match(plan.error, /Clip 2/);
});
