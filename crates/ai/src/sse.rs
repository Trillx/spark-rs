//! Minimal Server-Sent Events parser: feed it bytes as they arrive, get back
//! each complete event. Both providers stream their responses this way.

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct Parser {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl Parser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            // A complete line can't split a UTF-8 sequence, so lossy decoding only
            // replaces genuinely invalid bytes.
            let line = String::from_utf8_lossy(&line);

            if line.is_empty() {
                if self.event.is_some() || !self.data.is_empty() {
                    events.push(Event {
                        event: self.event.take(),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_ref(), ""),
            };
            match field {
                "event" => self.event = Some(value.to_owned()),
                "data" => self.data.push(value.to_owned()),
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks() {
        let mut parser = Parser::default();
        assert!(parser.push(b"event: ping\nda").is_empty());
        let events = parser.push(b"ta: {\"a\":1}\r\n\r\n: comment\n\ndata: two\ndata: lines\n\n");
        assert_eq!(
            events,
            vec![
                Event {
                    event: Some("ping".into()),
                    data: "{\"a\":1}".into()
                },
                Event {
                    event: None,
                    data: "two\nlines".into()
                },
            ]
        );
    }

    #[test]
    fn multibyte_text_split_mid_character() {
        let mut parser = Parser::default();
        let bytes = "data: café\n\n".as_bytes();
        let (head, tail) = bytes.split_at(9); // inside the two-byte "é"
        assert!(parser.push(head).is_empty());
        assert_eq!(parser.push(tail)[0].data, "café");
    }
}
