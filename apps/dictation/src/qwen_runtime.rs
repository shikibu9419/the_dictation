//! Runtime discovery and the manifest produced after model verification.
//! Shared by the CLI and settings UI; neither loads a model or invokes Python.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub const MODEL: &str = "moona3k/mlx-qwen3-asr-1.7b-8bit";
pub const REVISION: &str = "22c8abe6a6772122dda5905967d7496d1d3e8dd2";
pub const PROTOCOL: u32 = 2;
pub const FILES: &[(&str, &str)] = &[
    (
        "config.json",
        "2e74a751548b8ad7d7526d29365ad8144c345d8b412b1152d25dc6698452712f",
    ),
    (
        "merges.txt",
        "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
    ),
    (
        "quantization_config.json",
        "964fe0bcf1c9cb41a0a66616e64c2bfcf93bd5581f3abd869b0e72d3dbc66154",
    ),
    (
        "tokenizer_config.json",
        "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c",
    ),
    (
        "vocab.json",
        "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    ),
    (
        "weights.safetensors",
        "eba2bdb1ec74f5df99345f9f81492ba551f6b072eedfef564b353ef0dde90bb8",
    ),
];

fn usable_binary(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        && path.with_file_name("mlx.metallib").is_file()
}
pub fn executable() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("INDEX_VOICE_QWEN_BINARY") {
        let path = PathBuf::from(path);
        ensure!(
            usable_binary(&path),
            "QwenNative override requires an executable and adjacent mlx.metallib: {}",
            path.display()
        );
        return Ok(path);
    }
    let exe = std::env::current_exe()?;
    let mut candidates = vec![exe.with_file_name("QwenNative")];
    // Cargo integration tests live in <profile>/deps, including with a custom target directory.
    if exe
        .parent()
        .is_some_and(|p| p.file_name().is_some_and(|n| n == "deps"))
        && let Some(profile) = exe.parent().and_then(Path::parent)
    {
        candidates.push(profile.join("QwenNative"));
    }
    if let Some(contents) = exe.parent().and_then(Path::parent) {
        candidates.push(contents.join("Helpers/QwenNative"));
    }
    // A distributed application must not depend on this checkout.
    if !exe
        .ancestors()
        .any(|p| p.extension().is_some_and(|e| e == "app"))
    {
        candidates
            .push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release/QwenNative"));
    }
    candidates.into_iter().find(|p| usable_binary(p)).context(
        "QwenNative or mlx.metallib missing; run cargo build --release --bins from desktop/rust, or reinstall the app",
    )
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelFile {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    pub modified_ns: u128,
}
impl ModelFile {
    pub fn verified(path: &Path, name: &str, sha256: &str) -> Result<Self> {
        let meta = path.metadata()?;
        ensure!(
            meta.is_file(),
            "Model asset is not a file: {}",
            path.display()
        );
        Ok(Self {
            name: name.into(),
            sha256: sha256.into(),
            bytes: meta.len(),
            modified_ns: meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
        })
    }
}
#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub protocol_version: u32,
    pub model: String,
    pub revision: String,
    pub files: Vec<ModelFile>,
}
impl Manifest {
    #[allow(dead_code)] // The UI validates existing manifests; setup writes them.
    pub fn new(files: Vec<ModelFile>) -> Self {
        Self {
            protocol_version: PROTOCOL,
            model: MODEL.into(),
            revision: REVISION.into(),
            files,
        }
    }
    pub fn validate(&self, root: &Path) -> Result<()> {
        ensure!(
            self.protocol_version == PROTOCOL && self.model == MODEL && self.revision == REVISION,
            "Qwen manifest version mismatch; rerun setup-qwen"
        );
        ensure!(
            self.files.len() == FILES.len(),
            "Incomplete Qwen model manifest"
        );
        for &(name, expected) in FILES {
            let file = self
                .files
                .iter()
                .find(|f| f.name == name)
                .context("Missing Qwen model asset")?;
            ensure!(
                file.sha256 == expected,
                "Qwen asset checksum manifest mismatch: {name}"
            );
            let current = ModelFile::verified(&root.join("model").join(name), name, expected)?;
            ensure!(
                *file == current,
                "Qwen model asset changed: {name}; rerun setup-qwen"
            );
        }
        Ok(())
    }
}
pub fn ready(root: &Path) -> bool {
    executable().is_ok() && model_ready(root).is_ok()
}
pub fn model_ready(root: &Path) -> Result<()> {
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(root.join("native-model.json"))
            .context("Qwen model is not verified for the native runtime; run setup-qwen")?,
    )?;
    manifest.validate(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_requires_all_pinned_assets_and_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("model")).unwrap();
        let mut files = vec![];
        for &(name, hash) in FILES {
            let path = dir.path().join("model").join(name);
            std::fs::write(&path, b"test fixture").unwrap();
            files.push(ModelFile::verified(&path, name, hash).unwrap());
        }
        let mut manifest = Manifest::new(files);
        manifest.validate(dir.path()).unwrap();
        manifest.protocol_version = 1;
        assert!(manifest.validate(dir.path()).is_err());
        manifest.protocol_version = PROTOCOL;
        manifest.files[0].sha256 = "unverified".into();
        assert!(manifest.validate(dir.path()).is_err());
        manifest.files[0].sha256 = FILES[0].1.into();
        std::fs::write(dir.path().join("model/config.json"), b"changed").unwrap();
        assert!(manifest.validate(dir.path()).is_err());
    }
    #[test]
    fn discovery_requires_executable_and_metal_resource() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("QwenNative");
        std::fs::write(&exe, b"fixture").unwrap();
        assert!(!usable_binary(&exe));
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!usable_binary(&exe));
        std::fs::write(dir.path().join("mlx.metallib"), b"fixture").unwrap();
        assert!(usable_binary(&exe));
    }
}
