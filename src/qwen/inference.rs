use crate::qwen::tensor::{Device, Tensor};
use anyhow::{Context, Result};
use std::path::Path;

use crate::qwen::audio_encoder::AudioEncoder;
use crate::qwen::config::AsrConfig;
use crate::qwen::layers::compute_mrope_cos_sin;
use crate::qwen::mel::WhisperFeatureExtractor;
use crate::qwen::text_decoder::{KvCache, TextDecoder, create_causal_mask};
use crate::qwen::tokenizer::{
    AUDIO_PAD_TOKEN_ID, AsrTokenizer, ENDOFTEXT_TOKEN_ID, IM_END_TOKEN_ID,
};
use crate::qwen::weights;

const MEL_SAMPLE_RATE: u32 = 16000;

/// ASR inference engine.
pub struct AsrInference {
    audio_encoder: AudioEncoder,
    text_decoder: TextDecoder,
    mel_extractor: WhisperFeatureExtractor,
    tokenizer: AsrTokenizer,
    config: AsrConfig,
    device: Device,
}

/// Reusable encoder/decoder prefix for one bounded live audio window.
/// Call reset when shifting the window or starting another recording.
pub struct WindowCache {
    enabled: bool,
    mel_max: Option<f32>,
    complete_blocks: usize,
    kv: Option<KvCache>,
}
impl Default for WindowCache {
    fn default() -> Self {
        Self {
            enabled: true,
            mel_max: None,
            complete_blocks: 0,
            kv: None,
        }
    }
}
impl WindowCache {
    pub fn reset(&mut self) {
        let enabled = self.enabled;
        *self = Self {
            enabled,
            ..Self::default()
        };
    }
}

#[derive(Default, Debug, serde::Serialize)]
pub struct Timings {
    pub mel_seconds: f64,
    pub encoder_seconds: f64,
    pub prefill_seconds: f64,
    pub decode_seconds: f64,
    pub reused_frames: usize,
    pub reused_positions: usize,
}

impl AsrInference {
    /// Load model from directory containing config.json, model.safetensors, tokenizer.json
    pub fn load(model_dir: &Path, device: Device) -> Result<Self> {
        tracing::info!("Loading model from {:?}", model_dir);

        // Load config
        let config = AsrConfig::from_file(&model_dir.join("config.json"))
            .context("Failed to load config")?;

        let quant: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            model_dir.join("quantization_config.json"),
        )?)?;
        anyhow::ensure!(
            quant["bits"] == 8 && quant["group_size"] == 64,
            "QwenNative expects the pinned 8-bit/group-64 checkpoint"
        );

        // Load weights (supports both single-file and sharded safetensors)
        let all_weights =
            weights::load_model_weights(model_dir, device).context("Failed to load weights")?;

        tracing::info!("Loaded {} weight tensors", all_weights.len());

        // Load audio encoder
        tracing::info!("Loading audio encoder...");
        let audio_encoder = AudioEncoder::load(
            &all_weights,
            "audio_tower",
            &config.thinker_config.audio_config,
            device,
        )
        .context("Failed to load audio encoder")?;

        // Load text decoder
        tracing::info!("Loading text decoder...");
        let text_decoder =
            TextDecoder::load(&all_weights, "model", &config.thinker_config.text_config)
                .context("Failed to load text decoder")?;

        // Load tokenizer
        tracing::info!("Loading tokenizer...");
        let tokenizer = AsrTokenizer::from_dir(model_dir).context("Failed to load tokenizer")?;

        // Create mel feature extractor
        let mel_extractor = WhisperFeatureExtractor::new(
            400, // n_fft
            160, // hop_length
            config.thinker_config.audio_config.num_mel_bins,
            MEL_SAMPLE_RATE,
            device,
        );

        tracing::info!("Model loaded successfully");

        Ok(Self {
            audio_encoder,
            text_decoder,
            mel_extractor,
            tokenizer,
            config,
            device,
        })
    }

    pub fn rollback_prefix(&self, ids: &[i64], count: usize) -> Result<Vec<i64>> {
        let mut end = ids.len().saturating_sub(count);
        while end > 0 && self.tokenizer.decode(&ids[..end])?.contains('\u{fffd}') {
            end -= 1;
        }
        Ok(ids[..end].to_vec())
    }

    /// Transcribe mono PCM at 16 kHz. Cancellation is checked at token boundaries.
    pub fn transcribe_samples(
        &self,
        samples: &[f32],
        language: Option<&str>,
        prefix: &[i64],
        cancelled: impl Fn() -> bool,
    ) -> Result<TranscribeResult> {
        self.transcribe_cached(
            samples,
            language,
            prefix,
            &mut WindowCache {
                enabled: false,
                ..WindowCache::default()
            },
            cancelled,
        )
    }

    /// Samples belong to one audio window: append, or shorten only its uncached tail.
    /// Reset cache before replacing PCM, shifting the window, or changing language.
    pub fn transcribe_cached(
        &self,
        samples: &[f32],
        language: Option<&str>,
        prefix: &[i64],
        cache: &mut WindowCache,
        cancelled: impl Fn() -> bool,
    ) -> Result<TranscribeResult> {
        anyhow::ensure!(samples.len() >= 400, "At least 25ms PCM required");
        anyhow::ensure!(!cancelled(), "Recognition cancelled");
        let duration_seconds = samples.len() as f64 / MEL_SAMPLE_RATE as f64;

        let mut timings = Timings::default();
        let started = std::time::Instant::now();
        let mel = self.mel_extractor.extract(samples, self.device)?;
        let mel_max = mel.max().f64_value(&[]) as f32;
        let num_mel_frames = mel.size()[1] as usize;
        let block = self.config.thinker_config.audio_config.n_window_infer;
        let complete_blocks = num_mel_frames.saturating_sub(1) / block;
        // STFT reflects the waveform end. Cache only blocks with a later frame,
        // and invalidate when global log-mel normalization changes.
        if cache.mel_max != Some(mel_max) || cache.complete_blocks > complete_blocks {
            cache.reset();
            cache.mel_max = Some(mel_max);
        }
        let first_frame = cache.complete_blocks * block;
        let tokens_per_block = self.audio_encoder.get_output_length(block);
        let cached_tokens = cache.complete_blocks * tokens_per_block;
        let mel = mel.to_dtype(crate::qwen::tensor::DType::Float16);
        mel.eval();
        timings.mel_seconds = started.elapsed().as_secs_f64();
        timings.reused_frames = first_frame;
        let started = std::time::Instant::now();
        let mut features = Vec::new();
        for offset in (first_frame..num_mel_frames).step_by(block) {
            anyhow::ensure!(!cancelled(), "Recognition cancelled");
            let frames = block.min(num_mel_frames - offset);
            let features_part = self
                .audio_encoder
                .forward_window(&mel.narrow(1, offset as i64, frames as i64), offset > 0);
            features_part.eval();
            features.push(features_part);
        }
        let audio_embeds = Tensor::cat(&features, 0);
        let num_audio_tokens = cached_tokens + audio_embeds.size()[0] as usize;
        timings.encoder_seconds = started.elapsed().as_secs_f64();
        anyhow::ensure!(!cancelled(), "Recognition cancelled");
        let started = std::time::Instant::now();

        // Step 4: Build input token sequence
        let (input_ids, audio_positions) = self.build_prompt(num_audio_tokens, language, prefix)?;
        let total_seq_len = input_ids.len();
        let split = cache.kv.as_ref().map_or(0, |kv| kv.seq_len() as usize);
        let input_ids = &input_ids[split..];
        let seq_len = input_ids.len();
        timings.reused_positions = split;

        // Step 5: Build embeddings with audio injection
        let input_tensor = Tensor::from_slice_i64(input_ids).to_device(self.device);
        let embedded = self.text_decoder.embed(&input_tensor).unsqueeze(0);
        let start = audio_positions[0].saturating_sub(split) as i64;
        let after = start + audio_embeds.size()[0];
        let hidden_states = Tensor::cat(
            &[
                embedded.narrow(1, 0, start),
                audio_embeds.unsqueeze(0),
                embedded.narrow(1, after, seq_len as i64 - after),
            ],
            1,
        );

        // Step 6: Precompute MRoPE cos/sin for all positions (prefill + max decode)
        let text_config = &self.config.thinker_config.text_config;
        let max_new_tokens = (samples.len() / 16000 * 24 + 128).min(4096);
        // Precompute enough positions for prefill + a generous decode budget
        let max_total_positions = total_seq_len + max_new_tokens;
        let all_positions: Vec<i64> = (0..max_total_positions as i64).collect();
        let all_pos_ids: [Vec<i64>; 3] =
            [all_positions.clone(), all_positions.clone(), all_positions];
        let (all_cos, all_sin) = compute_mrope_cos_sin(
            &all_pos_ids,
            text_config.head_dim,
            text_config.rope_theta,
            &text_config.mrope_section(),
            text_config.mrope_interleaved(),
            self.device,
        );

        // Prefill cos/sin: positions 0..seq_len
        let cos = all_cos.narrow(0, split as i64, seq_len as i64);
        let sin = all_sin.narrow(0, split as i64, seq_len as i64);

        // Step 7: Prefill
        let mask = create_causal_mask(seq_len as i64, split as i64, self.device);
        let mut kv_cache = cache
            .kv
            .clone()
            .unwrap_or_else(|| KvCache::new(text_config.num_hidden_layers));

        let logits =
            self.text_decoder
                .forward(&hidden_states, &cos, &sin, &mut kv_cache, Some(&mask));
        // Eval prefill output to materialize computation graph before decode loop
        logits.eval();
        if cache.enabled && complete_blocks > cache.complete_blocks {
            cache.kv = Some(
                kv_cache.prefix((audio_positions[0] + complete_blocks * tokens_per_block) as i64),
            );
            cache.complete_blocks = complete_blocks;
        }
        timings.prefill_seconds = started.elapsed().as_secs_f64();
        let started = std::time::Instant::now();

        // Step 8: Autoregressive generation
        let mut generated_ids: Vec<i64> = prefix.to_vec();
        let mut ended = false;
        let eos_token_ids = [ENDOFTEXT_TOKEN_ID, IM_END_TOKEN_ID];

        let mut next_logits = logits.squeeze_dim(1);

        let mut current_pos = total_seq_len;

        for _ in 0..max_new_tokens {
            anyhow::ensure!(!cancelled(), "Recognition cancelled");
            let next_token = next_logits.argmax(-1, false).int64_value(&[0]);

            if eos_token_ids.contains(&next_token) {
                ended = true;
                break;
            }

            generated_ids.push(next_token);

            let next_input = Tensor::from_slice_i64(&[next_token]).to_device(self.device);
            let next_hidden = self.text_decoder.embed(&next_input).unsqueeze(0);

            // Index into precomputed cos/sin for this position
            let new_cos = all_cos.narrow(0, current_pos as i64, 1);
            let new_sin = all_sin.narrow(0, current_pos as i64, 1);

            // Single-token decode: causal mask is all-zeros (no masking needed)
            next_logits =
                self.text_decoder
                    .forward(&next_hidden, &new_cos, &new_sin, &mut kv_cache, None);
            next_logits = next_logits.squeeze_dim(1);

            next_logits.eval();
            current_pos += 1;
        }

        anyhow::ensure!(ended, "Qwen output reached token limit");

        timings.decode_seconds = started.elapsed().as_secs_f64();

        // Step 9: Parse output
        tracing::info!("Generated {} tokens", generated_ids.len());
        let raw_text = self.tokenizer.decode(&generated_ids)?;
        tracing::debug!("Raw output: {:?}", raw_text);
        let (language_detected, transcription) = parse_asr_output(&raw_text, language.is_some());

        Ok(TranscribeResult {
            text: transcription,
            language: language_detected,
            raw_output: raw_text,
            duration_seconds,
            token_ids: generated_ids,
            timings,
        })
    }

    fn build_prompt(
        &self,
        num_audio_tokens: usize,
        language: Option<&str>,
        prefix_ids: &[i64],
    ) -> Result<(Vec<i64>, Vec<usize>)> {
        let mut tokens: Vec<i64> = vec![
            151644, // <|im_start|>
            8948,   // system
            198,    // \n
            151645, // <|im_end|>
            198,    // \n
            151644, // <|im_start|>
            872,    // user
            198,    // \n
            151669, // <|audio_start|>
        ];

        let audio_start_pos = tokens.len();
        tokens.extend(std::iter::repeat_n(AUDIO_PAD_TOKEN_ID, num_audio_tokens));
        let audio_positions: Vec<usize> =
            (audio_start_pos..audio_start_pos + num_audio_tokens).collect();

        tokens.extend_from_slice(&[
            151670, // <|audio_end|>
            151645, // <|im_end|>
            198,    // \n
            151644, // <|im_start|>
        ]);

        if let Some(lang) = language {
            tokens.push(77091); // assistant
            tokens.push(198); // \n
            let prefix = format!("language {}", capitalize_first(lang));
            tokens.extend(self.tokenizer.encode(&prefix)?);
            tokens.push(crate::qwen::tokenizer::ASR_TEXT_TOKEN_ID);
        } else {
            tokens.push(77091); // assistant
            tokens.push(198); // \n
        }

        tokens.extend_from_slice(prefix_ids);
        Ok((tokens, audio_positions))
    }
}

/// Result of ASR transcription.
#[derive(Debug)]
pub struct TranscribeResult {
    pub text: String,
    pub language: String,
    pub raw_output: String,
    pub duration_seconds: f64,
    pub token_ids: Vec<i64>,
    pub timings: Timings,
}

fn parse_asr_output(raw: &str, language_forced: bool) -> (String, String) {
    if language_forced {
        return ("forced".to_string(), raw.trim().to_string());
    }

    let raw = raw.trim();

    if let Some(rest) = raw.strip_prefix("language ") {
        if let Some(asr_pos) = rest.find("<asr_text>") {
            let lang = rest[..asr_pos].trim().to_string();
            let text = rest[asr_pos + "<asr_text>".len()..].trim().to_string();
            return (lang, text);
        }
        let mut lang_end = 0;
        for (i, c) in rest.char_indices() {
            if c.is_whitespace() || !c.is_alphabetic() {
                lang_end = i;
                break;
            }
            lang_end = i + c.len_utf8();
        }
        if lang_end > 0 {
            let lang = rest[..lang_end].to_string();
            let text = rest[lang_end..].trim().to_string();
            return (lang, text);
        }
    }

    ("unknown".to_string(), raw.to_string())
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
