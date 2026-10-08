use anyhow::{Context, Result};
use std::path::Path;

pub struct AsrTokenizer {
    tokenizer: tokenizers::Tokenizer,
}

impl AsrTokenizer {
    /// Load tokenizer from model directory.
    /// Expects either tokenizer.json or vocab.json + merges.txt
    pub fn from_dir(model_dir: &Path) -> Result<Self> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        if tokenizer_path.exists() {
            let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
                .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;
            return Ok(Self { tokenizer });
        }

        use tokenizers::{
            decoders::byte_level::ByteLevel as Decoder,
            models::bpe::BPE,
            pre_tokenizers::{byte_level::ByteLevel, sequence::Sequence, split::Split},
            AddedToken, SplitDelimiterBehavior,
        };
        let model = BPE::from_file(
            model_dir
                .join("vocab.json")
                .to_str()
                .context("Model path must be UTF-8")?,
            model_dir
                .join("merges.txt")
                .to_str()
                .context("Model path must be UTF-8")?,
        )
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut tokenizer = tokenizers::Tokenizer::new(model);
        let pattern = tokenizers::pre_tokenizers::split::SplitPattern::Regex(
            r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+".to_string()
        );
        tokenizer.with_pre_tokenizer(Some(Sequence::new(vec![
            Split::new(pattern, SplitDelimiterBehavior::Isolated, false)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .into(),
            ByteLevel::new(false, true, false).into(),
        ])));
        tokenizer.with_decoder(Some(Decoder::default()));
        let config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            model_dir.join("tokenizer_config.json"),
        )?)?;
        if let Some(tokens) = config["added_tokens_decoder"].as_object() {
            let mut tokens: Vec<_> = tokens
                .iter()
                .map(|(id, t)| Ok((id.parse::<u32>()?, t)))
                .collect::<Result<_>>()?;
            tokens.sort_by_key(|(id, _)| *id);
            for (id, token) in tokens {
                let content = token["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid special token"))?;
                tokenizer.add_special_tokens(&[AddedToken::from(content, true).normalized(false)]);
                anyhow::ensure!(
                    tokenizer.token_to_id(content) == Some(id),
                    "Special token ID mismatch"
                );
            }
        }
        Ok(Self { tokenizer })
    }

    /// Encode text to token IDs.
    pub fn encode(&self, text: &str) -> Result<Vec<i64>> {
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;
        Ok(encoding.get_ids().iter().map(|&id| id as i64).collect())
    }

    /// Decode token IDs to text.
    pub fn decode(&self, ids: &[i64]) -> Result<String> {
        let u32_ids: Vec<u32> = ids.iter().map(|&id| id as u32).collect();
        let text = self
            .tokenizer
            .decode(&u32_ids, false)
            .map_err(|e| anyhow::anyhow!("Decoding failed: {}", e))?;
        Ok(text)
    }
}

// Special token IDs for Qwen3-ASR
pub const IM_START_TOKEN_ID: i64 = 151644;
pub const IM_END_TOKEN_ID: i64 = 151645;
pub const ENDOFTEXT_TOKEN_ID: i64 = 151643;
pub const AUDIO_START_TOKEN_ID: i64 = 151669;
pub const AUDIO_END_TOKEN_ID: i64 = 151670;
pub const AUDIO_PAD_TOKEN_ID: i64 = 151676;
pub const ASR_TEXT_TOKEN_ID: i64 = 151704;
