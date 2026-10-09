//! Process plumbing, logging, configuration and PCM utilities shared by every desktop app.
pub mod audio_level;
pub mod config;
pub mod filter;
pub mod helper;
pub mod ipc;
pub mod output;
pub mod pcm;
#[cfg(test)]
mod tests;
