//! LSP-style message framing over a byte stream.
//!
//! A `\n` is not enough as a separator, because the JSON of an exchange can
//! contain newlines inside a string. The `Content-Length` header format solves
//! that, and it is the one every editor's LSP client already implements, so
//! integrating costs less.

use std::io::{Error, ErrorKind, Result};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// The header announcing the body size, lowercased so it can be compared.
const CONTENT_LENGTH: &str = "content-length:";

/// The largest incoming message accepted.
///
/// A client announcing more than this is broken or hostile; without the limit,
/// a forged header would be enough to make the server reserve arbitrary
/// memory in one go.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Reads framed messages from an input stream.
pub struct MessageReader<R> {
    inner: BufReader<R>,
}

impl<R: AsyncRead + Unpin> MessageReader<R> {
    /// Wraps an input stream.
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
        }
    }

    /// Reads the next complete message.
    ///
    /// Returns `Ok(None)` when the stream ends cleanly between messages, which
    /// is how an editor says it is shutting down.
    ///
    /// # Errors
    ///
    /// If the stream fails, if the `Content-Length` header is missing, if its
    /// value is not a number, if the announced size exceeds the limit, or if
    /// the stream is cut in the middle of a body.
    pub async fn read_message(&mut self) -> Result<Option<String>> {
        let Some(length) = self.read_headers().await? else {
            return Ok(None);
        };

        let mut body = vec![0_u8; length];
        self.inner.read_exact(&mut body).await?;

        String::from_utf8(body)
            .map(Some)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))
    }

    /// Reads the header block and returns the announced `Content-Length`.
    async fn read_headers(&mut self) -> Result<Option<usize>> {
        let mut length = None;

        loop {
            let mut line = String::new();
            if self.inner.read_line(&mut line).await? == 0 {
                // End of stream. It is only clean if we had not started
                // reading a message's headers.
                return if length.is_none() {
                    Ok(None)
                } else {
                    Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "the stream ended between the headers and the body",
                    ))
                };
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);

            // The empty line closes the header block.
            if trimmed.is_empty() {
                return length.map(Some).ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidData,
                        "the Content-Length header is missing",
                    )
                });
            }

            if let Some(value) = header_value(trimmed) {
                let parsed: usize = value.parse().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("non-numeric Content-Length: `{value}`"),
                    )
                })?;

                if parsed > MAX_MESSAGE_BYTES {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("message of {parsed} bytes, maximum {MAX_MESSAGE_BYTES}"),
                    ));
                }

                length = Some(parsed);
            }
            // Every other header (Content-Type…) is ignored on purpose.
        }
    }
}

/// Extracts the value of a `Content-Length` header, case-insensitively.
fn header_value(line: &str) -> Option<&str> {
    let (name, value) = line.split_once(':')?;
    let mut header = String::with_capacity(name.len() + 1);
    header.push_str(&name.to_ascii_lowercase());
    header.push(':');

    (header == CONTENT_LENGTH).then(|| value.trim())
}

/// Writes a framed message to the output stream.
///
/// # Errors
///
/// If the write or the flush fails.
pub async fn write_message<W: AsyncWrite + Unpin>(writer: &mut W, body: &str) -> Result<()> {
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    // In tests, `unwrap` documents the expectation and its panic IS the failure.
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn reads_a_simple_message() {
        let input = "Content-Length: 2\r\n\r\n{}";
        let mut reader = MessageReader::new(input.as_bytes());

        assert_eq!(reader.read_message().await.unwrap(), Some("{}".to_owned()));
        assert_eq!(reader.read_message().await.unwrap(), None);
    }

    #[tokio::test]
    async fn reads_consecutive_messages() {
        let input = "Content-Length: 2\r\n\r\n{}Content-Length: 4\r\n\r\n[1;]";
        let mut reader = MessageReader::new(input.as_bytes());

        assert_eq!(reader.read_message().await.unwrap(), Some("{}".to_owned()));
        assert_eq!(
            reader.read_message().await.unwrap(),
            Some("[1;]".to_owned())
        );
    }

    #[tokio::test]
    async fn accepts_a_body_with_newlines() {
        let body = "{\n  \"a\": 1\n}";
        let input = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let mut reader = MessageReader::new(input.as_bytes());

        assert_eq!(reader.read_message().await.unwrap(), Some(body.to_owned()));
    }

    #[tokio::test]
    async fn ignores_other_headers() {
        let input = "Content-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let mut reader = MessageReader::new(input.as_bytes());

        assert_eq!(reader.read_message().await.unwrap(), Some("{}".to_owned()));
    }

    #[tokio::test]
    async fn the_header_is_case_insensitive() {
        let input = "content-length: 2\r\n\r\n{}";
        let mut reader = MessageReader::new(input.as_bytes());

        assert_eq!(reader.read_message().await.unwrap(), Some("{}".to_owned()));
    }

    #[tokio::test]
    async fn a_missing_content_length_is_an_error() {
        let input = "X-Other: 1\r\n\r\n{}";
        let mut reader = MessageReader::new(input.as_bytes());

        assert!(reader.read_message().await.is_err());
    }

    #[tokio::test]
    async fn a_non_numeric_content_length_is_an_error() {
        let input = "Content-Length: many\r\n\r\n{}";
        let mut reader = MessageReader::new(input.as_bytes());

        assert!(reader.read_message().await.is_err());
    }

    #[tokio::test]
    async fn rejects_an_outsized_message() {
        let input = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        let mut reader = MessageReader::new(input.as_bytes());

        assert!(reader.read_message().await.is_err());
    }

    #[tokio::test]
    async fn a_truncated_body_is_an_error() {
        let input = "Content-Length: 10\r\n\r\n{}";
        let mut reader = MessageReader::new(input.as_bytes());

        assert!(reader.read_message().await.is_err());
    }

    #[tokio::test]
    async fn writes_the_right_header() {
        let mut out = Vec::new();
        write_message(&mut out, "{\"a\":1}").await.unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Content-Length: 7\r\n\r\n{\"a\":1}"
        );
    }

    #[tokio::test]
    async fn what_was_written_can_be_read_back() {
        // The payload is deliberately not ASCII: `Content-Length` counts bytes
        // and not characters, so a body whose two counts differ is what proves
        // the writer and the reader agree on which one they mean.
        let message = "{\"greeting\":\"héllo\"}";

        let mut out = Vec::new();
        write_message(&mut out, message).await.unwrap();

        let mut reader = MessageReader::new(out.as_slice());
        assert_eq!(
            reader.read_message().await.unwrap(),
            Some(message.to_owned())
        );
    }
}
