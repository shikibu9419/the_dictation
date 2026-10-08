// Derived from second-state/qwen3_asr_rs; Apache-2.0. See NOTICE.
use crate::qwen::tensor::{DType, Device, Tensor};
use anyhow::Result;
use std::collections::HashMap;

use crate::qwen::config::AudioEncoderConfig;
use crate::qwen::layers::{AudioEncoderLayer, Conv2d, LayerNorm, Linear};

/// Qwen3 ASR Audio Encoder (Whisper-style with chunk-based processing).
pub struct AudioEncoder {
    // Convolutional downsampling
    conv2d1: Conv2d,
    conv2d2: Conv2d,
    conv2d3: Conv2d,
    conv_out: Linear,

    // Positional embedding (sinusoidal, precomputed)
    positional_embedding: Tensor,

    // Transformer encoder layers
    layers: Vec<AudioEncoderLayer>,

    // Output projection
    ln_post: LayerNorm,
    proj1: Linear,
    proj2: Linear,

    config: AudioEncoderConfig,
}

impl AudioEncoder {
    pub fn load(
        weights: &HashMap<String, Tensor>,
        prefix: &str,
        config: &AudioEncoderConfig,
        device: Device,
    ) -> Result<Self> {
        let conv2d1 = Conv2d::load(weights, &format!("{}.conv2d1", prefix), [2, 2], [1, 1])?;
        let conv2d2 = Conv2d::load(weights, &format!("{}.conv2d2", prefix), [2, 2], [1, 1])?;
        let conv2d3 = Conv2d::load(weights, &format!("{}.conv2d3", prefix), [2, 2], [1, 1])?;
        let conv_out = Linear::load(weights, &format!("{}.conv_out", prefix))?;

        let mut layers = Vec::new();
        for i in 0..config.encoder_layers {
            let layer = AudioEncoderLayer::load(
                weights,
                &format!("{}.layers.{}", prefix, i),
                config.encoder_attention_heads,
                config.d_model as usize,
            )?;
            layers.push(layer);
        }

        let ln_post = LayerNorm::load(weights, &format!("{}.ln_post", prefix), 1e-5)?;
        let proj1 = Linear::load(weights, &format!("{}.proj1", prefix))?;
        let proj2 = Linear::load(weights, &format!("{}.proj2", prefix))?;

        // Create sinusoidal positional embedding
        let positional_embedding = create_sinusoidal_embedding(
            config.max_source_positions,
            config.d_model as usize,
            device,
        )
        .to_dtype(DType::Float16);

        Ok(Self {
            conv2d1,
            conv2d2,
            conv2d3,
            conv_out,
            positional_embedding,
            layers,
            ln_post,
            proj1,
            proj2,
            config: config.clone(),
        })
    }

    /// Encode mel spectrogram features into continuous audio embeddings.
    pub fn forward(&self, mel_features: &Tensor) -> Tensor {
        self.forward_window(mel_features, false)
    }

    /// Encode an attention window cut out of a larger recording. A short tail
    /// still needs the padding of the full recording's conv chunk batch.
    pub fn forward_window(&self, mel_features: &Tensor, full_chunks_precede: bool) -> Tensor {
        let num_frames = mel_features.size()[1] as usize;

        // Chunk size = n_window * 2
        let chunk_size = self.config.n_window * 2;

        // Split mel into chunks
        let num_full_chunks = num_frames / chunk_size;
        let tail_frames = num_frames % chunk_size;
        let num_chunks = num_full_chunks + if tail_frames > 0 { 1 } else { 0 };

        let device = mel_features.device();
        let mut all_valid = Vec::with_capacity(num_chunks);
        let mut chunk_valid_tokens = Vec::with_capacity(num_chunks);
        if num_full_chunks > 0 {
            let full = mel_features
                .narrow(1, 0, (num_full_chunks * chunk_size) as i64)
                .reshape(&[
                    mel_features.size()[0],
                    num_full_chunks as i64,
                    chunk_size as i64,
                ])
                .permute(&[1, 0, 2])
                .unsqueeze(1);
            let features = self.conv_features(&full);
            for i in 0..num_full_chunks {
                let f = features.get(i as i64);
                chunk_valid_tokens.push(f.size()[0] as usize);
                all_valid.push(f);
            }
        }
        if tail_frames > 0 {
            let mut tail = mel_features
                .narrow(1, (num_full_chunks * chunk_size) as i64, tail_frames as i64)
                .unsqueeze(0)
                .unsqueeze(0);
            if num_full_chunks > 0 || full_chunks_precede {
                tail = Tensor::cat(
                    &[
                        tail,
                        Tensor::zeros(
                            &[
                                1,
                                1,
                                mel_features.size()[0],
                                (chunk_size - tail_frames) as i64,
                            ],
                            DType::Float16,
                            device,
                        ),
                    ],
                    3,
                );
            }
            let f = self.conv_features(&tail).squeeze_dim(0).narrow(
                0,
                0,
                tail_frames.div_ceil(8) as i64,
            );
            chunk_valid_tokens.push(f.size()[0] as usize);
            all_valid.push(f);
        }

        // Concatenate: (total_tokens, d_model)
        let hidden = Tensor::cat(&all_valid, 0);
        let total_tokens = hidden.size()[0];

        // Add batch dim for transformer: (1, total_tokens, d_model)
        let mut hidden = hidden.unsqueeze(0);

        // Build windowed attention mask
        let mask = self.build_window_mask(total_tokens, &chunk_valid_tokens, device);

        // Transformer encoder layers with windowed attention
        for layer in &self.layers {
            hidden = layer.forward(&hidden, mask.as_ref());
        }

        // Output projection: LN -> Linear -> GELU -> Linear
        let hidden = self.ln_post.forward(&hidden);
        let hidden = self.proj1.forward(&hidden).gelu();
        let hidden = self.proj2.forward(&hidden);

        // Remove batch dim: (num_tokens, output_dim)
        hidden.squeeze_dim(0)
    }

    fn conv_features(&self, x: &Tensor) -> Tensor {
        let x = self.conv2d1.forward(x).gelu();
        let x = self.conv2d2.forward(&x).gelu();
        let x = self.conv2d3.forward(&x).gelu();
        let (b, c, f, t) = x.size4();
        let features = self
            .conv_out
            .forward(&x.permute(&[0, 3, 1, 2]).reshape(&[b, t, c * f]));
        features + self.positional_embedding.narrow(0, 0, t).unsqueeze(0)
    }

    /// Build a block-diagonal windowed attention mask.
    fn build_window_mask(
        &self,
        total_tokens: i64,
        chunk_token_counts: &[usize],
        device: Device,
    ) -> Option<Tensor> {
        let chunk_size = self.config.n_window * 2;
        let chunks_per_window = self.config.n_window_infer / chunk_size;

        if chunks_per_window == 0 || chunk_token_counts.len() <= chunks_per_window {
            return None;
        }

        let num_windows = chunk_token_counts.len().div_ceil(chunks_per_window);

        // Build mask using where_cond: start with -inf, then zero out allowed blocks
        // Create a boolean mask indicating allowed positions
        let mut allow_data = vec![false; (total_tokens * total_tokens) as usize];

        let mut token_offset: i64 = 0;
        for w in 0..num_windows {
            let chunk_start = w * chunks_per_window;
            let chunk_end =
                std::cmp::min(chunk_start + chunks_per_window, chunk_token_counts.len());

            let window_tokens: i64 = chunk_token_counts[chunk_start..chunk_end]
                .iter()
                .map(|&c| c as i64)
                .sum();

            // Mark this window block as allowed
            for r in token_offset..token_offset + window_tokens {
                for c in token_offset..token_offset + window_tokens {
                    allow_data[(r * total_tokens + c) as usize] = true;
                }
            }

            token_offset += window_tokens;
        }

        // Build the mask tensor
        let neg_inf = Tensor::full(
            &[1, 1, total_tokens, total_tokens],
            f64::NEG_INFINITY,
            DType::Float16,
            device,
        );
        let zero = Tensor::zeros(&[1, 1, total_tokens, total_tokens], DType::Float16, device);

        // Create bool mask from data
        // For tch backend: use from_slice + reshape
        // For mlx backend: same approach

        {
            let allow_i32: Vec<i32> = allow_data.iter().map(|&b| if b { 1 } else { 0 }).collect();
            let allow_arr = crate::qwen::backend::mlx::array::MlxArray::from_i32(
                &allow_i32,
                &[1, 1, total_tokens as i32, total_tokens as i32],
            );
            // Cast to bool for where_cond
            let allow_bool = allow_arr.astype(crate::qwen::backend::mlx::ffi::mlx_dtype::MLX_BOOL);
            let mask = Tensor::from_mlx(crate::qwen::backend::mlx::ops::where_cond(
                &allow_bool,
                &zero.inner,
                &neg_inf.inner,
            ));
            Some(mask)
        }
    }

    /// Compute output token count for a given number of input frames through 3x Conv2d.
    fn feat_extract_output_length(input_frames: usize) -> usize {
        let after_conv = |len: usize| -> usize { (len - 1) / 2 + 1 };
        after_conv(after_conv(after_conv(input_frames)))
    }

    /// Get the number of output audio tokens for a given number of mel frames.
    pub fn get_output_length(&self, input_frames: usize) -> usize {
        let chunk_size = self.config.n_window * 2;
        let num_full_chunks = input_frames / chunk_size;
        let tail_frames = input_frames % chunk_size;

        let mut total = num_full_chunks * Self::feat_extract_output_length(chunk_size);
        if tail_frames > 0 {
            total += Self::feat_extract_output_length(tail_frames);
        }
        total
    }
}

/// Create sinusoidal positional embeddings.
fn create_sinusoidal_embedding(max_len: usize, dim: usize, device: Device) -> Tensor {
    let half_dim = dim / 2;
    let log_timescale_increment = (10000.0f64).ln() / (half_dim - 1) as f64;

    let mut embeddings = vec![0.0f32; max_len * dim];

    for pos in 0..max_len {
        for i in 0..half_dim {
            let inv_timescale = (-(i as f64) * log_timescale_increment).exp();
            let angle = pos as f64 * inv_timescale;
            embeddings[pos * dim + i] = angle.sin() as f32;
            embeddings[pos * dim + half_dim + i] = angle.cos() as f32;
        }
    }

    Tensor::from_slice_f32(&embeddings)
        .reshape(&[max_len as i64, dim as i64])
        .to_device(device)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires INDEX_QWEN_MODEL; numerical encoder comparison, no UI"]
    fn short_tail_matches_unsplit_encoder() {
        crate::qwen::backend::mlx::stream::init_mlx(true);
        let root = std::path::PathBuf::from(
            std::env::var_os("INDEX_QWEN_MODEL").expect("Set INDEX_QWEN_MODEL"),
        );
        let cfg = crate::qwen::config::AsrConfig::from_file(&root.join("config.json")).unwrap();
        let weights = crate::qwen::weights::load_model_weights(&root, Device::gpu()).unwrap();
        let encoder = AudioEncoder::load(
            &weights,
            "audio_tower",
            &cfg.thinker_config.audio_config,
            Device::gpu(),
        )
        .unwrap();
        for frames in [810, 899, 901] {
            let values: Vec<f32> = (0..128 * frames)
                .map(|i| (i as f64 * 0.017).sin() as f32)
                .collect();
            let mel = Tensor::from_slice_f32(&values)
                .reshape(&[128, frames as i64])
                .to_dtype(DType::Float16);
            let complete = encoder.forward(&mel);
            complete.eval();
            let split = Tensor::cat(
                &[
                    encoder.forward_window(&mel.narrow(1, 0, 800), false),
                    encoder.forward_window(&mel.narrow(1, 800, frames as i64 - 800), true),
                ],
                0,
            );
            split.eval();
            assert_eq!(complete.size(), split.size());
            let a = complete.to_vec_f32();
            let b = split.to_vec_f32();
            let max_error = a
                .iter()
                .zip(&b)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            let rms = (a
                .iter()
                .zip(&b)
                .map(|(a, b)| ((a - b) as f64).powi(2))
                .sum::<f64>()
                / a.len() as f64)
                .sqrt();
            eprintln!("frames={frames} max_error={max_error} rms={rms}");
            if frames == 810 {
                let incorrectly_unpadded = Tensor::cat(
                    &[
                        encoder.forward_window(&mel.narrow(1, 0, 800), false),
                        encoder.forward_window(&mel.narrow(1, 800, 10), false),
                    ],
                    0,
                )
                .to_vec_f32();
                let wrong_error = a
                    .iter()
                    .zip(&incorrectly_unpadded)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0_f32, f32::max);
                assert!(
                    wrong_error > 0.002,
                    "Fixture must expose missing tail padding"
                );
            }
            assert!(
                rms < 0.0001 && max_error < 0.001,
                "Window splitting must preserve encoder features within fp16 reduction error"
            );
        }
    }
}
