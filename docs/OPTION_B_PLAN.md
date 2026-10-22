# Option B — Claude API-assisted automatic editing: audit & proposal (2026-10-22)

Status: **M6 done (local pipeline, $0, no network).** M7 (the paid call) needs separate approval. Results are in WORKLOG.md.

Changes from the proposal, found while building M6:
- Word timing is **"dtw+energy"**: whisper.cpp DTW anchors (`-dtw small -nfa`; flash attention breaks DTW), with word
  edges at the audio energy minima between anchors. Plain Whisper timings were about 0.3s off and let an "um" through.
- The request body is smaller than estimated: about 3.2k characters of prompt and transcript plus the schema, against the ~4k-token estimate.

## 1. Audit — what exists today (verified)

| Capability | State | Evidence |
|---|---|---|
| Media import + ffprobe metadata + preview | ✅ implemented, tested | `media.rs` (6 tests), UI checks in M1/M2 |
| Timeline trim/reorder + render plan | ✅ implemented, tested | `timeline.ts` (8 tests) |
| FFmpeg render (concat, trims, silent-audio filler, safe output) | ✅ implemented, tested | `render.rs` (7 tests incl. real export), 3 UI exports |
| Export verification | ✅ implemented | `scripts/verify_export.py` (duration, decode, PSNR order/trim) |
| Transcription | ❌ none | no code; no whisper installed |
| AI / Claude integration | ❌ none | no code; no API credentials configured |
| Captions | ❌ none | and **this FFmpeg can't draw text**: no libass/freetype, so the `subtitles` and `drawtext` filters are missing |
| Vertical 9:16 output | ❌ none | output always follows the first clip's size |

Git: `main` @ `2849448` (2 commits). Only `WORKLOG.old-herk.md` is untracked.

Credentials: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_PROFILE` are all unset. There's no `~/.config/anthropic`, no `ant` CLI,
no Keychain item, and no `ANTHROPIC` lines in any shell profile. I checked for presence only; no values were read.

Local tools: whisper-cpp 1.9.5 (MIT) is available from Homebrew but not installed. macOS `say` voices
(Samantha, Daniel) are available for a synthetic speech test clip. Arial Bold is available for drawing captions.
FFmpeg has `silencedetect`, `overlay`, `crop` and `boxblur`.

## 2. Proposed pipeline (all local except one planning call)

```
import video ─► ffmpeg: 16 kHz mono WAV ─► whisper-cli (local): word timestamps ─► transcript.json
            └► ffmpeg silencedetect (local) ─► silences.json
compact prompt (transcript + metadata, no media) ─► ONE Claude API call ─► plan.raw.json
validator (Rust): timestamps, source refs, word evidence, overlaps, lengths ─► plan.validated.json
plan ─► existing render.rs segments + 9:16 framing + caption PNG overlays ─► output.mp4 + output.srt
verify_export.py (extended for 9:16 + captions) ─► verify.txt
```

- **Where it runs:** the Rust backend. The API key never reaches the webview, logs or files.
- **API client:** Rust has no official Anthropic SDK, so the call is plain HTTPS to `POST /v1/messages`
  (`x-api-key`, `anthropic-version: 2023-06-01`). It uses structured outputs
  (`output_config.format = json_schema`), so the reply must match the edit-plan schema.
- **Model:** a configurable model ID, defaulting to `claude-opus-5-5` at effort `medium`, with adaptive thinking (always on for this model).
- **Refusals:** the server-side `fallbacks: "default"` option is enabled. If the model declines, the API reroutes the
  request automatically. It's only billed if it actually triggers.
- **Request limits:** one planning call per edit, `max_tokens` 16,000, and at most one automatic retry on network or 5xx errors.
- **Validation before rendering.** Every segment must:
  - reference the known clip ID;
  - lie within 0 ≤ start < end ≤ duration, be at least 0.3s long, and not overlap another;
  - be snapped to word boundaries.

  Every removal must cite word ranges present in the transcript. Caption words must come from the transcript's words
  in that time range. Anything that fails is reported, not silently fixed.
- **Model output is only data.** Nothing it returns is run as a command; it only becomes numbers and strings in our own FFmpeg argument builder.
- **Vertical output:** 1080×1920. A wide source is scaled to fill the frame and centre-cropped. A blurred-background "fit" mode is optional.
- **Captions:** each caption is drawn as a transparent PNG in Rust (crates `ab_glyph` + `image`, system Arial Bold)
  and overlaid with `overlay=enable='between(t,a,b)'` at output-timeline times. An `.srt` sidecar is written too.
  This avoids replacing your system FFmpeg.
- **Where everything is saved:** `projects/<timestamp>-<clip>/` (git-ignored), containing
  `audio.wav, transcript.json, silences.json, request.json` (the exact body sent, with no key),
  `plan.raw.json, plan.validated.json, api_usage.json` (tokens and computed $), `render.log, output.mp4, output.srt, verify.txt`.

### Edit-plan schema (returned by Claude)

- `segments[]`: `{source_id, start, end, purpose, confidence: low|medium|high, rationale}`, in output order.
- `removals[]`: `{start, end, kind: dead_air|repeat|mistake|filler, evidence_word_ids[], confidence, rationale}`.
- `captions[]`: `{segment_index, start, end, text}`, using source-time ranges inside one segment.
- `summary`: `{title, target_duration_sec, overall_rationale, overall_confidence}`.

Confidence is an enum because structured outputs don't support numeric min/max constraints.

## 3. Exactly what is sent to Anthropic (one request per edit)

**Sent:**
- the transcript text with word-level start/end times and IDs;
- clip duration, resolution, fps and whether it has audio;
- the detected silence intervals;
- target settings (9:16, target length range, language);
- the opaque ID `clip_1`.

**Not sent:** the video, the audio, any frames, file names or paths, or device/location metadata.
Before the paid call I'll show you the exact `request.json`. Visual frame analysis isn't part of this milestone; it would
be proposed separately with its own cost and privacy review.

The transcript is your spoken content, so it leaves the machine under Anthropic's commercial API terms. Review the
Console's data-retention terms before using sensitive footage.

## 4. Billing and key setup

- **Separate billing:** the API is billed through the Claude Console (platform.claude.com). **Your Claude Pro
  subscription does not pay for API usage.** You need a Console account with a payment method or purchased credits
  (Billing page: `platform.claude.com/settings/billing`). The pricing page says new users get a small amount of free
  test credit. You do the account, billing and key steps yourself; I won't enter keys or payment details.
- **Storing the key (your step):** create a key in the Console, then store it in the macOS Keychain. This command
  prompts for the key, so it never lands in shell history:
  `security add-generic-password -a "$USER" -s ANTHROPIC_API_KEY -w`
- **How the app finds the key:** it checks the `ANTHROPIC_API_KEY` environment variable first, then that Keychain item.
  It reads `/usr/bin/security` with separate arguments, and only at call time.
- **Spending limit:** I recommend setting a low monthly spend limit for the key's workspace in the Console.

## 5. Cost estimate for one short test (live pricing checked 2026-10-22)

Test clip: about 45s of synthetic speech (around 110 words) with deliberate dead air, a repeated phrase and a fumbled word.

| | Input tokens | Output tokens (plan + thinking) |
|---|---|---|
| Estimate | ~4,000 (instructions + schema ~2.3k, transcript ~1.2k, metadata ~0.5k) | ~3,500–6,500 |
| Hard ceiling | ~4,000 | 16,000 (`max_tokens`) |

| Model | Price (in / out per MTok) | Expected per call | Ceiling per call |
|---|---|---|---|
| **Claude Opus 5.5** (default) | $4 / $20 | **≈ $0.09–0.15** | ≈ $0.34 |
| Claude Sonnet 5.5 | $2 / $10 | ≈ $0.05–0.07 | ≈ $0.17 |
| Claude Haiku 5.5 | $0.10 / $0.50 | ≈ $0.002–0.004 | ≈ $0.01 |

- **Budget:** I propose a budget of **3 paid calls maximum** for the first milestone, which is under $1 on Opus 5.5.
- **Free check first:** before the paid call I'll run the free `count_tokens` endpoint so the input count is exact.
  It needs the key, but no tokens are billed.
- **Logging:** actual usage from the response's `usage` field is logged to `api_usage.json`.

## 6. Dependencies requiring approval (all free)

1. `brew install whisper-cpp` (MIT). This also pulls in `ggml`, `llama.cpp` and `sdl2-compat`.
2. A Whisper model file, downloaded once from Hugging Face (`ggerganov/whisper.cpp`, MIT):
   - English only: `ggml-base.en.bin` (~148 MB);
   - Tagalog/Taglish or other languages: multilingual `ggml-small.bin` (~466 MB).
   It's stored outside the repo under `~/Library/Application Support/ai-reel-maker/models/`.
3. Rust crates: `ureq` (HTTPS client, rustls), `ab_glyph` + `image` (caption drawing), `base64` (not needed yet).
   No new npm packages.

## 7. Milestones

- **M6 (local only, $0):** transcription, silence detection, prompt builder, schema and validator.
  A **hand-written fixture plan** is rendered to a 9:16 MP4 with captions and verified. Unit tests cover validator rejections.
- **M7 (one approved paid call):** I show you `request.json` and the `count_tokens` result. With your approval I make the
  call, validate the plan, render, verify, and record tokens and cost.
- **M8:** an "Auto edit" button in the UI that runs the same backend path. Commit after the tests pass.
