#!/usr/bin/env python3
"""Independently inspect an exported MP4 against the timeline that produced it.

Usage:
  python3 -I scripts/verify_export.py OUTPUT.mp4 SOURCE:IN:OUT [SOURCE:IN:OUT ...]

Checks: file exists and is non-empty; ffprobe finds an H.264 video stream; total duration matches the
sum of segments; every frame decodes; and for each segment, frames near its start and end match the
expected source frames (PSNR), which confirms both order and trim points. Exit code 0 = all passed.
Uses only ffmpeg/ffprobe with argument lists (no shell).
"""
import json
import os
import re
import shutil
import subprocess
import sys

DURATION_TOL = 0.15  # seconds
PSNR_MIN = 30.0      # dB; a wrong frame or wrong clip scores far lower on the test patterns


def tool(name):
    for d in ("/opt/homebrew/bin", "/usr/local/bin"):
        p = os.path.join(d, name)
        if os.path.isfile(p):
            return p
    p = shutil.which(name)
    if not p:
        sys.exit(f"{name} not found")
    return p


FFMPEG, FFPROBE = tool("ffmpeg"), tool("ffprobe")


def probe(path):
    out = subprocess.run(
        [FFPROBE, "-v", "error", "-print_format", "json", "-show_format", "-show_streams", path],
        capture_output=True, text=True, check=True,
    ).stdout
    return json.loads(out)


def psnr(output, t_out, source, t_src, w, h):
    fit = f"scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"
    r = subprocess.run(
        [FFMPEG, "-hide_banner", "-nostdin", "-ss", f"{t_out:.4f}", "-i", output, "-ss", f"{t_src:.4f}", "-i", source,
         "-filter_complex", f"[1:v]{fit},format=yuv420p[s];[0:v]setsar=1,format=yuv420p[o];[o][s]psnr",
         "-frames:v", "1", "-f", "null", "-"],
        capture_output=True, text=True,
    )
    m = re.search(r"average:(inf|[\d.]+)", r.stderr)
    if not m:
        return None
    return float("inf") if m.group(1) == "inf" else float(m.group(1))


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    output = sys.argv[1]
    segments = []
    for spec in sys.argv[2:]:
        src, a, b = spec.rsplit(":", 2)
        segments.append((src, float(a), float(b)))

    results = []

    def check(name, ok, detail):
        results.append(ok)
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}")

    size = os.path.getsize(output) if os.path.isfile(output) else 0
    check("file", size > 0, f"{size} bytes")
    if size == 0:
        return 1

    info = probe(output)
    video = next((s for s in info["streams"] if s["codec_type"] == "video"), None)
    audio = next((s for s in info["streams"] if s["codec_type"] == "audio"), None)
    check("video stream", video is not None and video.get("codec_name") == "h264",
          f"{video.get('codec_name')} {video.get('width')}x{video.get('height')} {video.get('r_frame_rate')} "
          f"{video.get('pix_fmt')}" if video else "missing")
    if not video:
        return 1
    print(f"[INFO] audio stream: {audio['codec_name']} {audio.get('sample_rate')} Hz {audio.get('channels')} ch"
          if audio else "[INFO] audio stream: none")
    w, h = video["width"], video["height"]
    num, den = (int(x) for x in video["r_frame_rate"].split("/"))
    fps = num / den

    expected = sum(b - a for _, a, b in segments)
    actual = float(info["format"]["duration"])
    check("duration", abs(actual - expected) <= DURATION_TOL, f"{actual:.3f}s (expected {expected:.3f}s)")

    dec = subprocess.run([FFMPEG, "-v", "error", "-nostdin", "-i", output, "-f", "null", "-"],
                         capture_output=True, text=True)
    check("full decode", dec.returncode == 0 and not dec.stderr.strip(),
          "no errors" if dec.returncode == 0 and not dec.stderr.strip() else dec.stderr.strip()[-300:])

    # Sample mid-frame times (avoid frame-boundary rounding) near each segment's start and end.
    offset = 3.5 / fps
    start = 0.0
    for i, (src, a, b) in enumerate(segments, 1):
        length = b - a
        for label, rel in (("start", offset), ("end", length - offset)):
            p = psnr(output, start + rel, src, a + rel, w, h)
            check(f"segment {i} {label}", p is not None and p >= PSNR_MIN,
                  f"out@{start + rel:.3f}s vs {os.path.basename(src)}@{a + rel:.3f}s PSNR "
                  f"{'n/a' if p is None else ('inf' if p == float('inf') else f'{p:.1f} dB')}")
        start += length

    passed = sum(results)
    print(f"\n{passed}/{len(results)} checks passed")
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
