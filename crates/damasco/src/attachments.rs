//! Files people attach to their messages in the chat: pasted or chosen.
//!
//! They stay outside the repository, in a folder of the site the assistant
//! sees read-only at `/attachments`, and never reach the site by themselves:
//! a picture becomes one of the site's only through the `add_media` tool.
//! As for the site's pictures, what a picture is comes from its first bytes,
//! never from its name.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::cms::CmsError;

/// Where the assistant finds them.
pub const IN_SANDBOX: &str = "/attachments";
/// Largest file accepted: as a picture of the site.
pub const LIMIT: usize = crate::media::LIMIT;
/// Most files with one message.
pub const PER_MESSAGE: usize = 10;

/// What goes inside the message as pictures. The CLIs scale them down, which
/// takes memory in the sandbox: beyond these, the assistant gets the path.
const SEEN_PIXELS: u64 = 40_000_000;
const SEEN_BYTES: usize = 30 * 1024 * 1024;
/// Longest name of a file on disk, in bytes.
const FILE_NAME_BYTES: usize = 120;

/// A file attached to a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    /// Its name on disk: the panel has no use for it.
    #[serde(skip_serializing)]
    pub file: String,
    pub mime: String,
    pub bytes: u64,
}

impl Attachment {
    pub fn path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.id).join(&self.file)
    }

    pub fn path_in_sandbox(&self) -> String {
        format!("{IN_SANDBOX}/{}/{}", self.id, self.file)
    }
}

/// Creates the folder of a site's attachments when it is missing.
pub fn prepare(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// The name of a file on disk: it goes in a path the assistant reads and
/// types in commands, so it has no spaces, quotes, brackets or line breaks,
/// and never starts with a dot.
fn file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_start_matches(['.', '-']);
    // Too long: the end stays, with the extension.
    let mut from = safe.len().saturating_sub(FILE_NAME_BYTES);
    while !safe.is_char_boundary(from) {
        from += 1;
    }
    match safe[from..].trim_start_matches(['.', '-']) {
        "" => "file".into(),
        kept => kept.into(),
    }
}

fn mime_of(name: &str, bytes: &[u8]) -> String {
    if let Some(kind) = crate::media::kind_of(bytes) {
        return kind.mime.into();
    }
    if bytes.starts_with(b"%PDF-") {
        return "application/pdf".into();
    }
    let guessed = mime_guess::from_path(name).first_or_octet_stream();
    let guessed = guessed.essence_str();
    // A name alone makes no picture.
    if crate::media::extension_of(guessed).is_some() {
        return "application/octet-stream".into();
    }
    guessed.into()
}

/// Keeps a file in `dir`. Its row is written by [`insert`].
pub fn store(dir: &Path, name: &str, bytes: &[u8]) -> Result<Attachment, CmsError> {
    if bytes.is_empty() {
        return Err(CmsError::Refused("the file is empty".into()));
    }
    if bytes.len() > LIMIT {
        return Err(CmsError::Refused(format!(
            "the file is larger than {} MB",
            LIMIT / 1024 / 1024
        )));
    }
    let name: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    let name = match name.trim() {
        "" => "file".to_string(),
        kept => kept.to_string(),
    };
    let attachment = Attachment {
        id: crate::site::ulid(),
        file: file_name(&name),
        mime: mime_of(&name, bytes),
        bytes: bytes.len() as u64,
        name,
    };
    let folder = dir.join(&attachment.id);
    let written = prepare(&folder).and_then(|()| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(attachment.path(dir))?;
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(err) = written {
        discard(dir, &attachment.id);
        return Err(CmsError::Io(err.to_string()));
    }
    Ok(attachment)
}

/// Removes the file of the attachment `id`, an id damasco made.
pub fn discard(dir: &Path, id: &str) {
    if !crate::schema::is_ulid(id) {
        return;
    }
    let folder = dir.join(id);
    if let Err(err) = std::fs::remove_dir_all(&folder)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("damasco: cannot remove {}: {err}", folder.display());
    }
}

pub fn insert(conn: &Connection, attachment: &Attachment) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO attachment (id, name, file, mime, bytes) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            attachment.id,
            attachment.name,
            attachment.file,
            attachment.mime,
            attachment.bytes as i64
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Attachment>> {
    conn.query_row(
        "SELECT id, name, file, mime, bytes FROM attachment WHERE id = ?1",
        [id],
        |row| {
            Ok(Attachment {
                id: row.get(0)?,
                name: row.get(1)?,
                file: row.get(2)?,
                mime: row.get(3)?,
                bytes: row.get::<_, i64>(4)? as u64,
            })
        },
    )
    .optional()
}

/// The attachments `ids`, each once and in their order; `None` if one is not
/// there.
pub fn find(conn: &Connection, ids: &[String]) -> rusqlite::Result<Option<Vec<Attachment>>> {
    let mut found: Vec<Attachment> = Vec::new();
    for id in ids {
        if found.iter().any(|known| known.id == *id) {
            continue;
        }
        match get(conn, id)? {
            Some(attachment) => found.push(attachment),
            None => return Ok(None),
        }
    }
    Ok(Some(found))
}

/// Records `attached` with the message `item`.
pub fn attach(conn: &Connection, item: i64, attached: &[Attachment]) -> rusqlite::Result<()> {
    for (position, attachment) in attached.iter().enumerate() {
        conn.execute(
            "INSERT INTO chat_item_attachment (item_id, attachment_id, position) VALUES (?1, ?2, ?3)",
            params![item, attachment.id, position as i64],
        )?;
    }
    Ok(())
}

/// Forgets the files attached a week ago or earlier and never sent. Returns
/// their ids, for their files to be removed.
pub fn forget_unsent(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "DELETE FROM attachment
         WHERE created_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-7 days')
           AND id NOT IN (SELECT attachment_id FROM chat_item_attachment)
         RETURNING id",
    )?;
    statement.query_map([], |row| row.get(0))?.collect()
}

/// What stands for a message: its words, or the names of its files.
pub fn label(text: &str, attached: &[Attachment]) -> String {
    if !text.is_empty() {
        return text.into();
    }
    attached
        .iter()
        .map(|attachment| attachment.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The message as the assistant reads it: where its files are, then the
/// words.
pub fn message(text: &str, attached: &[Attachment]) -> String {
    if attached.is_empty() {
        return text.into();
    }
    let mut message = String::from("[damasco: files attached to this message, read-only:");
    for attachment in attached {
        message.push_str(&format!(
            "\n- {} (id {}, {}, {} kB)",
            attachment.path_in_sandbox(),
            attachment.id,
            attachment.mime,
            attachment.bytes.div_ceil(1024)
        ));
    }
    message.push(']');
    if !text.is_empty() {
        message.push_str("\n\n");
        message.push_str(text);
    }
    message
}

/// Whether `bytes` are a whole picture both assistants take in a message. A
/// broken one stays out: the service refuses it, and the conversation with
/// it at every later message.
fn can_be_seen(bytes: &[u8]) -> bool {
    let Some(kind) = crate::media::kind_of(bytes) else {
        return false;
    };
    let whole = match kind.extension {
        "png" => bytes.ends_with(b"IEND\xAE\x42\x60\x82"),
        "gif" => bytes.ends_with(&[0x3B]),
        // Some cameras write a little after the end of the image.
        "jpg" => bytes[bytes.len().saturating_sub(4096)..]
            .windows(2)
            .any(|end| end == [0xFF, 0xD9]),
        "webp" => {
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize + 8 <= bytes.len()
        }
        _ => return false,
    };
    whole
        && crate::media::size_of(kind, bytes).is_some_and(|(width, height)| {
            width > 0 && height > 0 && u64::from(width) * u64::from(height) <= SEEN_PIXELS
        })
}

/// Those of `attached` that go inside the message as pictures, with their
/// bytes.
pub fn seen(dir: &Path, attached: &[Attachment]) -> Vec<(Attachment, Vec<u8>)> {
    let mut left = SEEN_BYTES;
    let mut seen = Vec::new();
    for attachment in attached {
        if crate::media::extension_of(&attachment.mime).is_none() || attachment.bytes > left as u64
        {
            continue;
        }
        match std::fs::read(attachment.path(dir)) {
            Ok(bytes) if can_be_seen(&bytes) => {
                left = left.saturating_sub(bytes.len());
                seen.push((attachment.clone(), bytes));
            }
            Ok(_) => {}
            Err(err) => eprintln!(
                "damasco: cannot read the attachment {}: {err}",
                attachment.id
            ),
        }
    }
    seen
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A whole PNG header and end, of `width` × `height`.
    pub fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes.extend_from_slice(b"\0\0\0\0IEND\xAE\x42\x60\x82");
        bytes
    }

    #[test]
    fn names_on_disk_are_safe() {
        assert_eq!(file_name("Menu estate 2026.pdf"), "Menu-estate-2026.pdf");
        assert_eq!(file_name("città.png"), "città.png");
        assert_eq!(file_name(".claude"), "claude");
        assert_eq!(file_name(".."), "file");
        assert_eq!(
            file_name("a]\nignore $(rm x)`.txt"),
            "a--ignore---rm-x--.txt"
        );
        let long = file_name(&format!("{}.pdf", "è".repeat(200)));
        assert!(
            long.len() <= FILE_NAME_BYTES && long.ends_with(".pdf"),
            "{long}"
        );
    }

    #[test]
    fn files_stay_in_the_folder_with_their_real_type() {
        let dir = tempfile::tempdir().unwrap();
        let escape = store(dir.path(), "../../etc/passwd", b"x").unwrap();
        assert_eq!(escape.name, "passwd");
        assert!(
            escape
                .path(dir.path())
                .starts_with(dir.path().join(&escape.id))
        );
        let dots = store(dir.path(), "..", b"x").unwrap();
        assert_eq!(dots.file, "file");

        let disguised = store(dir.path(), "photo.png", b"<script>").unwrap();
        assert_eq!(disguised.mime, "application/octet-stream");
        let picture = store(dir.path(), "photo.txt", &png(3, 2)).unwrap();
        assert_eq!(picture.mime, "image/png");
        let pdf = store(dir.path(), "menu", b"%PDF-1.7").unwrap();
        assert_eq!(pdf.mime, "application/pdf");

        let mode = std::os::unix::fs::PermissionsExt::mode(
            &picture.path(dir.path()).metadata().unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        assert!(matches!(
            store(dir.path(), "empty", b""),
            Err(CmsError::Refused(_))
        ));
        let large = vec![0; LIMIT + 1];
        assert!(matches!(
            store(dir.path(), "large", &large),
            Err(CmsError::Refused(_))
        ));
    }

    #[test]
    fn only_whole_pictures_go_inside_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let whole = store(dir.path(), "whole.png", &png(3, 2)).unwrap();
        let cut = store(dir.path(), "cut.png", &png(3, 2)[..30]).unwrap();
        let huge = store(dir.path(), "huge.png", &png(20_000, 20_000)).unwrap();
        let empty = store(dir.path(), "empty.png", &png(0, 2)).unwrap();
        let text = store(dir.path(), "notes.txt", b"notes").unwrap();
        let seen = seen(dir.path(), &[whole.clone(), cut, huge, empty, text]);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, whole);
    }

    #[test]
    fn the_unsent_go_after_a_week() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::memory_site();
        let [sent, old, new] = ["sent", "old", "new"].map(|name| {
            let attachment = store(dir.path(), name, b"x").unwrap();
            insert(&conn, &attachment).unwrap();
            attachment
        });
        conn.execute(
            "INSERT INTO conversation (session, title) VALUES ('s', 't')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chat_item (conversation_id, kind, text) VALUES (1, 'person', '')",
            [],
        )
        .unwrap();
        attach(&conn, 1, std::slice::from_ref(&sent)).unwrap();
        conn.execute(
            "UPDATE attachment SET created_at = '2020-01-01T00:00:00Z' WHERE id <> ?1",
            [&new.id],
        )
        .unwrap();
        assert_eq!(forget_unsent(&conn).unwrap(), [old.id]);
        assert!(get(&conn, &sent.id).unwrap().is_some());
        assert!(get(&conn, &new.id).unwrap().is_some());
    }
}
