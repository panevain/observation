//! Reading upstream response bodies under a size cap.

use bytes::Bytes;

/// Reads an upstream response body, giving up as soon as it grows past `max`
/// bytes. `Ok(None)` means it was too large; a `Content-Length` is not
/// required (chunked bodies are capped while streaming, not after buffering).
pub async fn read_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> Result<Option<Bytes>, reqwest::Error> {
    let declared = resp.content_length();
    if declared.is_some_and(|n| n > max as u64) {
        return Ok(None);
    }
    // At most `max` here, so the cast cannot truncate.
    let mut body = Vec::with_capacity(declared.map_or(0, |n| n as usize));
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(Bytes::from(body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Fetches `/` from a one-shot loopback server that answers with `head`,
    /// then `body` (written in pieces of at most `piece` bytes), then closes.
    async fn fetch(head: String, body: Vec<u8>, piece: usize) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(head.as_bytes()).await;
            for part in body.chunks(piece.max(1)) {
                if sock.write_all(part).await.is_err() {
                    return;
                }
                let _ = sock.flush().await;
            }
        });
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap()
    }

    /// A response with a `Content-Length` of `declared` and `len` body bytes.
    async fn sized(declared: usize, len: usize) -> reqwest::Response {
        let head =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
        fetch(head, vec![7; len], len).await
    }

    /// A chunked response (no `Content-Length`) of `len` bytes in `piece`-byte chunks.
    async fn chunked(len: usize, piece: usize) -> reqwest::Response {
        let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
        let mut body = Vec::new();
        for part in vec![7u8; len].chunks(piece) {
            body.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
            body.extend_from_slice(part);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"0\r\n\r\n");
        fetch(head.to_owned(), body, piece + 8).await
    }

    #[tokio::test]
    async fn declared_lengths_at_the_cap_are_read_and_over_it_refused() {
        let body = read_capped(sized(16, 16).await, 16).await.unwrap();
        assert_eq!(
            body.as_deref(),
            Some(&[7u8; 16][..]),
            "exactly `max` is accepted"
        );
        let empty = read_capped(sized(0, 0).await, 16).await.unwrap();
        assert_eq!(empty.map(|b| b.len()), Some(0));
        // Refused on the header alone: the body is never sent.
        assert!(read_capped(sized(17, 0).await, 16).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn undeclared_lengths_are_capped_while_streaming() {
        let body = read_capped(chunked(16, 5).await, 16).await.unwrap();
        assert_eq!(body.map(|b| b.len()), Some(16), "exactly `max` is accepted");
        assert!(
            read_capped(chunked(17, 5).await, 16)
                .await
                .unwrap()
                .is_none()
        );
        // Far past the cap: stops at the cap rather than buffering it all.
        assert!(
            read_capped(chunked(1 << 20, 4096).await, 16)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_body_cut_short_is_an_error() {
        // Declares 16 bytes, sends 4, closes.
        let resp = fetch(
            "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n".into(),
            vec![7; 4],
            4,
        )
        .await;
        assert!(read_capped(resp, 16).await.is_err());
    }
}
