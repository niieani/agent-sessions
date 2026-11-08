use super::*;

#[derive(Clone)]
pub(super) struct AttachmentDescriptor {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) mime_type: String,
    pub(super) size: u64,
}

pub(super) fn collect_attachments(value: &Value, out: &mut BTreeMap<String, AttachmentDescriptor>) {
    let Some(raw) = value.as_str() else {
        return;
    };
    let Ok(values) = serde_json::from_str::<Vec<Value>>(raw) else {
        return;
    };
    for value in values {
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };
        let descriptor = AttachmentDescriptor {
            id: id.into(),
            kind: value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            mime_type: value
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            size: value
                .get("sizeBytes")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
        };
        out.insert(id.into(), descriptor);
    }
}

pub(super) fn hash_attachment_metadata(
    conn: &Connection,
    thread_id: &str,
    root: &Path,
    discriminator: &mut u64,
) -> Result<()> {
    let mut descriptors = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT payload_json FROM orchestration_v2_projection_messages
         WHERE thread_id=?1 ORDER BY created_at,message_id",
    )?;
    let mut rows = stmt.query([thread_id])?;
    while let Some(row) = rows.next()? {
        let raw = row.get_ref(0)?;
        if let ValueRef::Text(raw) = raw {
            collect_message_attachments(
                &Value::String(String::from_utf8_lossy(raw).into_owned()),
                &mut descriptors,
            )?;
        }
    }
    for descriptor in descriptors.values() {
        hash_bytes(discriminator, descriptor.id.as_bytes());
        if !matches!(descriptor.kind.as_str(), "image" | "file")
            || descriptor.size > MAX_ATTACHMENT_BYTES
            || !safe_component(&descriptor.id)
        {
            hash_bytes(discriminator, b"rejected");
            continue;
        }
        let path = root.join(format!(
            "{}{}",
            descriptor.id,
            attachment_extension(descriptor)
        ));
        match std::fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                hash_bytes(discriminator, b"regular");
                hash_bytes(discriminator, &metadata.len().to_le_bytes());
                if let Ok(modified) = metadata.modified()
                    && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
                {
                    hash_bytes(discriminator, &duration.as_nanos().to_le_bytes());
                }
            }
            Ok(_) => hash_bytes(discriminator, b"rejected"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hash_bytes(discriminator, b"missing")
            }
            Err(_) => hash_bytes(discriminator, b"unreadable"),
        }
    }
    *discriminator &= i64::MAX as u64;
    Ok(())
}

pub(super) fn has_retryable_attachment(meta: &CaptureMeta) -> bool {
    meta.native_meta.as_ref().is_some_and(|native_meta| {
        native_meta["attachments"]
            .as_array()
            .is_some_and(|attachments| {
                attachments.iter().any(|attachment| {
                    matches!(
                        attachment["status"].as_str(),
                        Some("missing" | "unreadable" | "unstable")
                    )
                })
            })
    })
}

pub(super) fn inspect_attachment(
    root: &Path,
    descriptor: &AttachmentDescriptor,
) -> (CapturedAttachment, Option<PathBuf>) {
    let status = |status| CapturedAttachment {
        record: "attachment",
        attachment_id: descriptor.id.clone(),
        status,
        size: None,
        value_type: None,
        encoding: None,
    };
    if descriptor.size > MAX_ATTACHMENT_BYTES {
        return (status("oversize"), None);
    }
    if !matches!(descriptor.kind.as_str(), "image" | "file") || !safe_component(&descriptor.id) {
        return (status("rejected"), None);
    }
    let extension = attachment_extension(descriptor);
    let path = root.join(format!("{}{}", descriptor.id, extension));
    let before = match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() && !meta.file_type().is_symlink() => meta,
        Ok(_) => return (status("rejected"), None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (status("missing"), None);
        }
        Err(_) => {
            return (status("unreadable"), None);
        }
    };
    if before.len() != descriptor.size {
        return (status("rejected"), None);
    }
    (status("captured"), Some(path))
}

pub(super) fn write_captured_attachment(
    root: &Path,
    descriptor: &AttachmentDescriptor,
    writer: &mut impl Write,
) -> Result<CapturedAttachment> {
    let (mut captured, path) = inspect_attachment(root, descriptor);
    let Some(path) = path else {
        serde_json::to_writer(&mut *writer, &captured)?;
        writer.write_all(b"\n")?;
        return Ok(captured);
    };
    let before = std::fs::symlink_metadata(&path)?;
    let mut source = match std::fs::File::open(&path) {
        Ok(source) => source,
        Err(_) => {
            captured.status = "unreadable";
            serde_json::to_writer(&mut *writer, &captured)?;
            writer.write_all(b"\n")?;
            return Ok(captured);
        }
    };
    let mut staged = tempfile::tempfile()?;
    let copied = std::io::copy(
        &mut std::io::Read::by_ref(&mut source).take(MAX_ATTACHMENT_BYTES + 1),
        &mut staged,
    )?;
    let after = std::fs::symlink_metadata(&path).ok();
    if copied != descriptor.size
        || after.as_ref().is_none_or(|after| {
            before.len() != after.len()
                || before.modified().ok() != after.modified().ok()
                || before.dev() != after.dev()
                || before.ino() != after.ino()
        })
    {
        captured.status = "unstable";
        serde_json::to_writer(&mut *writer, &captured)?;
        writer.write_all(b"\n")?;
        return Ok(captured);
    }
    captured.size = Some(copied);
    captured.value_type = Some("blob");
    captured.encoding = Some("base64-chunks");
    staged.rewind()?;
    serde_json::to_writer(&mut *writer, &captured)?;
    writer.write_all(b"\n")?;
    write_attachment_chunks(&mut staged, &mut *writer, &descriptor.id)?;
    Ok(captured)
}

pub(super) fn write_attachment_chunks(
    reader: &mut impl Read,
    writer: &mut impl Write,
    attachment_id: &str,
) -> Result<()> {
    const CHUNK: usize = 48 * 1024;
    let mut input = vec![0_u8; CHUNK + 2];
    let mut carried = 0;
    let mut index = 0;
    loop {
        let read = reader.read(&mut input[carried..])?;
        let total = carried + read;
        if read == 0 {
            if total > 0 {
                write_attachment_chunk(writer, attachment_id, index, &input[..total])?;
            }
            return Ok(());
        }
        let complete = total - total % 3;
        if complete > 0 {
            write_attachment_chunk(writer, attachment_id, index, &input[..complete])?;
            index += 1;
        }
        carried = total - complete;
        input.copy_within(complete..total, 0);
    }
}

pub(super) fn write_attachment_chunk(
    writer: &mut impl Write,
    attachment_id: &str,
    index: u64,
    bytes: &[u8],
) -> Result<()> {
    serde_json::to_writer(
        &mut *writer,
        &CapturedAttachmentChunk {
            record: "attachment-body",
            attachment_id,
            index,
            value_type: "blob",
            encoding: "base64",
            value: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    )?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub(super) fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && !value.contains(['/', '\\', '.'])
}

pub(super) fn attachment_extension(descriptor: &AttachmentDescriptor) -> Cow<'static, str> {
    if descriptor.kind == "file" {
        return Path::new(&descriptor.name)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| {
                value.len() <= 10 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .map(|value| Cow::Owned(format!(".{}", value.to_ascii_lowercase())))
            .unwrap_or(Cow::Borrowed(".bin"));
    }
    match descriptor.mime_type.to_ascii_lowercase().as_str() {
        "image/png" => Cow::Borrowed(".png"),
        "image/jpeg" => Cow::Borrowed(".jpg"),
        "image/gif" => Cow::Borrowed(".gif"),
        "image/webp" => Cow::Borrowed(".webp"),
        _ => Cow::Borrowed(".bin"),
    }
}

pub(super) fn collect_message_attachments(
    value: &Value,
    out: &mut BTreeMap<String, AttachmentDescriptor>,
) -> Result<()> {
    let payload: Value = serde_json::from_str(
        value
            .as_str()
            .context("T3 message payload_json is not text")?,
    )
    .context("incompatible T3 orchestration v2 message payload_json")?;
    if let Some(attachments) = payload.get("attachments") {
        ensure!(
            attachments.is_array(),
            "incompatible T3 v2 message attachments: expected array"
        );
        collect_attachments(&Value::String(serde_json::to_string(attachments)?), out);
    }
    Ok(())
}
