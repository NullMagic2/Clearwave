//! Export named weights without guessing unrecovered tensor dimensions.
use crate::{Result, package::Model};
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
};

const HEADER_ALIGNMENT: usize = 8;

pub fn safetensors(model: &Model, path: &Path) -> Result<()> {
    let mut header = serde_json::Map::new();
    header.insert(
        "__metadata__".into(),
        json!({
            "format":"weights_v0",
            "layout":"flat stored order; graph dimensions are separate",
            "package_sha256":model.package_sha256,
            "payload_sha256":model.payload_sha256,
        }),
    );
    let mut offset = 0usize;
    for t in &model.tensors {
        let end = offset + std::mem::size_of_val(t.values.as_slice());
        header.insert(
            t.name.clone(),
            json!({"dtype":"F32","shape":[t.values.len()],"data_offsets":[offset,end]}),
        );
        offset = end;
    }
    let mut header = serde_json::to_vec(&header)?;
    while !header.len().is_multiple_of(HEADER_ALIGNMENT) {
        header.push(b' ');
    }
    let mut out = BufWriter::new(OpenOptions::new().write(true).create_new(true).open(path)?);
    out.write_all(&(header.len() as u64).to_le_bytes())?;
    out.write_all(&header)?;
    for t in &model.tensors {
        for v in &t.values {
            out.write_all(&v.to_le_bytes())?;
        }
    }
    out.flush()?;
    Ok(())
}
