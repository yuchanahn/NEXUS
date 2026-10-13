use llm_contract::ProviderError;
use llm_wire::MAX_EVENT_BYTES;

/// Incremental byte parser for our UTF-8 SSE protocol. Accepts LF and CRLF,
/// comments/keepalives, and multiline data. Never decodes a partial UTF-8 chunk.
#[derive(Default)]
pub(crate) struct Parser {
    pending: Vec<u8>,
    data: Vec<u8>,
    event: Vec<u8>,
}

impl Parser {
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, ProviderError> {
        let mut frames = Vec::new();
        // Process incrementally to avoid copying an arbitrarily large transport chunk.
        for byte in chunk {
            self.pending.push(*byte);
            if self
                .pending
                .len()
                .saturating_add(self.data.len())
                .saturating_add(self.event.len())
                > MAX_EVENT_BYTES + 1024
            {
                return Err(super::invalid_response("SSE frame byte limit exceeded"));
            }
            if *byte != b'\n' {
                continue;
            }
            let mut line = std::mem::take(&mut self.pending);
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    if self.event != b"generation" {
                        return Err(super::invalid_response("unexpected SSE event type"));
                    }
                    self.data.pop();
                    frames.push(std::mem::take(&mut self.data));
                }
                self.event.clear();
                continue;
            }
            if line[0] == b':' {
                continue;
            }
            let colon = line.iter().position(|b| *b == b':').unwrap_or(line.len());
            let name = &line[..colon];
            let mut value = line.get(colon + 1..).unwrap_or(&[]);
            if value.first() == Some(&b' ') {
                value = &value[1..];
            }
            match name {
                b"data" => {
                    self.data.extend_from_slice(value);
                    self.data.push(b'\n');
                }
                b"event" => self.event = value.to_vec(),
                // Reconnect IDs/retry directives are not used; never reconnect silently.
                _ => {}
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fragmented_utf8_crlf_multiline_and_keepalive() {
        let input =
            ": keepalive\r\n\r\nevent: generation\r\ndata: {\"text\":\r\ndata: \"안녕\"}\r\n\r\n"
                .as_bytes();
        let mut parser = Parser::default();
        let mut frames = Vec::new();
        for byte in input {
            frames.extend(parser.feed(&[*byte]).unwrap());
        }
        assert_eq!(frames, vec!["{\"text\":\n\"안녕\"}".as_bytes().to_vec()]);
    }

    #[test]
    fn rejects_unknown_event_and_unbounded_line() {
        assert!(
            Parser::default()
                .feed(b"event: other\ndata: {}\n\n")
                .is_err()
        );
        assert!(
            Parser::default()
                .feed(&vec![b'x'; MAX_EVENT_BYTES + 1025])
                .is_err()
        );
    }
}
