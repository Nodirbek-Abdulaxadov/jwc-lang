use anyhow::Result;
use tiny_http::{Header, Response, Server};

use crate::ast::Program;
use crate::runner;

pub fn serve(program: &Program, port: u16) -> Result<()> {
    let addr = format!("0.0.0.0:{port}");
    let server = Server::http(&addr)
        .map_err(|e| anyhow::anyhow!("Failed to bind to {addr}: {e}"))?;

    println!("╔══════════════════════════════════════╗");
    println!("║         JWC Server started           ║");
    println!("╠══════════════════════════════════════╣");
    println!("║  http://{}            ║", addr);
    println!("║  Press Ctrl+C to stop                ║");
    println!("╚══════════════════════════════════════╝");
    println!();

    let content_type: Header = "Content-Type: application/json"
        .parse()
        .expect("valid header");

    for mut request in server.incoming_requests() {
        let method = request.method().to_string();
        let url = request.url().to_string();

        // Strip query string from path
        let path = url.split('?').next().unwrap_or(&url).to_string();

        // Read request body
        let mut body_bytes = Vec::new();
        let _ = std::io::Read::read_to_end(request.as_reader(), &mut body_bytes);
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();
        let body = if body_str.trim().is_empty() {
            None
        } else {
            Some(body_str)
        };

        // Dispatch to JWC route
        let (status, response_body) = runner::run_request(program, &method, &path, body)
            .unwrap_or_else(|e| {
                let msg = e.to_string().replace('"', "'");
                (500, format!("{{\"error\":\"{msg}\"}}"))
            });

        eprintln!("[JWC] {} {} → {}", method, path, status);

        let response = Response::from_string(response_body)
            .with_status_code(status)
            .with_header(content_type.clone());

        let _ = request.respond(response);
    }

    Ok(())
}
