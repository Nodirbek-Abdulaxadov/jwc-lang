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
