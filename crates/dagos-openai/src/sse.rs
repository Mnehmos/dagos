//! Server-sent-events decoding for streaming Chat Completions.

/// Splits a server-sent-events byte stream into the payloads of its `data:` lines. Bytes are
/// buffered until a line is complete, so chunk boundaries (even inside a UTF-8 sequence) are safe.
/// Comment lines (`: keep-alive`) and other fields are ignored.
#[derive(Debug, Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
}

impl SseDecoder {
    /// Feeds the next chunk and returns the payloads of every `data:` line it completes.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut payloads = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                payloads.push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
        }
        payloads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yields_data_payloads_across_arbitrary_chunk_boundaries() {
        let stream = ": OPENROUTER PROCESSING\n\ndata: {\"a\":\"é\"}\r\n\ndata:{\"b\":2}\n\nevent: x\ndata: [DONE]\n\n";
        let bytes = stream.as_bytes();
        for size in 1..=bytes.len() {
            let mut decoder = SseDecoder::default();
            let payloads: Vec<String> =
                bytes.chunks(size).flat_map(|chunk| decoder.push(chunk)).collect();
            assert_eq!(payloads, ["{\"a\":\"é\"}", "{\"b\":2}", "[DONE]"], "chunks of {size}");
        }
    }
}
