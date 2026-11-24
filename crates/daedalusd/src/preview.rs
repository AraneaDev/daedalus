use daedalus_proto::{ProtoError, rpc::ImagePreview};
use std::path::Path;

#[cfg(not(unix))]
pub(super) fn load(_: &Path, _: &str) -> Result<ImagePreview, ProtoError> {
    Err(ProtoError::InvalidRequest(
        "Image preview requires a Unix daemon".into(),
    ))
}

#[cfg(unix)]
pub(super) fn load(root: &Path, selected: &str) -> Result<ImagePreview, ProtoError> {
    use nix::{
        fcntl::{OFlag, openat},
        sys::stat::Mode,
    };
    use std::{
        fs::File,
        io::{Cursor, Read},
        path::Component,
    };
    const MAX_BYTES: u64 = 12 * 1024 * 1024;
    let invalid = |s: &str| ProtoError::InvalidRequest(s.into());
    let path = Path::new(selected);
    if selected.len() > 4096
        || !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
    {
        return Err(invalid("Select a workspace PNG path"));
    }
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| invalid("Image is outside this workspace"))?
    } else {
        path
    };
    let mut file = File::open(root).map_err(|e| ProtoError::IoError(e.to_string()))?;
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        if part == Component::CurDir {
            continue;
        }
        let Component::Normal(name) = part else {
            return Err(invalid("Select a path within this workspace"));
        };
        let mut flags = OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
        if parts.peek().is_some() {
            flags |= OFlag::O_DIRECTORY;
        }
        file = File::from(
            openat(&file, name, flags, Mode::empty())
                .map_err(|e| ProtoError::IoError(e.to_string()))?,
        );
    }
    let meta = file
        .metadata()
        .map_err(|e| ProtoError::IoError(e.to_string()))?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Err(invalid("Preview requires a regular PNG of at most 12 MiB"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ProtoError::IoError(e.to_string()))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("PNG exceeds 12 MiB"));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| ProtoError::InvalidRequest(format!("Cannot preview PNG: {e}")))?;
    let edge = ImagePreview::MAX_EDGE;
    let thumb = image
        .thumbnail(edge.min(image.width()), edge.min(image.height()))
        .to_rgba8();
    Ok(ImagePreview {
        width: thumb.width(),
        height: thumb.height(),
        rgba: thumb.into_raw(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::load;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn workspace_png_thumbnail_rejects_missing_and_escaping_files() {
        let ws = tempfile::tempdir().unwrap();
        image::RgbaImage::from_pixel(1200, 800, image::Rgba([12, 18, 24, 255]))
            .save(ws.path().join("sample.png"))
            .unwrap();
        let image = load(ws.path(), "sample.png").unwrap();
        assert!(image.width > 0 && image.width <= 192);
        assert!(image.height > 0 && image.height <= 192);
        assert_eq!(
            image.rgba.len(),
            image.width as usize * image.height as usize * 4
        );
        assert!(
            load(ws.path(), ws.path().join("sample.png").to_str().unwrap())
                .unwrap()
                .valid()
        );
        assert!(load(ws.path(), "/elsewhere/sample.png").is_err());
        assert!(load(ws.path(), "sample.jpg").is_err());
        assert!(load(ws.path(), &"a".repeat(4097)).is_err());
        fs::write(ws.path().join("invalid.png"), b"invalid").unwrap();
        assert!(load(ws.path(), "invalid.png").is_err());
        fs::File::create(ws.path().join("huge.png"))
            .unwrap()
            .set_len(13 * 1024 * 1024)
            .unwrap();
        assert!(load(ws.path(), "huge.png").is_err());
        image::GrayImage::new(4097, 1)
            .save(ws.path().join("wide.png"))
            .unwrap();
        assert!(load(ws.path(), "wide.png").is_err());
        assert!(load(ws.path(), "missing.png").is_err());
        assert!(load(ws.path(), "../sample.png").is_err());
        symlink(ws.path().join("sample.png"), ws.path().join("link.png")).unwrap();
        assert!(load(ws.path(), "link.png").is_err());
    }
}
