"""Tiny local Typhoon ASR service for vrc-translate.

Loads the Thai FastConformer-Transducer ONCE at startup, then transcribes each
posted utterance. The Rust app POSTs a 16 kHz mono WAV to /transcribe and gets
back {"text": "..."}. Stdlib HTTP only — no FastAPI/uvicorn dependency.

Run:  .venv\\Scripts\\python server.py   (optionally:  --port 8765 --device cpu)
"""

import argparse
import json
import os
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# The Windows console defaults to cp1252, which can't encode Thai — printing a
# Thai transcript would raise UnicodeEncodeError and 500 the request. Force UTF-8.
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass

MODEL = None
LOCK = threading.Lock()  # NeMo transcribe isn't guaranteed thread-safe; serialize

# --- Optional NLLB translation backend --------------------------------------
# A free, fully-offline alternative to DeepL/Claude, exposed at /translate. It's
# loaded LAZILY on the first /translate call, so the ASR-only path pays nothing
# for it. transformers + torch are already in the venv (NeMo deps), so this adds
# no new dependency — only a ~2.4GB model download on first use.
DEVICE = "cpu"  # set from --device in main()
MT_MODELS = {}  # model_id -> (tokenizer, model), cached after first load
MT_LOCK = threading.Lock()

# DeepL-style language code (e.g. "EN-US", "JA", "th") -> FLORES-200 code.
FLORES = {
    "th": "tha_Thai", "en": "eng_Latn", "ja": "jpn_Jpan", "zh": "zho_Hans",
    "ko": "kor_Hang", "fr": "fra_Latn", "de": "deu_Latn", "es": "spa_Latn",
    "vi": "vie_Latn", "id": "ind_Latn",
}


def _flores(code: str) -> str:
    base = code.split("-")[0].lower()
    if base not in FLORES:
        raise ValueError(f"unsupported language code for NLLB: {code!r}")
    return FLORES[base]


def load_mt(model_id: str):
    """Lazily load + cache an NLLB model by id. Caller must hold MT_LOCK."""
    cached = MT_MODELS.get(model_id)
    if cached is not None:
        return cached
    t = time.time()
    print(f"loading NLLB {model_id} on {DEVICE} (first run downloads ~2.4GB) ...", flush=True)
    from transformers import AutoModelForSeq2SeqLM, AutoTokenizer

    tok = AutoTokenizer.from_pretrained(model_id)
    model = AutoModelForSeq2SeqLM.from_pretrained(model_id).to(DEVICE).eval()
    MT_MODELS[model_id] = (tok, model)
    print(f"NLLB ready in {time.time() - t:.1f}s", flush=True)
    return tok, model


def _translate_nllb(model_id: str, text: str, src_code: str, tgt_codes):
    """Translate `text` into every target. One encode, reused across targets
    (only the decoder's forced BOS language token differs)."""
    import torch

    src = _flores(src_code)
    with MT_LOCK:
        tok, model = load_mt(model_id)
        tok.src_lang = src
        enc = tok(text, return_tensors="pt").to(DEVICE)
        outs = []
        for tgt in tgt_codes:
            bos = tok.convert_tokens_to_ids(_flores(tgt))
            with torch.no_grad():
                gen = model.generate(**enc, forced_bos_token_id=bos, max_new_tokens=128)
            outs.append(tok.batch_decode(gen, skip_special_tokens=True)[0].strip())
    return outs


def load_model(device: str):
    global MODEL
    t = time.time()
    print(f"loading typhoon-asr-realtime on {device} (first run downloads ~0.5GB) ...", flush=True)
    import nemo.collections.asr as nemo_asr

    MODEL = nemo_asr.models.ASRModel.from_pretrained(
        "typhoon-ai/typhoon-asr-realtime", map_location=device
    )
    MODEL.eval()
    # warm the graph so the first real utterance isn't slow
    try:
        import numpy as np
        import soundfile as sf

        warm = os.path.join(tempfile.gettempdir(), "typhoon_warm.wav")
        sf.write(warm, np.zeros(16000, dtype="float32"), 16000)
        _transcribe_file(warm)
        os.unlink(warm)
    except Exception as e:
        print(f"  (warmup skipped: {e})", flush=True)
    print(f"model ready in {time.time() - t:.1f}s", flush=True)


def _clean_thai(t: str) -> str:
    # The tokenizer emits a decomposed sara-am (nikhahit U+0E4D + sara aa U+0E32);
    # recombine to the precomposed ำ (U+0E33) so DeepL/VRChat render it correctly.
    return t.replace("ํา", "ำ").strip()


def _transcribe_file(path: str) -> str:
    out = MODEL.transcribe([path], verbose=False)
    if not out:
        return ""
    item = out[0]
    # NeMo returns list[str] or list[Hypothesis] depending on version
    text = item if isinstance(item, str) else getattr(item, "text", str(item))
    return _clean_thai(text)


class Handler(BaseHTTPRequestHandler):
    def _json(self, code, obj):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            # `service` lets the Rust client confirm it's talking to the Thai
            # Typhoon model and not, say, the JP scribe service on the same port.
            self._json(
                200 if MODEL is not None else 503,
                {"ready": MODEL is not None, "service": "typhoon-asr-realtime"},
            )
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        if self.path == "/transcribe":
            self._handle_transcribe()
        elif self.path == "/translate":
            self._handle_translate()
        elif self.path == "/shutdown":
            self._handle_shutdown()
        else:
            self._json(404, {"error": "not found"})

    def _handle_shutdown(self):
        # Let the UI's "Kill server" stop even a service it didn't spawn (one it
        # reused, or an orphan). Reply first, then exit from a side thread so the
        # response actually flushes before the process dies.
        self._json(200, {"stopping": True})
        print("shutdown requested — exiting", flush=True)

        def _bye():
            time.sleep(0.2)
            os._exit(0)

        threading.Thread(target=_bye, daemon=True).start()

    def _handle_transcribe(self):
        length = int(self.headers.get("Content-Length", 0))
        data = self.rfile.read(length)
        tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
        try:
            tmp.write(data)
            tmp.close()
            t = time.time()
            with LOCK:
                text = _transcribe_file(tmp.name)
            print(f"  transcribe {int((time.time() - t) * 1000)}ms -> {text!r}", flush=True)
            self._json(200, {"text": text})
        except Exception as e:
            import traceback
            traceback.print_exc()
            self._json(500, {"error": f"{type(e).__name__}: {e}"})
        finally:
            try:
                os.unlink(tmp.name)
            except OSError:
                pass

    def _handle_translate(self):
        length = int(self.headers.get("Content-Length", 0))
        try:
            req = json.loads(self.rfile.read(length) or b"{}")
            text = req.get("text", "")
            source = req.get("source", "th")
            targets = list(req.get("targets", []))
            model_id = req.get("model") or "facebook/nllb-200-distilled-600M"
            if not text.strip():
                self._json(200, {"translations": ["" for _ in targets]})
                return
            t = time.time()
            outs = _translate_nllb(model_id, text, source, targets)
            print(f"  translate {int((time.time() - t) * 1000)}ms -> {outs!r}", flush=True)
            self._json(200, {"translations": outs})
        except Exception as e:
            import traceback
            traceback.print_exc()
            self._json(500, {"error": f"{type(e).__name__}: {e}"})

    def log_message(self, *_):  # silence default per-request logging
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--device", default="cpu", help="cpu | cuda")
    args = ap.parse_args()

    global DEVICE
    DEVICE = args.device

    load_model(args.device)
    srv = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"listening on http://127.0.0.1:{args.port}  (Ctrl+C to stop)", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nbye")
        sys.exit(0)


if __name__ == "__main__":
    main()
