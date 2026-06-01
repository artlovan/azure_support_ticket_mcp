# Demo media

This directory holds visual assets referenced from the project's top-level
`README.md`:

- `demo.gif` — rendered demo shown inline in the README hero.
- `demo.cast` — source asciinema recording, kept so the GIF can be
  re-rendered (different size, font, trim, speed) without re-recording
  the whole session.

If you only want to *watch* the demo, the GIF in the project README is
all you need. The rest of this document is for whoever re-records or
re-renders it.

---

## Recording workflow

**Capture as text → sanitize the text → render to GIF.** This way every
sensitive value (tenant IDs, subscription IDs, ARM resource IDs, contact
details) can be redacted in a text editor before anything is rendered
to a binary format that's hard to scrub.

### Tools

```bash
brew install asciinema   # records terminal sessions as text (.cast)
brew install agg         # renders .cast files to .gif
brew install ffmpeg      # only if you want an MP4 sibling for downloads
```

### 1. Capture

```bash
asciinema rec \
  --idle-time-limit 2 \
  --cols 100 \
  --rows 28 \
  --title "azure-support-ticket-mcp — open AOAI deployment 409 status code error" \
  demo.cast
```

Why these flags:

- `--idle-time-limit 2` — caps any single pause at 2 seconds during playback.
  Without this, model-latency waits and "thinking" dead-air dominate the runtime.
- `--cols 100 --rows 28` — fixed terminal size. GitHub renders ~100 columns
  cleanly; 28 rows is enough for the Copilot CLI TUI plus a couple of status lines.
- `--title` — embedded in the cast metadata; shows up if anyone replays with
  `asciinema play`.

Then run the actual demo:

```bash
copilot --allow-tool='azure-support-ticket-mcp'
ticket this: my AOAI deployment gpt-5-nano-1 experiences 409 status code
```

Walk through the flow you want to publish. Ctrl-D to stop recording.

**Length note:** the current published demo is ~3 minutes and covers the full
ticket lifecycle (open → reply → status → close). Shorter demos make smaller
GIFs and are easier to read; longer demos can justify their size if they show
breadth that "open only" cannot.

### 2. Sanitize

The `.cast` file is a JSON-line text stream — every line of terminal output is
greppable. But there are several non-obvious gotchas a naive `sed` pass will
miss. Read this section before scrubbing.

**Gotcha 1: Copilot's TUI syntax-highlights GUIDs with alternating colors.**

A subscription ID like `00000000-0000-0000-0000-000000000001` is *not* a
contiguous string in the cast — it's split across multiple ANSI color
segments:

```text
\x1b[32m5\x1b[38;2;97;148;90mcdd4440\x1b[32m-1e91-4728\x1b[38;2;...
```

A plain `sed 's/12345678-.../00000000-.../g'` won't match. You need to
JSON-parse each event, strip ANSI, then substitute *within the JSON value*
(or write a regex that tolerates `\x1b\[[0-9;]*m` between every group).

**Gotcha 2: User typing uses DEC mode-2026 sync wrappers.**

When you type a phone number `4255550100`, the cast does *not* contain the
string `4255550100`. Each digit is wrapped in its own event:

```text
\x1b[?2026h4\x1b[?2026l
\x1b[?2026h2\x1b[?2026l
\x1b[?2026h5\x1b[?2026l
...
```

Scrub these with a regex that matches `(\x1b\[\?2026h)(\d)(\x1b\[\?2026l)`
and swaps digits one event at a time.

**Gotcha 3: TUI truncates long values with `...`.**

Tables and pickers show truncated forms like:

- `12345678-1234-1234-1234-12345...` (subscription ID, ARM-table truncation)
- `admin@MngEnvMCAP159946...` (email, dropdown truncation)
- `Administrat...` (family name, profile field truncation)

These don't match the full-string substitution. Add a separate replacement
for each truncated form, **length-matched** so the TUI's hard-coded column
widths stay aligned (use trailing spaces if your replacement is shorter).

**Gotcha 4: Resource names in box-drawn tables are hard-coded width.**

```text
│ ResourceName              │
```

If your real name is 20 chars and your replacement is 12 chars, the right
border misaligns. Pad replacements with trailing spaces to the original length.

**Replacement convention used for the current demo (reuse for consistency):**

| What | Replacement |
|---|---|
| Subscription ID | `00000000-0000-0000-0000-000000000001` |
| Tenant / session GUID | `11111111-1111-1111-1111-111111111111` |
| Ticket ID | `2500000000000001` |
| User email | `demo.user@contoso.com` |
| Login email (Entra) | `demouser@contoso.onmicrosoft.com` |
| Phone (typed digits) | `4255550100` → displays as `+1 425 555 0100` |
| Display name | `Demo User` |
| Resource names | neutral, no env labels (`aoai-acct-1`, `aoai-account-no-002`, ...) |

The `+1 425 555 0100` number is in Microsoft's official 555-line fictitious
range — safe for public demos.

**Paranoia sweep** after scrubbing — replay the cast and grep the file:

```bash
asciinema play demo.cast

grep -aoE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' demo.cast \
  | sort -u
grep -aoE '\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b' demo.cast | sort -u
grep -aoE '\+?[0-9]{10,}' demo.cast | sort -u
```

Anything that comes back and isn't a known placeholder gets added to the
substitution list. Re-run, re-sweep.

### 3. Trim and compress (optional)

The raw cast plays back at recording speed and includes every spinner frame
and every "thinking" pause. For a README hero you usually want something
tighter. Use the project's `scripts/trim-cast.py`:

```bash
scripts/trim-cast.py demo.cast demo-trimmed.cast 0 600 \
  --max-gap 0.02 \
  --speed 0.66
```

Key flags (see `scripts/trim-cast.py --help` or its docstring for the full list):

- `--max-gap SEC` — clamp every inter-event delta to at most SEC. Crushes
  spinner / "thinking" dead-air without dropping any events (dropping events
  breaks subsequent TUI cursor positioning). `0.02` is aggressive; `0.03–0.05`
  is gentler.
- `--speed FACTOR` — multiplies all remaining deltas. `0.66` = 1.5x playback.
- `--preserve-typing` — opt-in: detect user-typing bursts and picker
  navigation, hold them at readable speed even when `--max-gap` is aggressive
  elsewhere. Otherwise typing flies by too fast to follow.
- `--typing-paste` — when `--preserve-typing` is on, collapse typing bursts
  to instant ("paste mode") instead of animating character-by-character.
- `--read-pause SEC` — hold for SEC after each typing burst and each
  "User selected:" event so viewers can register what was typed/picked.
- `--picker-frame-sec SEC` — hold each arrow-key navigation frame (the `❯`
  row moving) for SEC so viewers can see the highlight change.

The published `demo.cast` is the raw recording (untrimmed). Trim happens at
render time so renders can be re-tuned without losing the source.

### 4. Render to GIF

```bash
agg \
  --theme asciinema \
  --font-size 14 \
  --speed 1.0 \
  --idle-time-limit 1.5 \
  --last-frame-duration 4 \
  --no-loop \
  demo.cast demo.gif
```

Why these flags:

- `--theme asciinema` — the default asciinema color scheme; high contrast,
  reads well on both light and dark GitHub themes.
- `--font-size 14` — readable on GitHub at native resolution; smaller = sharper
  but harder to read, larger = bigger file.
- `--idle-time-limit 1.5` — second-line defense against any pauses the cast
  scrub didn't catch.
- `--last-frame-duration 4` — holds the final frame for 4s so the GIF doesn't
  snap back to start immediately, giving readers time to take in the result.
- `--no-loop` — render plays once and stops. Looping demos are distracting
  in a README; readers can reload the page to replay.

Check the size:

```bash
ls -lh demo.gif
```

Aim for under 5 MB so GitHub renders it without complaint and the README
doesn't add huge load time. If it's larger: shorter input, smaller font,
more aggressive `--max-gap` in the trim step, or accept the size if the
content justifies it.

### 5. What to commit

- `demo.gif` — yes. README hero depends on it.
- `demo.cast` — yes. Source artifact; tiny; enables re-renders.
- `demo.mp4`, intermediate `demo-*.{cast,gif,mp4}` variants — no. Pure
  clutter; git stores binary blobs forever even after deletion.

GitHub does **not** auto-render local MP4 files in markdown — `![](demo.mp4)`
just gives you a broken link, and the `<video>` HTML tag is stripped by
GitHub's markdown sanitizer. The only way to get an inline-playing video on
a README is to drag-drop the MP4 into a GitHub issue/PR comment, copy the
resulting `user-images.githubusercontent.com/...` URL, and embed *that*. So
unless you're doing that, MP4 in the repo has no use and shouldn't be committed.
