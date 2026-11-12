//! Lectura acotada antes de decodificar: stdout aborta, stderr drena sin retener.
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub(crate) enum Line {
    Frame(Vec<u8>, u64),
    Overflow(u64),
}

pub(crate) async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
    drain: bool,
) -> std::io::Result<Option<Line>> {
    let mut bytes = Vec::new();
    let mut observed = 0_u64;
    let mut overflow = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok((observed != 0).then_some(if overflow {
                Line::Overflow(observed)
            } else {
                Line::Frame(bytes, observed)
            }));
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let count = end.unwrap_or(chunk.len());
        let consumed = count + usize::from(end.is_some());
        observed = observed.saturating_add(consumed as u64);
        if !overflow {
            if count > limit.saturating_sub(bytes.len()) {
                overflow = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&chunk[..count]);
            }
        }
        reader.consume(consumed);
        if overflow && !drain {
            return Ok(Some(Line::Overflow(observed)));
        }
        if end.is_some() {
            return Ok(Some(if overflow {
                Line::Overflow(observed)
            } else {
                Line::Frame(bytes, observed)
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn limite_exacto_y_siguiente_linea_no_se_pierden() {
        let input = b"12345678\nnext\n";
        let mut reader = BufReader::with_capacity(3, &input[..]);
        assert!(
            matches!(read_line(&mut reader, 8, false).await.unwrap(),Some(Line::Frame(b,9)) if b==b"12345678")
        );
        assert!(
            matches!(read_line(&mut reader, 8, false).await.unwrap(),Some(Line::Frame(b,5)) if b==b"next")
        );
        assert!(read_line(&mut reader, 8, false).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stdout_excesivo_se_rechaza_sin_esperar_newline_ni_eof() {
        let (mut write, read) = tokio::io::duplex(128);
        write.write_all(&[b'x'; 65]).await.unwrap();
        let mut reader = BufReader::with_capacity(8, read);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            read_line(&mut reader, 64, false),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, Some(Line::Overflow(65))));
        // El escritor permanece abierto: el límite no depende de EOF.
        write.write_all(b"still open").await.unwrap();
    }

    struct Failed;
    impl tokio::io::AsyncRead for Failed {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("owned test read failure")))
        }
    }
    impl tokio::io::AsyncBufRead for Failed {
        fn poll_fill_buf(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<&[u8]>> {
            std::task::Poll::Ready(Err(std::io::Error::other("owned test read failure")))
        }
        fn consume(self: std::pin::Pin<&mut Self>, _: usize) {}
    }
    #[tokio::test]
    async fn error_de_io_no_parece_eof_ni_frame_valido() {
        for drain in [false, true] {
            assert!(read_line(&mut Failed, 64, drain).await.is_err());
        }
    }

    #[tokio::test]
    async fn stderr_drena_linea_entera_y_conserva_la_siguiente() {
        let input = b"123456789012345\nnext\nlast";
        let mut reader = BufReader::with_capacity(3, &input[..]);
        assert!(matches!(
            read_line(&mut reader, 8, true).await.unwrap(),
            Some(Line::Overflow(16))
        ));
        assert!(
            matches!(read_line(&mut reader,8,true).await.unwrap(),Some(Line::Frame(b,5)) if b==b"next")
        );
        assert!(
            matches!(read_line(&mut reader,8,true).await.unwrap(),Some(Line::Frame(b,4)) if b==b"last")
        );
    }
}
