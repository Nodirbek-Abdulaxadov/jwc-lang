//! The listener itself — config.md §3.2.1 (`bind`), §3.5 (`tls`) and §3.6
//! (`header_timeout`).
//!
//! Everything else about the server is testable through `serve::handle`,
//! which is why `tests/hardening.rs` never opens a socket. These three are
//! not: an address, a TLS handshake and a header deadline all happen
//! strictly below the point where a `Request` exists, so the only way to
//! observe them is to speak to a real port.
//!
//! That distinction is not academic. `header_read_timeout` needs a
//! `hyper_util::rt::TokioTimer` installed alongside it; without one, hyper
//! panics *inside its own poll* on every HTTP/1 connection. Every unit
//! test stayed green through that, because none of them was on the other
//! end of a socket.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A free port, released immediately. There is a race between this and the
/// bind inside `serve`, but the alternative — a hardcoded port — collides
/// with whatever else is on the machine, which is a worse race.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
    l.local_addr().expect("addr").port()
}

fn program(source: &str) -> Arc<jwc::exec::Program> {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.jwc"), source).expect("write");
    let ws = jwc::workspace::Workspace::load(dir.path()).expect("load");
    Arc::new(jwc::serve::load(&ws).unwrap_or_else(|e| panic!("{e}")))
}

/// Boot `serve` on its own runtime thread and wait for the port to answer.
///
/// The thread is detached: `serve` only returns on SIGTERM/Ctrl-C, and a
/// test process that sent itself either would take the whole suite down.
/// The listener dies with the process.
fn boot(source: &str, port: u16) {
    let program = program(source);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let _ = rt.block_on(jwc::serve::serve(program, port));
    });
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("nothing listening on {port} after 5s");
}

const PLAIN: &str = "namespace s;\n\
                     server { header_timeout = \"1s\"; }\n\
                     routes \"/x\" {\n\
                     \x20   route GET \"\" { return json({ ok: true }); }\n\
                     }\n";

#[test]
fn a_dribbled_header_is_cut_off_and_a_whole_one_is_answered() {
    let port = free_port();
    boot(PLAIN, port);

    // The half that catches the missing timer. hyper's panic is per
    // connection, so a listener that cannot answer *anything* looks
    // exactly like a listener that is enforcing a deadline — this is what
    // separates the two.
    let mut ok = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    ok.write_all(b"GET /x HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut answer = String::new();
    ok.set_read_timeout(Some(Duration::from_secs(10))).ok();
    ok.read_to_string(&mut answer).expect("read");
    assert!(
        answer.starts_with("HTTP/1.1 200 "),
        "a complete request was not answered: {answer:?}"
    );
    assert!(answer.contains("{\"ok\":true}"), "body missing: {answer:?}");

    // The other half: headers that never terminate. `request_timeout`
    // cannot cover this — its clock starts in `handle`, which a request
    // stuck in its own headers never reaches.
    let mut slow = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    slow.write_all(b"GET /x HTTP/1.1\r\nHost: t\r\n")
        .expect("write");
    slow.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let started = Instant::now();
    let mut buf = [0u8; 256];
    let n = slow.read(&mut buf);
    let waited = started.elapsed();
    match n {
        // Closed, or a 408 — hyper may answer before hanging up. Either
        // way the connection is not still held open.
        Ok(_) => {}
        Err(e) => panic!("the deadline did not bite after {waited:?}: {e}"),
    }
    assert!(
        waited < Duration::from_secs(5),
        "`header_timeout = 1s` let the dribble run for {waited:?}"
    );
}

#[test]
fn the_listener_speaks_tls_when_a_tls_block_names_a_certificate() {
    let Some((cert, key, _dir)) = self_signed() else {
        eprintln!(
            "SKIPPED the_listener_speaks_tls_when_a_tls_block_names_a_certificate — no openssl"
        );
        return;
    };
    let port = free_port();
    let source = format!(
        "namespace s;\n\
         server {{ tls {{ cert = \"{cert}\"; key = \"{key}\"; }} }}\n\
         routes \"/x\" {{\n\
         \x20   route GET \"\" {{ return json({{ ok: true }}); }}\n\
         }}\n"
    );
    boot(&source, port);

    // Self-signed, so verification is off — the assertion is that the
    // bytes are wrapped at all, not that this certificate chains anywhere.
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    let r = client
        .get(format!("https://127.0.0.1:{port}/x"))
        .send()
        .expect("https request");
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.text().unwrap_or_default(), "{\"ok\":true}");

    // And plain HTTP against the same port gets nowhere, which is what
    // makes the previous assertion mean "TLS" rather than "answered".
    assert!(
        client
            .get(format!("http://127.0.0.1:{port}/x"))
            .send()
            .is_err(),
        "the TLS listener answered a plaintext request"
    );
}

#[test]
fn a_tls_block_naming_a_missing_file_stops_the_boot() {
    // Falling back to plain HTTP is the outcome this rules out: the
    // listener would answer, so nothing outside the process could tell
    // that every byte was in the clear.
    let source = "namespace s;\n\
                  server { tls { cert = \"/nonexistent/cert.pem\"; key = \"/nonexistent/key.pem\"; } }\n\
                  routes \"/x\" {\n\
                  \x20   route GET \"\" { return json({ ok: true }); }\n\
                  }\n";
    let program = program(source);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let err = rt
        .block_on(jwc::serve::serve(program, free_port()))
        .expect_err("a missing certificate must not boot");
    let text = format!("{err:#}");
    assert!(
        text.contains("/nonexistent/cert.pem"),
        "the error does not name the file it could not read: {text}"
    );
}

#[test]
fn bind_keeps_the_listener_off_every_other_interface() {
    let Some(outward) = non_loopback_address() else {
        eprintln!(
            "SKIPPED bind_keeps_the_listener_off_every_other_interface — \
             this host has only a loopback address"
        );
        return;
    };
    let port = free_port();
    boot(
        "namespace s;\n\
         server { bind = \"127.0.0.1\"; }\n\
         routes \"/x\" {\n\
         \x20   route GET \"\" { return json({ ok: true }); }\n\
         }\n",
        port,
    );

    // Reachable where it was asked to be...
    TcpStream::connect(("127.0.0.1", port)).expect("loopback");
    // ...and nowhere else. Without this half the test passes on the
    // default `0.0.0.0`, which answers on loopback too.
    assert!(
        TcpStream::connect((outward.as_str(), port)).is_err(),
        "`bind = 127.0.0.1` still answered on {outward}"
    );
}

#[test]
fn a_bind_that_is_not_an_address_stops_the_boot() {
    // Falling back to the default would put the listener on every
    // interface — the opposite of what someone writing `bind` is reaching
    // for, and invisible until something else finds the port.
    let program = program(
        "namespace s;\n\
         server { bind = \"127.0.0..1\"; }\n\
         routes \"/x\" {\n\
         \x20   route GET \"\" { return json({ ok: true }); }\n\
         }\n",
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let err = rt
        .block_on(jwc::serve::serve(program, free_port()))
        .expect_err("a malformed bind must not boot");
    assert!(
        format!("{err:#}").contains("127.0.0..1"),
        "the error does not name the value it could not parse: {err:#}"
    );
}

/// This host's own non-loopback address, or `None` if it has none. The UDP
/// "connect" sends nothing — it only asks the routing table which local
/// address would be used to reach the outside.
fn non_loopback_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback()).then(|| ip.to_string())
}

/// A throwaway certificate and key, or `None` when openssl is not on PATH.
/// Returns the tempdir so the caller keeps it alive — dropping it deletes
/// both files, and `serve` reads them at boot.
fn self_signed() -> Option<(String, String, tempfile::TempDir)> {
    let dir = tempfile::tempdir().ok()?;
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    let out = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some((cert.to_str()?.to_string(), key.to_str()?.to_string(), dir))
}

// ── sockets (routing.md §9) ────────────────────────────────────────────
//
// A socket is the other thing `serve::handle` cannot reach: the upgrade
// happens in the axum layer, and everything after it is frames on a live
// connection rather than a `Request` and a `Response`. So this speaks the
// protocol.

const SOCKET_APP: &str = "namespace s;\n\
                          middleware NeedKey provides who: text {\n\
                          \x20   let key = request.query(\"key\") or throw Unauthorized(\"kalit kerak\");\n\
                          \x20   context.who = $key;\n\
                          }\n\
                          routes \"/live\" {\n\
                          \x20   route GET \"health\" { return json({ ok: true }); }\n\
                          \x20   socket \"echo/{room: text}\" use NeedKey {\n\
                          \x20       on open { socket.send(\"salom \" + context.who + \" @\" + @room); }\n\
                          \x20       on message (m) {\n\
                          \x20           if ($m == \"bye\") { socket.close(); }\n\
                          \x20           socket.send(\"echo: \" + $m);\n\
                          \x20       }\n\
                          \x20   }\n\
                          }\n";

/// A masked text frame, as a client must send.
fn ws_text_frame(payload: &str) -> Vec<u8> {
    let body = payload.as_bytes();
    let mask = [0x37u8, 0xfa, 0x21, 0x3d];
    let mut out = vec![0x81];
    assert!(body.len() < 126, "test payloads stay in the short form");
    out.push(0x80 | body.len() as u8);
    out.extend_from_slice(&mask);
    out.extend(body.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// One unmasked server frame: `(opcode, payload)`.
fn ws_read_frame(s: &mut TcpStream) -> Option<(u8, String)> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).ok()?;
    let opcode = head[0] & 0x0f;
    let mut len = (head[1] & 0x7f) as usize;
    if len == 126 {
        let mut ext = [0u8; 2];
        s.read_exact(&mut ext).ok()?;
        len = u16::from_be_bytes(ext) as usize;
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some((opcode, String::from_utf8_lossy(&body).to_string()))
}

fn upgrade(port: u16, target: &str) -> (TcpStream, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!(
        "GET {target} HTTP/1.1\r\nHost: t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(req.as_bytes()).expect("write");

    // Read exactly the head, byte at a time: anything more would eat into
    // the first frame.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if s.read_exact(&mut byte).is_err() {
            break;
        }
        head.push(byte[0]);
    }
    (s, String::from_utf8_lossy(&head).to_string())
}

/// The whole point of `use` on a socket: the chain runs on the HTTP
/// request, so a rejected client reads a status instead of getting a 101
/// followed by an immediate close it has to guess about.
#[test]
fn middleware_refuses_a_socket_before_the_upgrade() {
    let port = free_port();
    boot(SOCKET_APP, port);

    let (mut s, head) = upgrade(port, "/live/echo/lobby");
    assert!(
        head.starts_with("HTTP/1.1 401 "),
        "the chain did not refuse: {head:?}"
    );
    let mut body = String::new();
    let _ = s.read_to_string(&mut body);
    assert!(body.contains("kalit kerak"), "wrong body: {body:?}");
}

#[test]
fn a_socket_runs_open_then_message_and_close_ends_it() {
    let port = free_port();
    boot(SOCKET_APP, port);

    let (mut s, head) = upgrade(port, "/live/echo/lobby?key=abc");
    assert!(head.starts_with("HTTP/1.1 101 "), "no upgrade: {head:?}");

    // `on open` — `context.who` from the chain and `@room` from the path.
    assert_eq!(
        ws_read_frame(&mut s).map(|(_, p)| p),
        Some("salom abc @lobby".to_string())
    );

    s.write_all(&ws_text_frame("hello")).expect("write");
    assert_eq!(
        ws_read_frame(&mut s).map(|(_, p)| p),
        Some("echo: hello".to_string())
    );

    // `socket.close()` runs before the `socket.send` after it, and both
    // are queued — so the send is dropped and the close is what arrives.
    s.write_all(&ws_text_frame("bye")).expect("write");
    let (opcode, _) = ws_read_frame(&mut s).expect("a frame after `bye`");
    assert_eq!(
        opcode, 0x8,
        "expected a close frame, got opcode {opcode:#x}"
    );
}

/// The path exists; the request is wrong. A 404 would send the caller
/// looking for a typo in a path that is right.
#[test]
fn a_plain_get_at_a_socket_path_is_a_400() {
    let port = free_port();
    boot(SOCKET_APP, port);

    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    s.write_all(b"GET /live/echo/lobby?key=abc HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut answer = String::new();
    s.read_to_string(&mut answer).expect("read");
    assert!(
        answer.starts_with("HTTP/1.1 400 "),
        "expected 400: {answer:?}"
    );
    assert!(answer.contains("websocket"), "unhelpful body: {answer:?}");
}

/// A socket and its HTTP siblings share one `routes` block, and the
/// sibling still answers.
#[test]
fn an_http_route_beside_a_socket_still_answers() {
    let port = free_port();
    boot(SOCKET_APP, port);

    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    s.write_all(b"GET /live/health HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut answer = String::new();
    s.read_to_string(&mut answer).expect("read");
    assert!(answer.starts_with("HTTP/1.1 200 "), "{answer:?}");
    assert!(answer.contains("{\"ok\":true}"), "{answer:?}");
}
