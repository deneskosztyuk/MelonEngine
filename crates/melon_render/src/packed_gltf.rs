//! Import a glTF that lives in the asset pack. A `.glb` has everything inside;
//! a `.gltf` may point at `.bin` buffers and image files next to it. Those
//! are packed too, so they are read through the `AssetServer` ahead of the
//! import (`external_files`) and handed to `import_with`.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use gltf::image::{Data as ImageData, Format, Source as ImageSource};

/// `%20` -> ` ` (glTF URIs are URL-encoded).
pub(crate) fn percent_decode(uri: &str) -> Result<String> {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .with_context(|| format!("bad % escape in {uri}"))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).with_context(|| format!("{uri} is not UTF-8"))
}

/// Pack path of `uri` (relative to the model file `model`): decoded, with
/// `.` / `..` folded. `None` for data URIs and other schemes.
pub(crate) fn resolve(model: &str, uri: &str) -> Result<Option<String>> {
    if uri.contains(':') {
        return Ok(None);
    }
    let decoded = percent_decode(uri)?;
    let mut parts: Vec<&str> = model.split('/').collect();
    parts.pop(); // the model's file name
    for part in decoded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("{uri} climbs out of the project");
                }
            }
            p => parts.push(p),
        }
    }
    Ok(Some(parts.join("/")))
}

/// Every external buffer / image file of the `.gltf` `bytes` (model path
/// `model`), read with `read`. Empty for a `.glb` without external files.
pub(crate) fn external_files(
    model: &str,
    bytes: &[u8],
    read: &dyn Fn(&str) -> std::io::Result<Vec<u8>>,
) -> Result<HashMap<String, Vec<u8>>> {
    let document = gltf::Gltf::from_slice(bytes)
        .with_context(|| format!("failed to parse glTF {model}"))?
        .document;
    let mut uris = Vec::new();
    for buffer in document.buffers() {
        if let gltf::buffer::Source::Uri(uri) = buffer.source() {
            uris.push(uri.to_string());
        }
    }
    for image in document.images() {
        if let ImageSource::Uri { uri, .. } = image.source() {
            uris.push(uri.to_string());
        }
    }
    let mut files = HashMap::new();
    for uri in uris {
        if let Some(path) = resolve(model, &uri)?
            && !files.contains_key(&path)
        {
            let data = read(&path).with_context(|| format!("{model} needs {path}"))?;
            files.insert(path, data);
        }
    }
    Ok(files)
}

/// `gltf::import_slice` that also resolves external buffers and images from
/// `files` (keys are pack paths, see `external_files`).
pub(crate) fn import_with(
    model: &str,
    bytes: &[u8],
    files: &HashMap<String, Vec<u8>>,
) -> Result<(
    gltf::Document,
    Vec<gltf::buffer::Data>,
    Vec<gltf::image::Data>,
)> {
    let gltf::Gltf { document, blob } =
        gltf::Gltf::from_slice(bytes).with_context(|| format!("failed to parse glTF {model}"))?;
    let mut blob = blob;
    let mut buffers = Vec::new();
    for buffer in document.buffers() {
        let data = match buffer.source() {
            gltf::buffer::Source::Uri(uri) => match resolve(model, uri)? {
                Some(path) => {
                    let mut data = files
                        .get(&path)
                        .with_context(|| format!("{path} was not read"))?
                        .clone();
                    while data.len() % 4 != 0 {
                        data.push(0);
                    }
                    gltf::buffer::Data(data)
                }
                None => gltf::buffer::Data::from_source_and_blob(buffer.source(), None, &mut None)
                    .with_context(|| format!("buffer {} of {model}", buffer.index()))?,
            },
            source => gltf::buffer::Data::from_source_and_blob(source, None, &mut blob)
                .with_context(|| format!("buffer {} of {model}", buffer.index()))?,
        };
        if data.len() < buffer.length() {
            bail!("buffer {} of {model} is too short", buffer.index());
        }
        buffers.push(data);
    }
    let mut images = Vec::new();
    for image in document.images() {
        let data = match image.source() {
            ImageSource::Uri { uri, .. } => match resolve(model, uri)? {
                Some(path) => {
                    let encoded = files
                        .get(&path)
                        .with_context(|| format!("{path} was not read"))?;
                    let decoded = image::load_from_memory(encoded)
                        .with_context(|| format!("cannot decode {path}"))?
                        .into_rgba8();
                    ImageData {
                        width: decoded.width(),
                        height: decoded.height(),
                        pixels: decoded.into_raw(),
                        format: Format::R8G8B8A8,
                    }
                }
                // data: URIs and buffer views decode without touching files.
                None => ImageData::from_source(
                    image.source(),
                    Some(std::path::Path::new(".")),
                    &buffers,
                )
                .with_context(|| format!("image {} of {model}", image.index()))?,
            },
            source => ImageData::from_source(source, None, &buffers)
                .with_context(|| format!("image {} of {model}", image.index()))?,
        };
        images.push(data);
    }
    Ok((document, buffers, images))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_resolve_relative_to_the_model_and_decode_percent_escapes() {
        let at = |m, u| resolve(m, u).unwrap();
        assert_eq!(
            at("assets/m/a.gltf", "a.bin").as_deref(),
            Some("assets/m/a.bin")
        );
        assert_eq!(
            at("assets/m/a.gltf", "tex%20one/b.png").as_deref(),
            Some("assets/m/tex one/b.png")
        );
        assert_eq!(
            at("assets/m/a.gltf", "../shared/c.bin").as_deref(),
            Some("assets/shared/c.bin")
        );
        assert_eq!(at("a.gltf", "./x.bin").as_deref(), Some("x.bin"));
        assert_eq!(
            at("a.gltf", "data:application/octet-stream;base64,AAAA"),
            None
        );
        assert!(resolve("a.gltf", "../../x.bin").is_err());
        assert!(resolve("a.gltf", "bad%zz.bin").is_err());
    }
}
