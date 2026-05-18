//! SMTP-based email sending for the JWC stdlib.
//!
//! Reads connection config from environment variables on first use and caches
//! a process-wide `SmtpTransport` for subsequent calls:
//!
//! | Env var              | Default     | Notes                                                   |
//! |----------------------|-------------|---------------------------------------------------------|
//! | `JWC_SMTP_HOST`      | (required)  | Server hostname (e.g. `smtp.gmail.com`)                 |
//! | `JWC_SMTP_PORT`      | `587`       | Integer port                                            |
//! | `JWC_SMTP_USER`      | (required)  | Login / AuthN username                                  |
//! | `JWC_SMTP_PASSWORD`  | (required)  | Login password / app token                              |
//! | `JWC_SMTP_FROM`      | (required)  | `Display Name <addr@host>` formatted                    |
//! | `JWC_SMTP_TLS`       | `starttls`  | `starttls` \| `tls` (implicit, port 465) \| `none`      |

use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};

/// Cached, shared transport. Built once on the first `send_email` call.
fn transport() -> Result<&'static Mutex<SmtpTransport>> {
    static TRANSPORT: OnceLock<Mutex<SmtpTransport>> = OnceLock::new();
    if let Some(t) = TRANSPORT.get() {
        return Ok(t);
    }
    let built = build_transport_from_env()?;
    // Race-tolerant: if another thread initialized first, drop ours.
    let _ = TRANSPORT.set(Mutex::new(built));
    Ok(TRANSPORT.get().expect("transport just initialized"))
}

/// Resolved SMTP configuration read from environment variables.
struct SmtpConfig {
    host: String,
    port: u16,
    user: String,
    password: String,
    tls: TlsMode,
}

#[derive(Clone, Copy)]
enum TlsMode {
    StartTls,
    ImplicitTls,
    None,
}

fn read_env_var(name: &str) -> Result<String> {
    let v = std::env::var(name)
        .map_err(|_| anyhow!("send_email: {name} is required"))?;
    if v.trim().is_empty() {
        bail!("send_email: {name} is required");
    }
    Ok(v)
}

fn load_config() -> Result<SmtpConfig> {
    let host = read_env_var("JWC_SMTP_HOST")?;
    let user = read_env_var("JWC_SMTP_USER")?;
    let password = read_env_var("JWC_SMTP_PASSWORD")?;
    // JWC_SMTP_FROM is validated/used in `send_email` (one read per call so
    // changes to it take effect without rebuilding the transport).
    let _ = read_env_var("JWC_SMTP_FROM")?;
    let port: u16 = match std::env::var("JWC_SMTP_PORT") {
        Ok(s) if !s.trim().is_empty() => s
            .parse()
            .with_context(|| format!("send_email: invalid JWC_SMTP_PORT '{s}'"))?,
        _ => 587,
    };
    let tls = match std::env::var("JWC_SMTP_TLS")
        .ok()
        .as_deref()
        .map(str::trim)
        .unwrap_or("starttls")
        .to_ascii_lowercase()
        .as_str()
    {
        "starttls" => TlsMode::StartTls,
        "tls" | "implicit" | "smtps" => TlsMode::ImplicitTls,
        "none" | "plaintext" | "off" => TlsMode::None,
        other => bail!(
            "send_email: invalid JWC_SMTP_TLS '{other}' (expected starttls|tls|none)"
        ),
    };
    Ok(SmtpConfig {
        host,
        port,
        user,
        password,
        tls,
    })
}

fn build_transport_from_env() -> Result<SmtpTransport> {
    let cfg = load_config()?;
    let creds = Credentials::new(cfg.user.clone(), cfg.password.clone());

    let builder = match cfg.tls {
        TlsMode::ImplicitTls => {
            let params = TlsParameters::new(cfg.host.clone())
                .map_err(|e| anyhow!("send_email: TLS setup failed: {e}"))?;
            SmtpTransport::builder_dangerous(&cfg.host).tls(Tls::Wrapper(params))
        }
        TlsMode::StartTls => {
            let params = TlsParameters::new(cfg.host.clone())
                .map_err(|e| anyhow!("send_email: TLS setup failed: {e}"))?;
            SmtpTransport::builder_dangerous(&cfg.host).tls(Tls::Required(params))
        }
        TlsMode::None => SmtpTransport::builder_dangerous(&cfg.host),
    };

    let transport = builder.port(cfg.port).credentials(creds).build();
    Ok(transport)
}

/// Send a single HTML email through the cached SMTP transport.
pub fn send_email(to: &str, subject: &str, body_html: &str) -> Result<()> {
    // Validate JWC_SMTP_HOST up-front so the error is identical even when the
    // cached transport already exists from a prior successful init (this is
    // also what the unit test asserts).
    let _ = read_env_var("JWC_SMTP_HOST")?;
    let from = read_env_var("JWC_SMTP_FROM")?;

    let msg = Message::builder()
        .from(
            from.parse()
                .map_err(|e| anyhow!("send_email: invalid JWC_SMTP_FROM '{from}': {e}"))?,
        )
        .to(to
            .parse()
            .map_err(|e| anyhow!("send_email: invalid 'to' address '{to}': {e}"))?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(body_html.to_string())
        .map_err(|e| anyhow!("send_email: failed to build message: {e}"))?;

    let mailer = transport()?;
    let guard = mailer.lock().expect("smtp transport mutex poisoned");
    guard
        .send(&msg)
        .map_err(|e| anyhow!("send_email: SMTP send failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Env-var manipulation is process-global; serialize the tests that
    /// touch `JWC_SMTP_*` so they don't clobber each other.
    fn env_lock() -> MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        let m = M.get_or_init(|| Mutex::new(()));
        match m.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    #[test]
    fn send_email_errors_when_host_missing() {
        let _g = env_lock();
        // Snapshot then clear the relevant variable.
        let prev = std::env::var("JWC_SMTP_HOST").ok();
        std::env::remove_var("JWC_SMTP_HOST");

        let err = send_email("a@b.test", "subj", "<p>hi</p>")
            .expect_err("expected error when JWC_SMTP_HOST is missing");
        let msg = err.to_string();
        assert!(
            msg.contains("JWC_SMTP_HOST is required"),
            "unexpected error message: {msg}"
        );

        if let Some(v) = prev {
            std::env::set_var("JWC_SMTP_HOST", v);
        }
    }
}
