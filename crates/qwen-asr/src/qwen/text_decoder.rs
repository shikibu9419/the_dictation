// Derived from second-state/qwen3_asr_rs; Apache-2.0. See NOTICE.
use crate::qwen::tensor::{DType, Device, Tensor};
use anyhow::Result;
use std::collections::HashMap;

use crate::qwen::config::TextDecoderConfig;
use crate::qwen::layers::{Linear, RmsNorm, TextDecoderLayer};

/// KV cache for autoregressive generation.
#[derive(Clone)]
pub struct KvCache {
    pub layers: Vec<Option<(Tensor, Tensor)>>,
}

impl KvCache {
    pub fn new(num_layers: usize) -> Self {
        let mut layers = Vec::with_capacity(num_layers);
        for _ in 0..num_layers {
            layers.push(None);
        }
        Self { layers }
    }

    pub fn get(&self, layer: usize) -> Option<&(Tensor, Tensor)> {
        self.layers[layer].as_ref()
    }

    pub fn set(&mut self, layer: usize, cache: (Tensor, Tensor)) {
        self.layers[layer] = Some(cache);
    }

    pub fn prefix(&self, length: i64) -> Self {
        assert!(length <= self.seq_len());
        Self {
            layers: self
                .layers
                .iter()
                .map(|layer| {
                    layer.as_ref().map(|(k, v)| {
                        let k = k.narrow(2, 0, length);
                        let v = v.narrow(2, 0, length);
                        k.eval();
                        v.eval();
                        (k, v)
                    })
                })
                .collect(),
        }
    }

    pub fn seq_len(&self) -> i64 {
        self.layers[0]
            .as_ref()
            .map(|(k, _)| k.size()[2])
            .unwrap_or(0)
    }
}

/// Qwen3 Text Decoder model.
pub struct TextDecoder {
    embed_tokens: Linear,
    layers: Vec<TextDecoderLayer>,
    norm: RmsNorm,
    lm_head: Linear,
    config: TextDecoderConfig,
}

impl TextDecoder {
    pub fn load(
        weights: &HashMap<String, Tensor>,
        prefix: &str,
        config: &TextDecoderConfig,
    ) -> Result<Self> {
        let embed_tokens = Linear::load(weights, &format!("{}.embed_tokens", prefix))?;

        let mut layers = Vec::new();
        for i in 0..config.num_hidden_layers {
            let layer = TextDecoderLayer::load(
                weights,
                &format!("{}.layers.{}", prefix, i),
                config.num_attention_heads,
                config.num_key_value_heads,
                config.head_dim,
                config.rms_norm_eps,
            )?;
            layers.push(layer);
        }

        let norm = RmsNorm::load(weights, &format!("{}.norm", prefix), config.rms_norm_eps)?;

        let head_prefix = if weights.contains_key("lm_head.weight") {
            "lm_head".to_owned()
        } else if config.tie_word_embeddings {
            format!("{}.embed_tokens", prefix)
        } else {
            anyhow::bail!("Missing untied lm_head");
        };
        let lm_head = Linear::load(weights, &head_prefix)?;

        Ok(Self {
            embed_tokens,
            layers,
            norm,
            lm_head,
            config: config.clone(),
        })
    }

    pub fn embed(&self, input_ids: &Tensor) -> Tensor {
        self.embed_tokens.embedding(input_ids)
    }

    pub fn forward(
        &self,
        hidden_states: &Tensor,
        cos: &Tensor,
        sin: &Tensor,
        kv_cache: &mut KvCache,
        mask: Option<&Tensor>,
    ) -> Tensor {
        let mut hidden = hidden_states.shallow_clone();

        for (i, layer) in self.layers.iter().enumerate() {
            let cache = kv_cache.get(i);
            let (h, new_cache) = layer.forward(&hidden, cos, sin, cache, mask);
            kv_cache.set(i, new_cache);
            hidden = h;
        }

        let hidden = self.norm.forward(&hidden);
        self.lm_head
            .forward(&hidden.narrow(1, hidden.size()[1] - 1, 1))
    }

    pub fn config(&self) -> &TextDecoderConfig {
        &self.config
    }
}

/// Create a causal attention mask.
pub fn create_causal_mask(seq_len: i64, past_len: i64, device: Device) -> Tensor {
    let total_len = past_len + seq_len;
    let mask = Tensor::full(
        &[seq_len, total_len],
        f64::NEG_INFINITY,
        DType::Float16,
        device,
    );
    let mask = mask.triu(past_len + 1);
    mask.unsqueeze(0).unsqueeze(0)
}
