"""Optional development comparison against mlx-qwen3-asr 0.4.4.
Not used, embedded, or installed by the application or native inference crate.
Pass MODEL_DIR followed by one or more mono 16 kHz PCM WAV files.
"""
import json, sys, time
import mlx.core as mx
import numpy as np
from mlx_qwen3_asr.load_models import load_model
from mlx_qwen3_asr.transcribe import transcribe
from mlx_qwen3_asr.audio import load_audio_np, log_mel_spectrogram
model_dir=sys.argv[1]
t=time.perf_counter()
model,_=load_model(model_dir)
print(json.dumps({'load_seconds':time.perf_counter()-t}),file=sys.stderr,flush=True)
for path in sys.argv[2:]:
    audio=load_audio_np(path)
    mel=log_mel_spectrogram(mx.array(audio))
    mx.eval(mel)
    np.save(path+'.mel.npy',np.array(mel))
    t=time.perf_counter()
    r=transcribe(audio,model=model,language='Japanese')
    print(json.dumps({'file':path,'seconds':time.perf_counter()-t,'audio_seconds':len(audio)/16000,'text':r.text},ensure_ascii=False),flush=True)
