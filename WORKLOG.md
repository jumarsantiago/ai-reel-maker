# WORKLOG - AI Reel Maker Day 1 (runbook: Notion "One-Day MVP Sprint & Claude Code Runbook v0.1")

Project dir: ~/Projects/ai-video-edit (user override; runbook default was ~/Projects/ai-reel-maker-day1)
Budget: no spend. No paid API, no uploads. Kailan Lang files untouched.

## M0 - Safe setup & environment audit (DONE 2026-10-22)
Done:
- Notion runbook + parent spec + Kailan Lang spec read (read-only).
- Target dir audited: contained only my earlier Herk-kit leftovers; renamed, not deleted:
  `_to_delete/_failed_clone_kit`, `WORKLOG.old-herk.md`.
- Session shells: device_bash = sandboxed Linux aarch64 VM (4 GB RAM, ~9 GB disk), NOT macOS.
  rustc/cargo/xcode-select missing there (says nothing about the Mac).
- Computer use: Terminal is click-only (cannot type). Mac audit needs user-pasted output.
Deviation: earlier Herk Edit Kit work (VM scratch ~/herk-edit-kit, npm ci, 11/11 tests) is out of plan; ignored.

### 2026-10-22 native Mac audit (new session; shell runs on the Mac itself, not a VM)
- Runbook re-fetched via Notion connector (OK). Dir is not a git repo; only WORKLOG*, `_to_delete/` present.
- macOS 27.0.1 arm64; Xcode 26.6 at /Applications/Xcode.app; Apple clang 21.0.0; 205 GiB free.
- node v24.14.1, npm 11.11.0, Homebrew 6.0.20 present.
- MISSING: rustc/cargo (no ~/.cargo, ~/.rustup), Tauri CLI, ffmpeg, ffprobe (not in brew or PATH).
Blockers (need owner approval - system-level installs, free):
1. Rust toolchain (rustup) - required for Tauri 2.
2. FFmpeg + FFprobe (e.g. `brew install ffmpeg`) - required for M3/M4.
Owner approved 1+2. Installed: rustc/cargo 1.99.1 (rustup, minimal profile, ~/.cargo);
ffmpeg/ffprobe 9.0.2 (brew, /opt/homebrew/bin). Tauri CLI 2.12.4 (project devDependency).
Gate: MET - safe path + feasible toolchain.

## M1 - Desktop shell + real media import (DONE 2026-10-22)
Gate MET: owner opened the debug .app, imported test-media/clip-a.mp4 by hand, confirmed preview playback
  and properties (1280x720, h264, 30 fps, audio) - owner-reported, not automated.
## M6 - Local auto-edit pipeline, no API (DONE 2026-10-22)
Approved by owner: whisper-cpp, multilingual ggml-small.bin, crates ureq/ab_glyph/image. Footage: Tagalog/Taglish.
Installed: whisper-cpp 1.9.5 (brew). Model ~/Library/Application Support/ai-reel-maker/models/ggml-small.bin,
  487,601,967 B, sha256 1be3a9b2...987b = Hugging Face x-linked-etag. Crates: ureq 2, ab_glyph 0.2, image 0.25 (png).
Test clip: test-media/clip-speech.mp4 (28.85s, 1920x1080 testsrc + macOS `say` Samantha reading a Taglish script
  with dead air, "um", repeated "how to make", fumbled "strong cough"). No Filipino TTS voice on this Mac.
Findings + fixes:
- whisper.cpp word times smear across pauses -> split audio on FFmpeg silencedetect (-35 dB, >=0.35s) and
  transcribe all chunks in ONE whisper-cli run (model loads once), offset per chunk.
- Still ~0.3s late inside chunks ("um" leaked into output; caught by re-transcription check). DTW anchors
  were -1 because flash attention is on by default -> `-dtw small -nfa`. Word edges now = quietest point of
  the 10 ms RMS envelope between consecutive DTW anchors ("dtw+energy"). "um" now 6.68-6.98 (audio 6.65-6.95).
- FFmpeg build has no libass/freetype -> captions rasterized in Rust (Arial Bold, white + black outline,
  wrapped) and overlaid with `overlay=enable='between(t,a,b)'`; .srt sidecar written.
- verify_export.py flaked (random "PSNR n/a"): with `-frames:v 1` this FFmpeg sometimes skips the psnr
  filter's uninit (no summary, unflushed stats). Fixed: per-input `-t 0.02` + `psnr=stats_file`. Stable
  37/37 x6 runs. Flake could only cause false FAILs; earlier M3-M5 PASSes stand (re-run: 10/10, 8/8, 8/8).
Code: src-tauri/src/{transcribe,plan,captions,autoedit}.rs, bin/autoedit.rs (CLI), render.rs (+Fit::Crop,
  caption overlays, render_to). Validator: source_id, 0<=start<end<=dur, min 0.3s, no overlaps, snap to word
  edges, removal evidence ids exist + near range, removals may not cut kept speech, captions inside segment
  and >=60% words match transcript (fuzzy, spelling fixes allowed + warned).
Run (actual):
- `autoedit prepare test-media/clip-speech.mp4 --lang auto` -> 59 words in 2.1s, lang en, request.json
  (system 1,737 chars + user 1,418 chars + schema) written, NOT sent.
- Fixture plan test-media/clip-speech.fixture-plan.json (hand-written stand-in for Claude). First fixture
  draft was REJECTED by validator (repeat evidence word 19 outside range) -> fixed fixture.
- `autoedit render` -> output.mp4 17.40s 1080x1920 h264+aac, 8 segments, 17 captions, ~3s incl re-check.
  Re-transcription: kept-words match 94% (diffs only Whisper Tagalog spelling dalee/dalai, salamit/salamate),
  "how to make" x1, no "cough", no "um".
- `verify_export.py output.mp4 --validated plan.validated.json --source clip-speech.mp4 --fit crop --srt
  output.srt` -> 37/37 PASS (segments above caption band 57.7-60.5 dB; every caption band 16-22 dB).
- cargo test --lib 30/30; npm test 8/8; npm run build OK; app+CLI build OK; source checksums OK.
Not done: no Claude call (M7); no UI button (M8); real Tagalog speech untested (synthetic EN voice only).

## Option B (Claude API auto-edit) - audit + proposal (2026-10-22, approved for M6)
Owner chose Option B (supersedes runbook's "defer AI"). Proposal: docs/OPTION_B_PLAN.md.
Audit: no transcription/AI/caption/9:16 code exists. Creds: ANTHROPIC_API_KEY/AUTH_TOKEN/PROFILE unset, no
  ~/.config/anthropic, no `ant`, no Keychain item (presence-only checks). FFmpeg lacks libass/freetype ->
  no subtitles/drawtext; plan = Rust-rendered caption PNGs + overlay. whisper-cpp 1.9.5 in brew, not installed.
Pricing (live page, 2026-10-22): Opus 5.5 $4/$20, Sonnet 5.5 $2/$10, Haiku 5.5 $0.10/$0.50 per MTok.
Estimate per test call (Opus 5.5): ~$0.09-0.15, ceiling ~$0.34. No paid calls, installs, or uploads done.

## M5 - Stabilize and deliver (DONE 2026-10-22)
- Review: no critical failures/usability blockers found; no code changes needed beyond M2 label fix.
- README.md rewritten (was the Tauri template): prerequisites + verified versions, run, test, export
  workflow, limitations, licensing note (Homebrew FFmpeg = `--enable-gpl --enable-libx264`).
- `npm run tauri build -- --bundles app` -> OK (1m 07s), release `AI Reel Maker.app` 4.3 MB, unsigned.
- Release smoke test via UI: import a/b/c, timeline clip-c 0->6 + clip-a 2->4, Export ->
  `exports/m5-release.mp4` "0:08.00 1280x720 audio 1.2 MB"; verify_export 8/8 PASS (PSNR 56.0/55.2, 44.9/44.9).
  Typing a name without extension -> saved as .mp4 correctly.
- test-media checksums OK; 0 temp leftovers. COMPLETION_REPORT.md written.
Day 1 status: all P0 done + verified. P1 not started. Git: initial commit on `main` (49 files; test clips included, WORKLOG.old-herk.md left untracked). Owner may delete `_to_delete/`,
  `WORKLOG.old-herk.md` (untouched by me).

## M4 - End-to-end verification (DONE 2026-10-22)
Tooling: `scripts/verify_export.py OUT SRC:IN:OUT ...` (python3 -I; ffprobe/ffmpeg arg lists, no shell).
  Checks file size, H.264 stream, duration vs plan (+/-0.15s), full decode, and per-segment PSNR of
  frames near start/end vs expected source frames (>= 30 dB) -> proves order AND trim points.
  Negative controls fail as they should: wrong trim ~23-26 dB, wrong order ~9 dB (correct: 44-68 dB).
Added `test-media/clip-c.mp4` (6s 1280x720 `testsrc` w/ frame counter + 660 Hz) because clip-b is static
  bars (PSNR can't prove its trim). SHA256SUMS appended.
Fresh UI run (relaunched app, computer use, full control approved):
- Imported a, b, c via native picker; added a, c, b; trims a 0.5->2.5, b 1.0->2.5, c 1.5->4.0; c moved first.
  Plan: `1. clip-c 1.50->4.00  2. clip-a 0.50->2.50  3. clip-b 1.00->2.50`, 0:06.00.
- Export -> modal progress -> "Export complete 0:06.00 1280x720 audio 1.0 MB". Show in Finder -> Finder
  front window `exports`, selection = exported file (osascript-confirmed).
- Save panel quirk: it pre-selects only the base name, so typing "m4-e2e.mp4" gave `m4-e2e.mp4.mp4`
  (standard macOS behaviour, not app logic). Renamed with `mv -n` to `exports/m4-e2e.mp4`.
Results (actual):
- `verify_export.py exports/m4-e2e.mp4 c:1.5:4.0 a:0.5:2.5 b:1.0:2.5` -> 10/10 PASS: 1,035,328 B;
  h264 1280x720 30fps yuv420p; aac 48k 2ch; 6.000s; decode clean; PSNR c 55.4/55.1, a 45.6/45.3, b 68.2/68.2 dB.
- Audio pitch per segment: ~659 Hz (c) -> ~439 Hz (a) -> ~880 Hz (b) = correct order.
- `verify_export.py exports/m3-ui-export.mp4 b:0:4 a:1:3` -> 8/8 PASS.
- QuickTime Player playback (osascript): duration 6.0; after 3s current time 2.79s, playing=true; then
  reached 6.0 and stopped. Real-time playback OK. (QuickTime window screenshot unavailable off-Space.)
- `cargo test --lib` 13/13, `npm test` 8/8, `npm run build` OK.
- test-media SHA256 a/b/c OK after all exports; 0 `.partial-` files. ~/Projects contains only ai-video-edit.
Not covered: owner's own imported video was only read (no baseline hash taken); app code never writes
  sources (render refuses source paths). Windows/other codecs untested. No cancel during export.

## M3 - Real FFmpeg render pipeline (DONE 2026-10-22)
Design (`src-tauri/src/render.rs`, command `export_video` in lib.rs):
- Backend re-probes every source and re-validates in/out (UI values not trusted); output format = first
  clip's size (even-rounded) + fps; others scaled/letterboxed. One FFmpeg run: per-input `-ss/-t`,
  filter_complex scale/pad/fps + concat; clips without audio get `anullsrc` silence; no audio at all -> `-an`.
  libx264 veryfast crf20 yuv420p + AAC 160k, +faststart.
- Discrete args via `Command` (no shell). Renders to hidden `.<name>.partial-*.mp4` beside output, probes it,
  then renames. Temp removed on failure. Errors return last 8 FFmpeg stderr lines.
- Overwrite: refused unless confirmed by native save panel (`overwriteConfirmed`; false if we appended .mp4).
  Writing onto a source file is always refused. Progress: `-progress pipe:1` -> `export-progress` event.
- UI: Export enabled only when render plan is valid -> save dialog -> modal with progress bar, result
  (duration/resolution/audio/size), Show in Finder, or error text.
Verified (actual):
- `cargo test --lib` -> 13 passed, 0 failed. Includes `real_export_end_to_end`: real FFmpeg renders single
  trimmed clip (1.5s, 640x360, audio) and 2-clip concat (no-audio 320x240 clip first + audio clip) -> 3.5s
  320x240 with audio; refuses existing output; refuses writing onto source; sources byte-identical; no temp left.
- `npm test` 8/8, `npm run build` OK, debug .app rebuilt.
- UI export via computer use: test clips imported, clip-b moved first, clip-a trimmed 1->3, Export ->
  `exports/m3-ui-export.mp4`. Modal: progress shown, then "Export complete 0:06.00 1280x720 audio 1.0 MB".
- Independent inspection: ffprobe h264 1280x720 30fps yuv420p 6.000s + aac 48k stereo 6.000s, 1,007,801 B;
  `ffmpeg -f null` full decode with no errors; frame@2s = SMPTE bars (clip-b), frame@5s = testsrc2 (clip-a);
  audio ~879 Hz in 0-4s (clip-b 880), ~439 Hz in 4-6s (clip-a 440) -> order + trim correct.
- test-media checksums OK after export; no `.partial-` leftovers.
Limitations: no cancel button; always re-encodes; output size follows first clip (presets are P1).
Not yet done: playback of the exported file in a player (decode verified only) - M4 covers it.

## M2 - Timeline + trim model (DONE 2026-10-22)
Gate MET - interactive check by Claude via computer use (full-screen control approved by owner):
- Native picker import of test-media clip-a + clip-b -> probed clip-a: 0:05.00, 1280x720, h264, 30fps, audio, 2.0MB.
- Owner's own video (already imported by owner) added 3x; trim #3 to 2->5 -> length 3.00, total 1:09.06 -> 0:49.04.
- Earlier x2 moved it to #1; plan line showed `1. ...mp4 2.00->5.00` first.
- Out=99 -> "Out point cannot be after the clip end (23.02s)"; plan + total unchanged.
- Preview of trimmed clip started at 2s and paused at 00:05 (= out point).
- Final: removed owner-video clips from timeline (file untouched), clip-b before clip-a, clip-a trimmed 1->3:
  plan = `1. clip-b.mp4 0.00->4.00  2. clip-a.mp4 1.00->3.00`, total 0:06.00. Exactly as intended.
- Fix: timeline block labels clipped vertically -> track height 56->64px (frontend rebuilt; .app not yet rebuilt).
- test-media checksums after test: OK.
Limitation: background (off-Space) automation can't type into the webview or open the picker; needs full control.

### M2 build notes
- `src/timeline.ts`: TimelineClip {id, sourcePath, sourceDuration, inPoint, outPoint}; order = array index.
  validateTrim (0 <= in, out <= duration, out-in >= 0.1s, finite), setTrim, moveClip, removeClip,
  toRenderPlan -> ordered segments (what M3 export will consume). Same source may be added twice.
- UI: + adds media to timeline; clip blocks sized by trimmed length; In/Out inputs + "set at playhead";
  Earlier/Later/Remove; preview of a timeline clip is confined to its in/out; live render-plan readout.
- Tests: `npm test` (node --test, Node native TS, no new deps) -> 8 passed, 0 failed.
  `npm run build` OK. Debug .app rebuilt + launched. test-media checksums: OK (unchanged).
NOT yet verified: interactive UI check of add/reorder/trim (owner had the file dialog open; I stopped).
Decision: owner asked about auto-editing; kept deferred per runbook (free local options noted for after M5:
  FFmpeg silencedetect/scdet, local Whisper; LLM clip picking needs paid API).

Decisions:
- Scaffolded with `create-tauri-app@4 --template react-ts` in scratch, copied in with `cp -n` (no overwrites).
- Backend `src-tauri/src/media.rs`: ffprobe via `Command` + discrete args, extension allowlist
  (mp4/mov/m4v/webm/mkv), absolute-path + exists checks. Looks in /opt/homebrew/bin first (Finder PATH).
- Preview via asset protocol with EMPTY static scope; each probed file is allowed individually.
- UI: dark grid (top bar / media bin / preview / properties / timeline). Export + timeline LABELED as
  not implemented (M2/M3).
- Test clips: `test-media/clip-a.mp4` (5s 1280x720 h264+aac), `clip-b.mp4` (4s), SHA256SUMS recorded.
Verified (actual):
- `npm run build` -> OK (tsc + vite).
- `cargo test --lib` -> 6 passed, 0 failed (incl. real ffmpeg-generated clip probe).
- `npm run tauri dev` -> compiled, process ran. `npm run tauri build -- --debug --bundles app` -> OK,
  `src-tauri/target/debug/bundle/macos/AI Reel Maker.app`; launched; screenshot shows dark workspace.
NOT yet verified: Import via native file dialog + playback in window. Blocker: owner's screen is on a
  full-screen Space; window can't be moved there for automation.
Next: owner switches to a normal desktop Space (or imports test-media/clip-a.mp4 by hand) -> confirm
  metadata + playback -> M1 gate -> M2.
