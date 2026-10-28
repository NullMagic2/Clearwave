//! Resolve legacy and Broadcast packages or validated DLL-free model exports.
use crate::{
    Error, Result,
    network::SUPPORTED_TENSOR_COUNT,
    package::{MAX_TENSORS, Model, ModelProfile, Tensor, hash},
    storage::bounded_read,
};
use safetensors::{Dtype, SafeTensors};
use std::path::{Path, PathBuf};

const MAX_MODEL_BYTES: usize = 128 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum ModelSource {
    Safetensors(PathBuf),
    Package {
        weights: PathBuf,
        decoder_data: PathBuf,
    },
    Broadcast {
        weights: PathBuf,
        decoder_data: PathBuf,
    },
}
impl ModelSource {
    pub fn resolve(location: &Path) -> Result<Self> {
        if location.is_file() {
            if crate::model_import::is_installer(location) {
                return Self::resolve(&crate::model_import::prepare(location)?);
            }
            return if location.extension().is_some_and(|ext| ext == "safetensors") {
                Ok(Self::Safetensors(location.to_owned()))
            } else {
                Err(Error::new(
                    "Select a model folder, .safetensors file, or RTX Voice / Broadcast installer",
                ))
            };
        }
        for name in ["voice-weights.safetensors", "model.safetensors"] {
            let path = location.join(name);
            if path.is_file() {
                return Ok(Self::Safetensors(path));
            }
        }
        let weights = location.join("denoiser_48k.trtpkg");
        let decoder_data = location.join("nvafxdenoiser.dll");
        if weights.is_file() && decoder_data.is_file() {
            return Ok(Self::Broadcast {
                weights,
                decoder_data,
            });
        }
        for (weights, decoder_data) in [
            ("denoiser_48k.wpkg", "NVAudioEffects.dll"),
            (
                "NvAFX/models/denoiser_48k.wpkg",
                "NvAFX/nv/NVAudioEffects.dll",
            ),
        ] {
            let weights = location.join(weights);
            let decoder_data = location.join(decoder_data);
            if weights.is_file() && decoder_data.is_file() {
                return Ok(Self::Package {
                    weights,
                    decoder_data,
                });
            }
        }
        Err(Error::new(
            "No supported model found: place voice-weights.safetensors or model.safetensors directly in the selected folder for DLL-free loading. RTX Voice packages need denoiser_48k.wpkg and NVAudioEffects.dll together, or in NvAFX/models and NvAFX/nv. Broadcast 2.3 packages need denoiser_48k.trtpkg and nvafxdenoiser.dll together. You can also select a supported installer.",
        ))
    }
    pub fn load(&self) -> Result<Model> {
        match self {
            Self::Broadcast {
                weights,
                decoder_data,
            } => crate::broadcast::load(weights, decoder_data),
            Self::Package {
                weights,
                decoder_data,
            } => Model::load(weights, decoder_data),
            Self::Safetensors(path) => {
                let bytes = bounded_read(path, MAX_MODEL_BYTES)?;
                let (tensors, payload_offset) = decode_safetensors(&bytes)?;
                if tensors.len() != SUPPORTED_TENSOR_COUNT {
                    return Err(Error::new(
                        "The supported denoising graph requires exactly 32 tensors",
                    ));
                }
                let model = Model {
                    profile: profile(&bytes)?,
                    format: "Safetensors F32 / Clearwave graph",
                    tensors,
                    package_sha256: hash(&bytes),
                    runtime_sha256: None,
                    payload_sha256: hash(&bytes[payload_offset..]),
                };
                crate::network::specification(&model)?;
                Ok(model)
            }
        }
    }
}

fn profile(bytes: &[u8]) -> Result<ModelProfile> {
    let (_, metadata) = SafeTensors::read_metadata(bytes)
        .map_err(|error| Error::new(format!("Invalid Safetensors metadata: {error}")))?;
    match metadata
        .metadata()
        .as_ref()
        .and_then(|values| values.get("model_profile"))
        .map(String::as_str)
    {
        None | Some("rtx-voice") => Ok(ModelProfile::RtxVoice),
        Some("broadcast-2.3") => Ok(ModelProfile::Broadcast23),
        Some(value) => Err(Error::new(format!("Unsupported model profile: {value}"))),
    }
}

fn decode_safetensors(bytes: &[u8]) -> Result<(Vec<Tensor>, usize)> {
    let length = bytes
        .get(..8)
        .ok_or_else(|| Error::new("Truncated Safetensors header"))?;
    let size = u64::from_le_bytes(
        length
            .try_into()
            .map_err(|_| Error::new("Invalid header size"))?,
    );
    if size > MAX_HEADER_BYTES as u64 {
        return Err(Error::new(
            "Safetensors header exceeds the 1 MiB size limit",
        ));
    }
    let safe = SafeTensors::deserialize(bytes)
        .map_err(|e| Error::new(format!("Invalid Safetensors model: {e}")))?;
    if safe.is_empty() || safe.len() > MAX_TENSORS {
        return Err(Error::new("Invalid tensor count"));
    }
    let mut tensors = Vec::new();
    for (name, view) in safe.tensors() {
        if view.dtype() != Dtype::F32 {
            return Err(Error::new(format!("Tensor {name} must contain F32 values")));
        }
        tensors.push(Tensor::decode(name, view.data())?);
    }
    tensors.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((tensors, 8 + size as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("clearwave-model-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, []).unwrap();
            path
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn discovers_root_and_nested_packages_and_prefers_dll_free_weights() {
        for (weights, data) in [
            ("denoiser_48k.wpkg", "NVAudioEffects.dll"),
            (
                "NvAFX/models/denoiser_48k.wpkg",
                "NvAFX/nv/NVAudioEffects.dll",
            ),
        ] {
            let dir = Directory::new();
            let weights = dir.file(weights);
            assert!(ModelSource::resolve(&dir.0).is_err());
            let decoder_data = dir.file(data);
            assert_eq!(
                ModelSource::resolve(&dir.0).unwrap(),
                ModelSource::Package {
                    weights,
                    decoder_data
                }
            );
            let exported = dir.file("voice-weights.safetensors");
            assert_eq!(
                ModelSource::resolve(&dir.0).unwrap(),
                ModelSource::Safetensors(exported.clone())
            );
            assert_eq!(
                ModelSource::resolve(&exported).unwrap(),
                ModelSource::Safetensors(exported)
            );
        }
        let dir = Directory::new();
        let weights = dir.file("denoiser_48k.trtpkg");
        assert!(ModelSource::resolve(&dir.0).is_err());
        let decoder_data = dir.file("nvafxdenoiser.dll");
        assert_eq!(
            ModelSource::resolve(&dir.0).unwrap(),
            ModelSource::Broadcast {
                weights,
                decoder_data
            }
        );
        let dir = Directory::new();
        let exported = dir.file("model.safetensors");
        assert_eq!(
            ModelSource::resolve(&dir.0).unwrap(),
            ModelSource::Safetensors(exported)
        );
    }
    fn fixture(dtype: &str, shape: Vec<usize>, offsets: [usize; 2], data: &[u8]) -> Vec<u8> {
        let mut header =
            serde_json::to_vec(&json!({"w":{"dtype":dtype,"shape":shape,"data_offsets":offsets}}))
                .unwrap();
        while !header.len().is_multiple_of(8) {
            header.push(b' ');
        }
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend(header);
        bytes.extend(data);
        bytes
    }
    #[test]
    fn validates_f32_shapes_offsets_finiteness_and_truncation() {
        let values = [1.25f32, -2.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        let bytes = fixture("F32", vec![2], [0, 8], &values);
        assert_eq!(
            decode_safetensors(&bytes).unwrap().0[0].values,
            [1.25, -2.0]
        );
        for end in 0..bytes.len() {
            assert!(decode_safetensors(&bytes[..end]).is_err(), "offset {end}");
        }
        assert!(decode_safetensors(&fixture("F16", vec![4], [0, 8], &values)).is_err());
        assert!(decode_safetensors(&fixture("F32", vec![3], [0, 8], &values)).is_err());
        assert!(decode_safetensors(&fixture("F32", vec![2], [4, 12], &values)).is_err());
        assert!(
            decode_safetensors(&fixture("F32", vec![1], [0, 4], &f32::NAN.to_le_bytes())).is_err()
        );
        assert!(decode_safetensors(&u64::MAX.to_le_bytes()).is_err());
    }
    #[test]
    fn exported_weights_round_trip_through_official_reader() {
        let dir = Directory::new();
        let path = dir.0.join("voice-weights.safetensors");
        let model = Model {
            profile: ModelProfile::RtxVoice,
            format: "test",
            tensors: vec![Tensor {
                name: "w".into(),
                values: vec![1.25, -2.0],
            }],
            package_sha256: String::new(),
            runtime_sha256: None,
            payload_sha256: String::new(),
        };
        crate::export::safetensors(&model, &path).unwrap();
        assert_eq!(
            profile(&std::fs::read(&path).unwrap()).unwrap(),
            ModelProfile::RtxVoice
        );
        let decoded = decode_safetensors(&std::fs::read(&path).unwrap())
            .unwrap()
            .0;
        assert_eq!(decoded[0].name, "w");
        assert_eq!(decoded[0].values, model.tensors[0].values);
        assert!(ModelSource::Safetensors(path).load().is_err());
        let path = dir.0.join("broadcast.safetensors");
        let model = Model {
            profile: ModelProfile::Broadcast23,
            ..model
        };
        crate::export::safetensors(&model, &path).unwrap();
        assert_eq!(
            profile(&std::fs::read(&path).unwrap()).unwrap(),
            ModelProfile::Broadcast23
        );
    }
}
