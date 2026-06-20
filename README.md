# vrc-translate (Typhoon ASR edition)

Live Thai voice → JP/EN → VRChat chatbox, using **Typhoon ASR** (Thai-specialized
streaming FastConformer-Transducer) instead of Whisper. Transducers don't
hallucinate/loop on short clips the way Whisper does, so short sentences are
much more accurate.

## Architecture

```
cpal mic → resample 16k → Silero VAD ─(utterance WAV, HTTP)→ Typhoon service (Python)
                                                                  │ Thai text
DeepL TH→JA/EN  ←──────────────────────────────────────────────┘
   └→ VRChat OSC chatbox
```

The Rust app has **no whisper-rs** — STT is the local Python service it POSTs to.

## Requirements

| | |
|---|---|
| **OS** | Windows 10/11. Windows-first (system-tray icon, auto-launch/kill of the Python service); not tested on Linux/macOS. |
| **VRChat** | Running, with **OSC enabled** (Action Menu → Options → OSC → Enabled). |
| **Build toolchain** | [Rust](https://rustup.rs) **1.85+** (2024 edition). The crate has **no native deps** (no whisper-rs / LLVM / cmake), so it compiles in seconds. |
| **ASR service** | **Python 3.12** — NeMo pins numpy 1.26, which has no 3.14 wheel. Deps (`typhoon-asr`) installed from `service/requirements.txt` into `service/.venv`. |
| **Disk** | **~0.5 GB** for the Typhoon ASR model on first run (plus **~2.4 GB** only if you use the offline `nllb` translation backend). |
| **RAM** | ~4 GB free for the loaded model. |
| **GPU** *(optional)* | NVIDIA GPU + CUDA build of torch for lower latency (`--device cuda`). CPU is fine for Typhoon (cold load ~10–20 s). |
| **Translation key** | A **DeepL** API key (free tier works) for the default backend, **or** an **Anthropic** key for `claude`, **or** no key at all for the offline `nllb` backend. |
| **Network** | Internet for the model download and for DeepL/Claude calls. The `nllb` backend runs fully offline after its one-time download. |

> **No toolchain needed for end users:** `powershell -ExecutionPolicy Bypass -File package.ps1` builds a standalone `dist-bundle/` (Rust app + frozen Python service) that runs with **neither Rust nor Python** installed — just drop in your `config.toml` and run.

## Run (one step)

With VRChat open + OSC enabled:

```
cd C:\dev\vrc-translate-typhoon
cargo run --release
```

The app **auto-launches the Python ASR service itself** (and stops it on exit), so
there's no second terminal. A small status window opens:

- **Health dots** — Mic / STT / DeepL / VRChat.
- **Input level meter** + a big **Mute** toggle.
- **Latest result** — the Thai it heard and the JA/EN it sent, with latencies.
- **System-tray icon** — right-click for Show/Hide · Toggle mute · Quit.
  Closing the window (✕) hides it to the tray; use **Quit** to actually exit.

On a cold start the window shows *“loading Typhoon model…”* for ~10-20s (first
run downloads ~0.5GB), then *“listening — speak Thai.”* If a service is already
running on port 8765 it's reused instead of starting a second copy.

**Headless** (old console-only behavior, no window/tray — you start the service
yourself): `cargo run --release -- --headless`

## Translation backend (DeepL vs Claude)

Set `mt_backend` in `config.toml`:

- **`deepl`** (default) — fast (~430 ms) and free. Good for clear sentences, but
  translates Thai idioms literally (e.g. `พี่น้อง` → "siblings" instead of
  "folks/everyone") and Japanese can over-interpret.
- **`claude`** — sends the utterance + recent context to Claude with a casual
  VRChat-register prompt; one call returns both JA + EN. Fixes the idiom/context
  errors DeepL can't. Needs `anthropic_key` set; small per-utterance cost.
  Defaults to `claude-haiku-4-5` (low latency) — set `claude_model` to
  `claude-sonnet-4-6` / `claude-opus-4-8` for higher quality (slower).

```toml
mt_backend   = "claude"
anthropic_key = "sk-ant-..."
claude_model  = "claude-haiku-4-5"
```

## Setup notes

- The service needs **Python 3.12** (NeMo pins numpy 1.26, which has no 3.14 wheel).
  Venv lives at `service/.venv`. Recreate with:
  `py -3.12 -m venv .venv; .venv\Scripts\pip install --index-url https://download.pytorch.org/whl/cpu torch; .venv\Scripts\pip install -r requirements.txt`
- GPU: pass `--device cuda` to `server.py` once CUDA torch is installed (optional).
