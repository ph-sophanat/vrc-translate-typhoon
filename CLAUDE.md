# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Live Thai voice → JP/EN translation piped into the VRChat chatbox. A Rust app
captures the mic, segments speech, and orchestrates STT + translation + OSC. STT
is **not** in the Rust crate — it's a local Python service (`service/server.py`)
running the Typhoon ASR model (a Thai-specialized streaming FastConformer-Transducer),
which the Rust app talks to over plain HTTP. Transducers don't hallucinate/loop on
short clips the way Whisper does, so no dedup logic is needed.

## Commands

```bash
# Run the GUI app (auto-launches the Python service, opens status window + tray)
cargo run --release

# Headless mode — no window/tray; you must start service/server.py yourself
cargo run --release -- --headless

# Build only
cargo build --release

# DeepL latency benchmark (cold vs warm vs idle connection)
cargo run --release --example deepl_bench

# Package a standalone Windows bundle (Rust app + frozen Python service, no
# Rust/Python needed on target). Output in dist-bundle/.
powershell -ExecutionPolicy Bypass -File package.ps1
```

There are no Rust unit tests in this repo; verification is done by running the app
and reviewing `translation_log.txt` (per-utterance TH / primary / secondary blocks
written to the working dir).

### Python ASR service

Needs **Python 3.12** (NeMo pins numpy 1.26, no 3.14 wheel). Venv at `service/.venv`.

```bash
cd service
py -3.12 -m venv .venv
.venv\Scripts\pip install --index-url https://download.pytorch.org/whl/cpu torch
.venv\Scripts\pip install -r requirements.txt

# Run manually (the Rust app normally launches this for you)
.venv\Scripts\python server.py --port 8765 --device cpu   # or --device cuda
```

## Architecture

```
cpal mic → resample 16k → Silero VAD ─(utterance WAV, HTTP)→ Typhoon service (Python)
                                                                  │ Thai text
DeepL/Claude/NLLB TH→JA/EN ←───────────────────────────────────┘
   └→ VRChat OSC chatbox
```

**Two processes.** The Rust binary auto-launches the Python service on startup
(`src/service.rs`) and kills it on exit via a `Drop` guard. Before launching it
probes the port: an already-running Typhoon service is reused; a *foreign* ASR
service on the same port (e.g. vrc-jp-scribe) is detected via `/health` and refused
rather than transcribing Thai with the wrong model. The service self-identifies as
`"typhoon-asr-realtime"` in `/health`, and the Rust client (`src/typhoon.rs`)
verifies that string on connect.

**Threading model** (`src/main.rs`): eframe owns the main thread for the UI; the
audio→STT→translate→OSC pipeline (`src/worker.rs::run`) runs on a spawned thread.
They communicate through `src/state.rs::Shared` — lock-free atomics for hot values
(mute flag, mic level RMS) so the audio callback never blocks, and a short-lived
`Mutex<Snapshot>` for the richer per-utterance state the UI reads each frame.

**Pipeline flow** (`src/worker.rs`):
1. `src/audio.rs` — cpal capture at the device rate, downmixed to mono, then
   `StreamResampler` (rubato) to 16 kHz.
2. `src/vad.rs::Segmenter` — Silero VAD in 512-sample (32 ms) chunks; emits a
   complete utterance after ~384 ms of silence, with pre-roll so the first phoneme
   isn't clipped. Tunables are consts at the top of the file.
3. `src/typhoon.rs` — encodes f32 samples to an in-memory 16-bit PCM WAV (no crate)
   and POSTs to `/transcribe`.
4. Translation via the `Translator` enum (see below).
5. `src/osc.rs::Vrc` — sends the `primary\nsecondary` two-line result to VRChat's
   OSC chatbox, plus typing indicators.

**Translation backends** (`src/mt.rs::Translator`) selected by `mt_backend` in config:
- `deepl` (`src/translate.rs`) — two parallel API calls (one per target language),
  fast and free.
- `claude` (`src/claude.rs`) — one Messages API call returns both languages with a
  casual VRChat-register system prompt; understands idioms/context DeepL misses.
  Falls back to DeepL per-utterance on any error so the chatbox never goes blank.
  Talks to `POST /v1/messages` over raw reqwest (no official Rust SDK).
- `nllb` (`src/nllb.rs`) — free/offline NLLB-200; runs *inside the same Python
  service* via `/translate`, no extra process or key. Lazily loaded on first call.

The worker keeps a rolling window of the last 3 Thai utterances and passes them as
translation context to disambiguate dropped subjects/idioms. "Thai-only" mode
(`Shared::translate_enabled`) skips translation and sends the raw Thai transcript.

## Config

`config.toml` (next to the exe, falling back to the cwd — see
`Config::load_default`). Copy `config.example.toml` to start. Holds `deepl_key`,
`anthropic_key`, `mt_backend`, target languages, `osc_addr`, `typhoon_url`, model
ids. Note: `claude_model` defaults to `claude-haiku-4-5` (low latency); switch to
`claude-sonnet-4-6` / `claude-opus-4-8` for quality.

`package.ps1` ships `config.example.toml` as the bundle's `config.toml` — never
bundle the real key-bearing `config.toml`.

⚠️ `config.toml` holds personal API keys and is gitignored. Keep keys only in your
local `config.toml`; never print, commit, or otherwise propagate them.
