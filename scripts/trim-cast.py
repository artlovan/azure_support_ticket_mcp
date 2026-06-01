#!/usr/bin/env python3
"""
Slice + compress an asciinema v3 cast file.

Usage:
    trim-cast.py INPUT.cast OUTPUT.cast START_SEC END_SEC \\
        [--max-gap SEC] [--speed FACTOR] \\
        [--preserve-typing] [--typing-cps N] [--read-pause SEC]

START_SEC, END_SEC : absolute time window from the original recording (floats ok).
--max-gap SEC      : clamp any single inter-event delta to at most SEC (default: keep
                     original deltas). Use this to collapse long "thinking" pauses
                     without dropping any events (so TUI cursor state stays consistent).
--speed FACTOR     : multiply all deltas by FACTOR after clamping. FACTOR<1 speeds up
                     playback (e.g. 0.5 = 2x speed); FACTOR>1 slows down. Default 1.0.
--preserve-typing  : detect runs of user-typed characters (DEC mode-2026 single-char
                     events, run length >= 3) and "User selected:" picker results, and
                     keep them at readable speed even when --max-gap is aggressive.
--typing-cps N     : target typing speed in characters-per-second when --preserve-typing
                     is set. Default 12 (~83ms per char). Higher = faster typing.
--typing-paste     : instead of animating typing, collapse each burst to instant (delta=0
                     within the burst, like a paste). Requires --preserve-typing.
--read-pause SEC   : how long to pause after each typing burst and after each
                     "User selected:" event, so viewers can read what was typed/picked.
                     Default 1.5. Set to 0 to disable.
--picker-frame-sec SEC : when --preserve-typing is set, how long to hold each picker
                     arrow-key navigation frame (the ❯ row changing). Default 0.6s,
                     enough to register the highlight movement. Set to 0 to disable.

Header is preserved verbatim except for `duration`, which is updated to the new length.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

_ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[a-zA-Z]")
# A typing event = mode-2026 sync open + (cursor-hide?) + one visible char + mode-2026 close,
# with no other side effects. We accept variations because the very first char of an input
# field also sets cursor-hide; subsequent chars don't.
_TYPING_RE = re.compile(r"^\x1b\[\?2026h(?:\x1b\[\?25l)?.{0,4}[\x20-\x7e]\x1b\[\?2026l$")
# Picker navigation frames render the option list with a `❯` arrow at the highlighted row.
_PICKER_OPT_RE = re.compile(r"❯\s*\d+\.")


def _categorize(text: str) -> str:
    """Classify an output event for delta budgeting."""
    if _TYPING_RE.match(text):
        return "typing"
    clean = _ANSI_RE.sub("", text)
    if "User selected:" in clean:
        return "selection"
    if _PICKER_OPT_RE.search(clean):
        return "picker_nav"
    return "other"


def _find_typing_bursts(categories: list[str], min_run: int = 3) -> set[int]:
    """Return the set of indices that are part of a typing burst (>= min_run consecutive)."""
    in_burst: set[int] = set()
    i = 0
    n = len(categories)
    while i < n:
        if categories[i] == "typing":
            j = i
            while j < n and categories[j] == "typing":
                j += 1
            if j - i >= min_run:
                in_burst.update(range(i, j))
            i = j
        else:
            i += 1
    return in_burst


def trim(
    in_path: Path,
    out_path: Path,
    start: float,
    end: float,
    max_gap: float | None = None,
    speed: float = 1.0,
    preserve_typing: bool = False,
    typing_cps: float = 12.0,
    typing_paste: bool = False,
    read_pause: float = 1.5,
    picker_frame_sec: float = 0.6,
) -> tuple[int, int, float, float]:
    with in_path.open() as f:
        lines = f.readlines()

    if not lines:
        raise SystemExit("empty cast file")

    header = json.loads(lines[0])
    if header.get("version") not in (2, 3):
        raise SystemExit(f"unsupported asciinema version: {header.get('version')!r}")

    # First pass: parse events, capture original delta + categorize each output event.
    parsed: list[tuple[float, list]] = []  # (abs_t, ev)
    categories: list[str] = []  # parallel to parsed, only for 'o' events
    abs_t = 0.0
    for ln in lines[1:]:
        ln = ln.rstrip("\n")
        if not ln:
            continue
        try:
            ev = json.loads(ln)
        except json.JSONDecodeError:
            continue
        if not isinstance(ev, list) or len(ev) < 3:
            continue
        abs_t += ev[0]
        parsed.append((abs_t, ev))
        categories.append(_categorize(ev[2]) if ev[1] == "o" else "control")

    typing_indices = _find_typing_bursts(categories) if preserve_typing else set()
    # Indices where a typing burst ends (last index of each burst).
    burst_end_indices: set[int] = set()
    if preserve_typing:
        sorted_burst = sorted(typing_indices)
        for k, idx in enumerate(sorted_burst):
            if idx + 1 not in typing_indices:
                burst_end_indices.add(idx)
    selection_indices = {i for i, c in enumerate(categories) if c == "selection"} if preserve_typing else set()
    picker_nav_indices = {i for i, c in enumerate(categories) if c == "picker_nav"} if preserve_typing else set()

    typing_delta = 1.0 / typing_cps if typing_cps > 0 else 0.08

    kept: list[str] = []
    first_kept_t: float | None = None
    last_kept_t: float | None = None
    out_t = 0.0
    total_in = len(parsed)

    for idx, (abs_t, ev) in enumerate(parsed):
        if abs_t < start:
            continue
        if abs_t > end:
            break

        if first_kept_t is None:
            new_delta = 0.0
            first_kept_t = abs_t
        else:
            raw_gap = abs_t - last_kept_t  # equals original delta for contiguous slice
            if idx in typing_indices:
                # Inside a typing burst: paste-instant or natural cps, ignore clamp/speed.
                new_delta = 0.0 if typing_paste else typing_delta
            elif idx in picker_nav_indices:
                # Picker arrow-key navigation: hold each highlight long enough to track.
                new_delta = picker_frame_sec
            else:
                # Default clamp + speed pipeline.
                gap = raw_gap
                if max_gap is not None and gap > max_gap:
                    gap = max_gap
                new_delta = gap * speed
                # Read-pause budget: if previous event ended a typing burst, OR current
                # event is a selection result, inflate the gap so the viewer can read it.
                if read_pause > 0:
                    prev_idx = idx - 1
                    just_after_burst = prev_idx in burst_end_indices
                    is_selection = idx in selection_indices
                    if just_after_burst or is_selection:
                        new_delta = max(new_delta, read_pause)

        out_t += new_delta
        ev[0] = round(new_delta, 6)
        kept.append(json.dumps(ev, ensure_ascii=False))
        last_kept_t = abs_t

    if not kept:
        raise SystemExit(f"no events in window [{start}, {end}] (cast spans 0..{abs_t:.2f}s)")

    new_duration = round(out_t, 3)
    orig_duration = round((last_kept_t or 0.0) - (first_kept_t or 0.0), 3)
    if "duration" in header:
        header["duration"] = new_duration

    with out_path.open("w") as f:
        f.write(json.dumps(header, ensure_ascii=False) + "\n")
        for ev_line in kept:
            f.write(ev_line + "\n")

    return total_in, len(kept), new_duration, orig_duration


def main() -> None:
    args = sys.argv[1:]
    if len(args) < 4:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    in_path = Path(args[0])
    out_path = Path(args[1])
    start = float(args[2])
    end = float(args[3])
    max_gap: float | None = None
    speed = 1.0
    preserve_typing = False
    typing_cps = 12.0
    typing_paste = False
    read_pause = 1.5
    picker_frame_sec = 0.6
    i = 4
    while i < len(args):
        if args[i] == "--max-gap" and i + 1 < len(args):
            max_gap = float(args[i + 1]); i += 2
        elif args[i] == "--speed" and i + 1 < len(args):
            speed = float(args[i + 1]); i += 2
        elif args[i] == "--preserve-typing":
            preserve_typing = True; i += 1
        elif args[i] == "--typing-cps" and i + 1 < len(args):
            typing_cps = float(args[i + 1]); i += 2
        elif args[i] == "--typing-paste":
            typing_paste = True; i += 1
        elif args[i] == "--read-pause" and i + 1 < len(args):
            read_pause = float(args[i + 1]); i += 2
        elif args[i] == "--picker-frame-sec" and i + 1 < len(args):
            picker_frame_sec = float(args[i + 1]); i += 2
        else:
            print(f"unknown arg: {args[i]!r}", file=sys.stderr)
            print(__doc__, file=sys.stderr)
            sys.exit(2)
    if end <= start:
        raise SystemExit("END_SEC must be greater than START_SEC")
    total_in, total_out, duration, orig_dur = trim(
        in_path, out_path, start, end,
        max_gap=max_gap, speed=speed,
        preserve_typing=preserve_typing,
        typing_cps=typing_cps,
        typing_paste=typing_paste,
        read_pause=read_pause,
        picker_frame_sec=picker_frame_sec,
    )
    knobs = []
    if max_gap is not None:
        knobs.append(f"max-gap={max_gap}s")
    if speed != 1.0:
        knobs.append(f"speed={speed}x")
    if preserve_typing:
        if typing_paste:
            knobs.append("typing=paste")
        else:
            knobs.append(f"typing={typing_cps:.0f}cps")
        knobs.append(f"read-pause={read_pause}s")
    knob_str = f" [{', '.join(knobs)}]" if knobs else ""
    print(
        f"wrote {out_path} — {total_out}/{total_in} events kept, "
        f"window {orig_dur:.2f}s -> playback {duration:.2f}s{knob_str}"
    )


if __name__ == "__main__":
    main()
