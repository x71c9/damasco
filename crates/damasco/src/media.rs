//! Pictures of the site: the files in an archive outside the repository,
//! named by their SHA-256, and their details in the repository, in
//! `content/media/<id>.json`, which pages refer to as `{ "media": "<id>" }`.
//!
//! The same file uploaded twice is kept once. Files never change: replacing
//! a picture is a new file. The type comes from the first bytes of the file,
//! never from its name; SVG is not accepted, as it can carry scripts.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cms::CmsError;

/// Largest picture accepted.
pub const LIMIT: usize = 20 * 1024 * 1024;

/// A kind of picture: its media type and the extension of its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub mime: &'static str,
    pub extension: &'static str,
}

/// The kind of a picture from its first bytes.
pub fn kind_of(bytes: &[u8]) -> Option<Kind> {
    let kind = |mime, extension| Some(Kind { mime, extension });
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        kind("image/jpeg", "jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        kind("image/png", "png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        kind("image/gif", "gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        kind("image/webp", "webp")
    } else if bytes.len() > 12
        && &bytes[4..8] == b"ftyp"
        && (&bytes[8..12] == b"avif" || &bytes[8..12] == b"avis")
    {
        kind("image/avif", "avif")
    } else {
        None
    }
}

fn u16_be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn u16_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u24_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

/// Width and height in pixels, read from the file's header.
pub fn size_of(kind: Kind, b: &[u8]) -> Option<(u32, u32)> {
    match kind.extension {
        "png" => Some((u32_be(b, 16)?, u32_be(b, 20)?)),
        "gif" => Some((u16_le(b, 6)?, u16_le(b, 8)?)),
        "jpg" => {
            // The first start-of-frame segment holds the size.
            let mut at = 2;
            while at + 9 < b.len() {
                if b[at] != 0xFF {
                    return None;
                }
                let marker = b[at + 1];
                let length = u16_be(b, at + 2)? as usize;
                if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                    return Some((u16_be(b, at + 7)?, u16_be(b, at + 5)?));
                }
                at += 2 + length;
            }
            None
        }
        "webp" => match b.get(12..16)? {
            b"VP8 " => Some((u16_le(b, 26)? & 0x3FFF, u16_le(b, 28)? & 0x3FFF)),
            b"VP8L" => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => Some((u24_le(b, 24)? + 1, u24_le(b, 27)? + 1)),
            _ => None,
        },
        "avif" => {
            // The image spatial extents property.
            let at = b.windows(4).position(|w| w == b"ispe")?;
            Some((u32_be(b, at + 8)?, u32_be(b, at + 12)?))
        }
        _ => None,
    }
}

/// The archive of files, in the data directory.
pub struct Archive {
    dir: PathBuf,
}

impl Archive {
    pub fn at(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("media"),
        }
    }

    pub fn path(&self, sha256: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{sha256}.{extension}"))
    }

    /// Keeps `bytes` in the archive, once, and returns its SHA-256.
    pub fn store(&self, bytes: &[u8], kind: Kind) -> std::io::Result<String> {
        let sha256: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let path = self.path(&sha256, kind.extension);
        if !path.exists() {
            std::fs::create_dir_all(&self.dir)?;
            let temporary = self.dir.join(format!(".{sha256}.new"));
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
        }
        Ok(sha256)
    }
}

/// The extension of a stored picture, from its media type.
pub fn extension_of(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        _ => return None,
    })
}

/// The details of every picture of the site, by name.
pub fn list(site: &Path) -> Result<Vec<Value>, CmsError> {
    let dir = site.join("content/media");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(CmsError::Io(err.to_string())),
    };
    let mut items = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| CmsError::Io(e.to_string()))?;
        let item: Value = serde_json::from_str(&text)
            .map_err(|e| CmsError::Io(format!("{}: {e}", path.display())))?;
        items.push(item);
    }
    items.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
    Ok(items)
}

pub fn read(site: &Path, id: &str) -> Result<Value, CmsError> {
    if !crate::schema::is_ulid(id) {
        return Err(CmsError::NotFound);
    }
    let text = std::fs::read_to_string(site.join(format!("content/media/{id}.json")))
        .map_err(|_| CmsError::NotFound)?;
    serde_json::from_str(&text).map_err(|e| CmsError::Io(e.to_string()))
}

/// Writes the details of a picture in their canonical form.
pub fn write(site: &Path, item: &Value) -> Result<(), CmsError> {
    let id = item["id"].as_str().unwrap_or_default();
    let dir = site.join("content/media");
    std::fs::create_dir_all(&dir).map_err(|e| CmsError::Io(e.to_string()))?;
    let keys = [
        "id", "name", "type", "bytes", "width", "height", "alt", "sha256",
    ];
    let mut ordered = serde_json::Map::new();
    for key in keys {
        ordered.insert(key.into(), item.get(key).cloned().unwrap_or(Value::Null));
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(ordered)).expect("json");
    text.push('\n');
    let temporary = dir.join(format!(".{id}.json.new"));
    std::fs::write(&temporary, text).map_err(|e| CmsError::Io(e.to_string()))?;
    std::fs::rename(&temporary, dir.join(format!("{id}.json")))
        .map_err(|e| CmsError::Io(e.to_string()))
}

/// Adds a picture: the file to the archive, its details to the site.
pub fn add(site: &Path, archive: &Archive, name: &str, bytes: &[u8]) -> Result<Value, CmsError> {
    if bytes.len() > LIMIT {
        return Err(CmsError::Refused(format!(
            "the picture is larger than {} MB",
            LIMIT / 1024 / 1024
        )));
    }
    let kind = kind_of(bytes).ok_or_else(|| {
        CmsError::Refused(
            "this is not a picture damasco accepts: JPEG, PNG, WebP, GIF or AVIF".into(),
        )
    })?;
    let (width, height) = size_of(kind, bytes).unwrap_or((0, 0));
    let sha256 = archive
        .store(bytes, kind)
        .map_err(|e| CmsError::Io(e.to_string()))?;
    let name: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    let item = json!({
        "id": crate::site::ulid(),
        "name": if name.trim().is_empty() { format!("picture.{}", kind.extension) } else { name },
        "type": kind.mime,
        "bytes": bytes.len(),
        "width": width,
        "height": height,
        "alt": "",
        "sha256": sha256,
    });
    write(site, &item)?;
    Ok(item)
}

/// Changes the description (alternative text) or the name of a picture.
pub fn update(site: &Path, id: &str, changes: &Value) -> Result<Value, CmsError> {
    let mut item = read(site, id)?;
    for (key, limit) in [("alt", 500), ("name", 120)] {
        if let Some(value) = changes.get(key) {
            let text = value
                .as_str()
                .ok_or_else(|| CmsError::Refused(format!("{key} must be text")))?;
            if text.chars().count() > limit {
                return Err(CmsError::Refused(format!(
                    "{key} is longer than {limit} characters"
                )));
            }
            if key == "name" && text.trim().is_empty() {
                return Err(CmsError::Refused("a picture needs a name".into()));
            }
            item[key] = json!(text.trim());
        }
    }
    write(site, &item)?;
    Ok(item)
}

/// The titles of the pages, and the names of the components, that use
/// picture `id`.
pub fn used_on(site: &Path, id: &str) -> Result<Vec<String>, CmsError> {
    let cms = crate::cms::Cms::new(site);
    let mut titles = Vec::new();
    for summary in cms.list()? {
        let page = cms.read(&summary.id)?;
        if refers_to(&page, id) {
            titles.push(summary.title);
        }
    }
    // A component using it is named as such.
    for component in cms.all_components()? {
        if refers_to(&component, id) {
            let name = component["name"].as_str().unwrap_or_default();
            titles.push(format!("{name} (component)"));
        }
    }
    Ok(titles)
}

/// Whether `value` refers to picture `id` anywhere.
pub fn refers_to(value: &Value, id: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.get("media").and_then(Value::as_str) == Some(id)
                || object.values().any(|v| refers_to(v, id))
        }
        Value::Array(items) => items.iter().any(|v| refers_to(v, id)),
        _ => false,
    }
}

/// Removes the details of a picture no page uses. The file stays in the
/// archive: earlier versions and releases may still show it.
pub fn delete(site: &Path, id: &str) -> Result<Value, CmsError> {
    let item = read(site, id)?;
    let pages = used_on(site, id)?;
    if !pages.is_empty() {
        return Err(CmsError::Refused(format!(
            "it is used on {}; take it out of those pages first",
            pages.join(", ")
        )));
    }
    std::fs::remove_file(site.join(format!("content/media/{id}.json")))
        .map_err(|e| CmsError::Io(e.to_string()))?;
    Ok(item)
}

/// Puts the files of the site's pictures in a release, under
/// `_damasco/media/`: hard links into the archive where possible.
pub fn publish(site: &Path, archive: &Archive, release: &Path) -> Result<usize, String> {
    let items = list(site).map_err(|e| e.to_string())?;
    let dir = release.join("_damasco/media");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut count = 0;
    for item in items {
        let (Some(sha256), Some(extension)) = (
            item["sha256"].as_str(),
            item["type"].as_str().and_then(extension_of),
        ) else {
            continue;
        };
        if !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let source = archive.path(sha256, extension);
        let target = dir.join(format!("{sha256}.{extension}"));
        if target.exists() || !source.is_file() {
            continue;
        }
        if std::fs::hard_link(&source, &target).is_err() {
            std::fs::copy(&source, &target).map_err(|e| e.to_string())?;
        }
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG of 3 × 2 pixels.
    pub fn png() -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&3u32.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn kinds_and_sizes_come_from_the_bytes() {
        let kind = kind_of(&png()).unwrap();
        assert_eq!(kind.mime, "image/png");
        assert_eq!(size_of(kind, &png()), Some((3, 2)));
        let gif = b"GIF89a\x05\x00\x07\x00".to_vec();
        assert_eq!(size_of(kind_of(&gif).unwrap(), &gif), Some((5, 7)));
        // A JPEG with an APP0 segment, then the frame: 640 × 480.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01,
            0xE0, 0x02, 0x80, 0x03,
        ];
        assert_eq!(size_of(kind_of(&jpeg).unwrap(), &jpeg), Some((640, 480)));
        assert!(kind_of(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").is_none());
        assert!(kind_of(b"%PDF-1.7").is_none());
    }

    #[test]
    fn pictures_are_kept_once_and_used_with_care() {
        let (dir, site, home) = crate::cms::test_site();
        let archive = Archive::at(dir.path());
        let first = add(&site, &archive, "C:\\photos\\team.png", &png()).unwrap();
        let second = add(&site, &archive, "copy.png", &png()).unwrap();
        assert_eq!(first["name"], "team.png");
        assert_eq!(
            (first["width"].clone(), first["height"].clone()),
            (json!(3), json!(2))
        );
        assert_eq!(first["sha256"], second["sha256"]);
        assert_eq!(
            std::fs::read_dir(dir.path().join("media")).unwrap().count(),
            1
        );
        assert!(add(&site, &archive, "x.svg", b"<svg/>").is_err());

        let id = first["id"].as_str().unwrap();
        let changed = update(&site, id, &json!({ "alt": "The team in the studio" })).unwrap();
        assert_eq!(changed["alt"], "The team in the studio");

        // Used on a page: it stays.
        let cms = crate::cms::Cms::new(&site);
        cms.update(&home, &json!({ "blocks": [{ "id": "m1", "type": "media", "media": { "media": id }, "caption": null }] }))
            .unwrap();
        assert_eq!(used_on(&site, id).unwrap(), ["Studio"]);
        assert!(matches!(delete(&site, id), Err(CmsError::Refused(_))));
        cms.update(&home, &json!({ "blocks": [] })).unwrap();
        delete(&site, id).unwrap();
        // A page cannot show a picture that is not there.
        let missing = cms.update(&home, &json!({ "seo": { "image": { "media": id } } }));
        assert!(matches!(missing, Err(CmsError::Invalid(_))));
        assert_eq!(list(&site).unwrap().len(), 1);

        // A release gets the files of the pictures there are.
        let release = dir.path().join("release");
        assert_eq!(publish(&site, &archive, &release).unwrap(), 1);
        let sha = second["sha256"].as_str().unwrap();
        assert!(release.join(format!("_damasco/media/{sha}.png")).is_file());
    }
}
