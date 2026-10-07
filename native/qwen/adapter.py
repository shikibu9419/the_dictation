"""Index Voice: Qwen3-ASR 1.7B on MLX, JSONL speech adapter."""
import base64
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import threading

MODEL = 'moona3k/mlx-qwen3-asr-1.7b-8bit'
REVISION = '22c8abe6a6772122dda5905967d7496d1d3e8dd2'
HASHES = {'config.json': '2e74a751548b8ad7d7526d29365ad8144c345d8b412b1152d25dc6698452712f', 'merges.txt': '8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5', 'quantization_config.json': '964fe0bcf1c9cb41a0a66616e64c2bfcf93bd5581f3abd869b0e72d3dbc66154', 'tokenizer_config.json': '4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c', 'vocab.json': 'ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910', 'weights.safetensors': 'eba2bdb1ec74f5df99345f9f81492ba551f6b072eedfef564b353ef0dde90bb8'}
protocol = sys.stdout
sys.stdout = sys.stderr
output_lock = threading.Lock()


def emit(kind, **fields):
    with output_lock:
        print(json.dumps({"type": kind, **fields}, ensure_ascii=False), file=protocol, flush=True)


def load_model(root):
    import mlx.core as mx
    from mlx_qwen3_asr import Session
    return Session(str(root / "model"), dtype=mx.float16)


def digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def download(root):
    dest = root / "model"
    dest.mkdir(parents=True, exist_ok=True)
    for name, expected in HASHES.items():
        target = dest / name
        if target.is_file() and digest(target) == expected:
            continue
        partial = dest / (name + ".download")
        if not partial.is_file() or digest(partial) != expected:
            subprocess.run(["/usr/bin/curl", "--fail", "--location", "--retry", "3",
                            "--continue-at", "-", "--output", str(partial),
                            f"https://huggingface.co/{MODEL}/resolve/{REVISION}/{name}"], check=True)
        if digest(partial) != expected:
            raise ValueError(f"Qwen3-ASR checksum mismatch: {name}; remove {partial} and retry")
        partial.replace(target)
    load_model(root)
    marker = root / "ready.tmp"
    marker.write_text(REVISION)
    marker.replace(root / "ready")
    print("Qwen3-ASR MLX is ready.", flush=True)


class Input:
    def __init__(self):
        self.condition = threading.Condition()
        self.generation = 0
        self.audio = bytearray()
        self.rate = None
        self.finish = False
        self.closed = False

    def reset(self):
        self.generation += 1
        self.audio.clear()
        self.rate = None
        self.finish = False

    def read(self):
        try:
            for line in sys.stdin:
                value = json.loads(line)
                with self.condition:
                    kind = value["type"]
                    if kind == "audio":
                        rate = value["sample_rate"]
                        pcm = base64.b64decode(value["pcm"], validate=True)
                        if not isinstance(rate, int) or not 1000 <= rate <= 192000 or len(pcm) % 2:
                            raise ValueError("Invalid PCM format")
                        if self.finish or (self.rate is not None and rate != self.rate):
                            raise ValueError("Audio after finish or sample rate changed")
                        self.rate = rate
                        self.audio.extend(pcm)
                        emit("accepted")
                    elif kind == "finish":
                        if self.finish:
                            raise ValueError("Duplicate finish")
                        self.finish = True
                    elif kind == "cancel":
                        self.reset()
                        emit("cancelled")
                    else:
                        raise ValueError(f"Unknown command: {kind}")
                    self.condition.notify()
        except Exception as exc:
            emit("error", text=str(exc))
        finally:
            with self.condition:
                self.closed = True
                self.generation += 1
                self.condition.notify()


class Resampler:
    """Keep interpolation phase and the boundary sample across PCM packets."""
    def __init__(self, rate):
        import numpy as np
        self.rate = rate
        self.buffer = np.empty(0, dtype=np.float32)
        self.start = 0
        self.received = 0
        self.emitted = 0

    def feed(self, pcm, final=False):
        import numpy as np
        data = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768
        if self.rate == 16000:
            return data
        self.buffer = np.concatenate((self.buffer, data))
        self.received += len(data)
        if not len(self.buffer):
            return self.buffer.copy()
        stop = round(self.received * 16000 / self.rate) if final else max(
            0, int(np.ceil((self.received - 1) * 16000 / self.rate)))
        positions = np.arange(self.emitted, stop, dtype=np.float64) * self.rate / 16000
        result = np.interp(positions - self.start, np.arange(len(self.buffer)), self.buffer)
        self.emitted = stop
        keep_from = min(self.received - 1, int(stop * self.rate / 16000))
        self.buffer = self.buffer[max(0, keep_from - self.start):]
        self.start = keep_from
        return result.astype(np.float32)


class Cancelled(Exception):
    pass


def serve(root, language, mode):
    import time
    import numpy as np
    if mode not in ("live", "batch"):
        raise ValueError("Unknown recognition mode")
    from mlx_qwen3_asr.tokenizer import canonicalize_language
    language = canonicalize_language(language.replace("_", "-"))
    model = load_model(root)
    emit("status", text="Warming up Qwen3-ASR MLX…")
    model.transcribe(np.zeros(16000, dtype=np.float32), language=language)
    state = Input()
    threading.Thread(target=state.read, daemon=True).start()
    emit("ready")
    version = -1
    session = None
    resampler = None
    last_text = ""

    while True:
        with state.condition:
            if state.closed:
                return
            if state.generation != version:
                version = state.generation
                session, resampler, last_text = None, None, ""
            if (mode == "batch" and not state.finish) or (not state.audio and not state.finish):
                state.condition.wait()
                continue
            rate = state.rate or 16000
            # A bounded feed lets cancellation interrupt between decode turns.
            end = len(state.audio) if mode == "batch" else min(len(state.audio), rate * 2)
            pcm = bytes(state.audio[:end])
            del state.audio[:end]
            final = state.finish and not state.audio
        if resampler is None:
            resampler = Resampler(rate)
        audio = resampler.feed(pcm, final=final)
        started = time.monotonic()

        def progress(event):
            if state.closed or state.generation != version:
                raise Cancelled()
            if event["event"] == "chunk_completed":
                if event.get("truncated"):
                    raise RuntimeError("Qwen decode token limit reached; transcription is incomplete")
                start = event["chunk_offset_sec"]
                emit("status", text="Qwen full recording segment recognized",
                     segment_start=start, segment_end=start + event["chunk_duration_sec"])

        try:
            if mode == "batch":
                text = ""
                if len(audio) and np.mean(audio * audio) >= 1e-6:
                    result = model.transcribe(audio, language=language, on_progress=progress)
                    if result.truncated:
                        raise RuntimeError("Qwen final transcription was truncated")
                    text = result.text
            else:
                if session is None:
                    session = model.init_streaming(language=language, chunk_size_sec=1.0,
                                                   max_context_sec=30.0,
                                                   reuse_window_prefix=True, commit_at_silence=True)
                if len(audio):
                    session = model.feed_audio(audio, session)
                if final:
                    session = model.finish_streaming(session)
                text = session.text
        except Cancelled:
            continue
        with state.condition:
            if state.generation != version or state.closed:
                continue
            elapsed = time.monotonic() - started
            if elapsed >= .05:
                emit("status", text=f"Qwen {mode} decode {elapsed:.3f}s; pending PCM {len(state.audio)/2/rate:.3f}s")
            if final:
                emit("final", text=text.strip())
                state.reset()
            elif text != last_text:
                emit("partial", text=text.strip())
            last_text = text


if __name__ == "__main__":
    try:
        if sys.argv[1] == "--download":
            download(Path(sys.argv[2]))
        else:
            serve(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
    except Exception as exc:
        emit("error", text=f"Qwen3-ASR MLX: {exc}")
        import traceback
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
