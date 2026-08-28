//! What a WebSocket peer is allowed to send.
//!
//! `server { max_body_bytes }` is documented as the cap on a request body
//! (config.md §3.1). A socket frame is also a thing a peer sends, and
//! until 0.9.944 the cap stopped at the HTTP body: the upgrade carried no
//! limit of its own, so the real ceiling was tungstenite's 64 MiB default
//! whatever the config said.
//!
//! Measured against a server configured for 1024 bytes: a **5,000,000**
//! byte text frame was accepted and handled — about 5000x the configured
//! limit — and 64 MiB was the point where the connection finally died.
//!
//! The test drives a real server over a real socket, hand-rolling the
//! frame, because that is the only way to send the frame the library
//! would refuse to build.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const SOURCE: &str = r#"
server {
    max_body_bytes = 1024;
}

routes "/" {
    socket "ws" {
        on message (text) {
            socket.send("len=" + string.of(string.len($text)));
        }
    }
}
"#;

/// A masked client text frame. Client frames must be masked (RFC 6455
/// §5.3), and the length has three encodings depending on size.
fn text_frame(payload: &[u8]) -> Vec<u8> {
    let mut f = vec![0x81u8];
    let n = payload.len();
    if n < 126 {
        f.push(0x80 | n as u8);
    } else if n < 65536 {
        f.push(0x80 | 126);
        f.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        f.push(0x80 | 127);
        f.extend_from_slice(&(n as u64).to_be_bytes());
    }
    let mask = [0x37u8, 0xfa, 0x21, 0x3d];
    f.extend_from_slice(&mask);
    f.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// What the server did with a frame of `size` bytes.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// A text frame came back — the handler ran.
    Echoed(String),
    /// The connection closed or reset without the handler answering.
    Refused,
}

async fn send_frame(port: u16, size: usize) -> Outcome {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let req = format!(
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.expect("handshake");

    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte).await {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    assert!(
        String::from_utf8_lossy(&head).contains("101"),
        "the upgrade must succeed: {}",
        String::from_utf8_lossy(&head)
    );

    if s.write_all(&text_frame(&vec![b'A'; size])).await.is_err() {
        return Outcome::Refused;
    }
    let mut reply = [0u8; 256];
    match s.read(&mut reply).await {
        Ok(0) | Err(_) => Outcome::Refused,
        Ok(n) => {
            // 0x1 is a text frame, 0x8 a close. The server does not mask.
            if reply[0] & 0x0f != 0x1 {
                return Outcome::Refused;
            }
            let len = (reply[1] & 0x7f) as usize;
            Outcome::Echoed(String::from_utf8_lossy(&reply[2..(2 + len).min(n)]).to_string())
        }
    }
}

/// A socket message is capped by `max_body_bytes`, not by the library.
#[tokio::test(flavor = "multi_thread")]
async fn a_socket_message_is_bounded_by_max_body_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.jwc"), SOURCE).expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let program = Arc::new(jwc::serve::load(&ws).unwrap_or_else(|e| panic!("{e}")));

    // Ask the OS for a free port, then hand the number to the server. A
    // fixed port makes the suite fail when something else holds it.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    tokio::spawn(async move {
        let _ = jwc::serve::serve(program, port).await;
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // At the cap: handled.
    assert_eq!(
        send_frame(port, 1024).await,
        Outcome::Echoed("len=1024".to_string()),
        "a message at the cap must still be delivered"
    );
    // One byte over: refused. Before 0.9.944 this was echoed, and so was
    // a five-megabyte one.
    assert_eq!(
        send_frame(port, 1025).await,
        Outcome::Refused,
        "a message over the cap must not reach the handler"
    );
    assert_eq!(
        send_frame(port, 5_000_000).await,
        Outcome::Refused,
        "the 5 MB frame that this test exists for must not reach the handler"
    );
}
