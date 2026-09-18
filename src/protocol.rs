use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn read_message(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':')
            && key.eq_ignore_ascii_case("Content-Length")
        {
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.context("Missing Content-Length")?;
    if length > 32 * 1024 * 1024 {
        bail!("LSP message exceeds 32 MiB");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

pub async fn write_message(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

/// Incremental servers need an explicit range in the old document, even for a full replacement.
pub fn document_change(old: &str, new: &str, incremental: bool) -> Value {
    if !incremental {
        return serde_json::json!({"text":new});
    }
    let mut line = 0;
    let mut character = 0;
    let mut chars = old.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' || ch == '\n' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16();
        }
    }
    serde_json::json!({"text":new,"range":{"start":{"line":0,"character":0},"end":{"line":line,"character":character}}})
}

// LSP positions are UTF-16 code units, not UTF-8 bytes or Unicode scalar counts.
pub fn offset(text: &str, position: &Value) -> Result<usize> {
    let line = position["line"].as_u64().context("Missing line")? as usize;
    let character = position["character"]
        .as_u64()
        .context("Missing character")? as usize;
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n').context("Line out of range")? + 1;
    }
    let mut units = 0;
    for (byte, ch) in text[start..].char_indices() {
        if units == character {
            return Ok(start + byte);
        }
        if ch == '\n' || ch == '\r' {
            bail!("Character out of range");
        }
        units += ch.len_utf16();
        if units > character {
            bail!("Position splits a UTF-16 surrogate pair");
        }
    }
    if units == character {
        Ok(text.len())
    } else {
        bail!("Character out of range")
    }
}

pub fn edit_text(text: &str, edits: &[Value]) -> Result<String> {
    let mut ranges = edits
        .iter()
        .map(|e| {
            Ok((
                offset(text, &e["range"]["start"])?,
                offset(text, &e["range"]["end"])?,
                e["newText"].as_str().context("Missing newText")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    let mut previous_end = 0;
    for (start, end, _) in &ranges {
        if start > end || *start < previous_end {
            bail!("Invalid or overlapping edits");
        }
        previous_end = *end;
    }
    let mut result = text.to_owned();
    for (start, end, replacement) in ranges.into_iter().rev() {
        result.replace_range(start..end, replacement);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unicode_and_crlf() {
        assert_eq!(
            document_change("a\r\n😀", "", true)["range"]["end"],
            json!({"line":1,"character":2})
        );
        assert_eq!(
            document_change("a\n", "", true)["range"]["end"],
            json!({"line":1,"character":0})
        );
        assert_eq!(document_change("", "x", false), json!({"text":"x"}));
        let text = "a😀b\r\n雪x";
        assert_eq!(offset(text, &json!({"line":0,"character":3})).unwrap(), 5);
        assert!(offset(text, &json!({"line":0,"character":2})).is_err());
        assert_eq!(edit_text(text, &[json!({"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}},"newText":"z"})]).unwrap(), "a😀b\r\nzx");
    }
}
