import { test } from "node:test";
import assert from "node:assert/strict";
import {
  BUILT_IN_PRESETS, DEFAULT_SETTINGS, applyPreset, newMotion, restorePrefs, restoreSettings, toExtras, toHook, toMotion, toTalkStyle, toTravelStyle,
} from "./settings.ts";

test("restoreSettings keeps valid fields and ignores damaged ones", () => {
  const s = restoreSettings({ orientation: "horizontal", musicVolume: 250, grade: "sepia", zoom: "yes", music: "/m.mp3", unknown: 1 });
  assert.equal(s.orientation, "horizontal");
  assert.equal(s.musicVolume, 100, "clamped");
  assert.equal(s.grade, "warm", "unknown choice falls back");
  assert.equal(s.zoom, true, "wrong type ignored");
  assert.equal(s.music, "/m.mp3");
  assert.deepEqual(restoreSettings(null), DEFAULT_SETTINGS);
});

test("presets change only what they set", () => {
  const current = { ...DEFAULT_SETTINGS, music: "/song.mp3", captionFont: "/f.ttf" };
  const travel = applyPreset(current, BUILT_IN_PRESETS.find((p) => p.name.startsWith("Travel reel"))!);
  assert.equal(travel.mode, "travel");
  assert.equal(travel.beatSync, true);
  assert.equal(travel.music, "/song.mp3", "music file kept");
  assert.equal(travel.captionFont, "/f.ttf", "font kept");
});

test("styles map to the Rust command fields", () => {
  const s = { ...DEFAULT_SETTINGS, musicVolume: 25, videoVolume: 0, muteVideo: true, fitBlur: true, bothShapes: true };
  const talk = toTalkStyle(s);
  assert.equal(talk.musicVolume, 0.25);
  assert.equal(talk.videoVolume, 0);
  assert.equal(talk.muteVideo, true);
  assert.equal(talk.softCaptions, true);
  assert.equal(talk.bothShapes, true);
  const travel = toTravelStyle(s);
  assert.equal(travel.fit, "blur");
  assert.equal(travel.font, null);
  assert.equal(travel.smartPick, true);
});

test("extras: tags with and without phrases, empty means none", () => {
  assert.equal(toExtras({ hook: " ", tags: "\n" }, " "), null);
  const x = toExtras({ hook: "HOOK", tags: "A | apple\nB" }, "CTA");
  assert.deepEqual(x, { hook: "HOOK", tags: [{ text: "A", phrase: "apple" }, { text: "B" }], cta: "CTA", motion: [], kitDir: null });
});

test("restorePrefs reads user presets and drops broken ones", () => {
  const p = restorePrefs({ last: { mode: "travel" }, presets: [{ name: "Mine", settings: { musicVolume: 33 } }, { nope: true }, null] });
  assert.equal(p.last.mode, "travel");
  assert.equal(p.presets.length, 1);
  assert.equal(p.presets[0].settings.musicVolume, 33);
  assert.deepEqual(restorePrefs("garbage"), { last: DEFAULT_SETTINGS, presets: [] });
});

test("motion drafts: seconds or phrase, empty ones dropped, kit folder passed along", () => {
  const card = { ...newMotion("card"), text: " 1.5 YEARS ", when: "12.5" };
  const byPhrase = { ...newMotion("sticker"), text: "NEW", when: "ergonomic" };
  const empty = newMotion("card");
  const arrow = newMotion("arrow");
  assert.equal(empty.when, "1", "new graphics start at 1 s so they show in a preview");
  const m = toMotion([card, byPhrase, empty, arrow]);
  assert.equal(m.length, 3, "an empty card is dropped, an arrow needs no text");
  assert.deepEqual([m[0].at, m[0].phrase, m[0].text], [12.5, null, "1.5 YEARS"]);
  assert.deepEqual([m[1].at, m[1].phrase, m[1].variant], [null, "ergonomic", "badge"]);
  assert.equal(m[2].variant, "down");
  const x = toExtras({ hook: "", tags: "" }, "", [card], "/kit");
  assert.equal(x?.kitDir, "/kit");
  assert.equal(x?.motion.length, 1);
  assert.equal(toExtras({ hook: "", tags: "" }, "", [empty]), null);
  assert.equal(restoreSettings({ kitDir: "/k" }).kitDir, "/k");
});

test("hook settings: length choices, whole video, custom time, bounds", () => {
  const base = { ...DEFAULT_SETTINGS };
  assert.deepEqual(toHook(base), { hookStart: null, hookSec: 3, hookUntilEnd: false, hookSize: 1, hookPosition: "top", hookAnim: "slide" });
  assert.equal(toHook({ ...base, hookDuration: "8" }).hookSec, 8);
  assert.equal(toHook({ ...base, hookDuration: "custom", hookCustomSec: 12.5 }).hookSec, 12.5);
  const whole = toHook({ ...base, hookDuration: "whole", hookStart: 2, hookSize: 150 });
  assert.deepEqual([whole.hookUntilEnd, whole.hookSec, whole.hookStart, whole.hookSize], [true, null, 2, 1.5]);
  // Saved values that are out of range or unknown are repaired.
  const s = restoreSettings({ hookDuration: "forever", hookSize: 900, hookCustomSec: -4, hookPosition: "sideways", hookAnim: "spin" });
  assert.deepEqual([s.hookDuration, s.hookSize, s.hookCustomSec, s.hookPosition, s.hookAnim], ["3", 180, 1, "top", "slide"]);
  // The hook settings travel with the hook, and only when there is one.
  assert.equal(toExtras({ hook: "", tags: "" }, "CTA", [], null, base)?.hookSec, undefined);
  assert.equal(toExtras({ hook: "H", tags: "" }, "", [], null, { ...base, hookDuration: "5" })?.hookSec, 5);
  assert.equal(toExtras({ hook: "H", tags: "" }, "", [])?.hookSec, undefined, "old callers without settings still work");
});
