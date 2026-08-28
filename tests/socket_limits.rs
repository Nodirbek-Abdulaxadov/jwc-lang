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

/// How many sockets may be open at once.
///
/// Measured before 0.9.946, against a server whose descriptor limit was
/// 200: an attacker opened **190** WebSocket connections, sent nothing on
/// any of them, and every ordinary HTTP request then failed to connect at
/// all — `/healthz` and `/readyz` included, so an orchestrator would see a
/// dead pod, restart it, and hand the attacker a fresh one to refill. The
/// connections cost about 14.7 kB and exactly one descriptor each, and
/// nothing ever closed them: `socket.recv()` waits with no timeout, so a
/// peer that says nothing is indistinguishable from one that is thinking.
///
/// The cap is taken **before** the handshake, so the descriptor is never
/// spent, and the refusal is a 503 the client can read rather than a 101
/// followed by a close.
#[tokio::test(flavor = "multi_thread")]
async fn the_number_of_open_sockets_is_capped() {
    const CAP: usize = 8;
    let src = format!(
        r#"
server {{
    max_sockets = {CAP};
}}

routes "/" {{
    route GET "ping" {{ return content("text/plain", "pong"); }}
    socket "ws" {{
        on message (text) {{
            socket.send("echo");
        }}
    }}
}}
"#
    );
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.jwc"), src).expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    let program = Arc::new(jwc::serve::load(&ws).unwrap_or_else(|e| panic!("{e}")));

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

    // Hold the connections open by keeping the streams alive.
    let mut held = Vec::new();
    let mut refused = 0;
    for _ in 0..(CAP * 3) {
        match idle_upgrade(port).await {
            Ok(s) => held.push(s),
            Err(status) => {
                assert!(
                    status.contains("503"),
                    "an over-cap upgrade must be a readable 503, got: {status}"
                );
                refused += 1;
            }
        }
    }
    assert_eq!(held.len(), CAP, "exactly the cap must be admitted");
    assert_eq!(refused, CAP * 2, "everything past the cap must be refused");

    // The point of the cap: HTTP is still answering while sockets are full.
    assert!(
        http_ping(port).await,
        "HTTP must survive a socket flood — this is the whole reason for the cap"
    );

    // A closed connection returns its slot.
    held.truncate(CAP / 2);
    for _ in 0..50 {
        if idle_upgrade(port).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("closing a connection did not release its slot");
}

/// Upgrade and hold, sending nothing. `Err` carries the status line.
async fn idle_upgrade(port: u16) -> Result<TcpStream, String> {
    let mut s = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let req = format!(
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte).await {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    if text.contains("101") {
        Ok(s)
    } else {
        Err(text.lines().next().unwrap_or("").to_string())
    }
}

/// A plain HTTP request on the same listener.
async fn http_ping(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    let req = format!("GET /ping HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    match s.read(&mut buf).await {
        Ok(n) if n > 0 => String::from_utf8_lossy(&buf[..n]).contains("200"),
        _ => false,
    }
}
