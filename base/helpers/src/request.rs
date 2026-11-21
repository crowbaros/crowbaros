//! What every helper's socket shares: one JSON line in, one out.

use std::io::{BufRead, BufReader, Read};

/// One request line of at most `max` bytes, ended by a newline.
pub fn read_line(stream: &mut impl Read, max: usize) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    BufReader::new(stream.take(max as u64 + 1))
        .read_until(b'\n', &mut line)
        .map_err(|e| e.to_string())?;
    if line.len() > max || !line.ends_with(b"\n") {
        return Err("Request too large or incomplete".into());
    }
    Ok(line)
}

/// A request line as a JSON object.
pub fn parse_object(line: &[u8]) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    match serde_json::from_slice(line).map_err(|e| e.to_string())? {
        serde_json::Value::Object(object) => Ok(object),
        _ => Err("Request must be an object".into()),
    }
}

/// The reply for a refused or failed request.
pub fn refusal(error: impl std::fmt::Display) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": error.to_string()})
}
