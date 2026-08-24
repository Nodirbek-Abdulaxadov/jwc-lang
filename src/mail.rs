//! SMTP transport behind the `mail.send` built-in (builtins.md §8).
//!
//! `mail.send` was **typed but not implemented**: `check.rs` gave it an
//! arity and a `Void` result, and the interpreter's built-in table mapped
//! it to `Value::Null`. A program that sent a password-reset link
//! typechecked clean, ran clean, and delivered nothing — with no error
//! anywhere. That is the same defect the `redis.rate_limit` stub had, and
//! it gets the same answer: without a reachable server the call **raises**.
//! "No mail server" must never read as "sent".
//!
//! `DEFERRED-10` puts the provider shape in a package rather than in the
//! language, and that still holds — `mail.send` is the package surface, and
//! this module is the core-tier driver under it, exactly as
//! [`crate::redis_engine`] sits under `redis.*`.
//!
//! | Env var             | Default    | Notes                                |
//! |---------------------|------------|--------------------------------------|
//! | `JWC_SMTP_HOST`     | (required) | Server hostname                      |
//! | `JWC_SMTP_PORT`     | `587`      | Integer port                         |
//! | `JWC_SMTP_USER`     | (required) | Auth username                        |
//! | `JWC_SMTP_PASSWORD` | (required) | Auth password / app token            |
//! | `JWC_SMTP_FROM`     | (required) | `Display Name <addr@host>`           |
//! | `JWC_SMTP_TLS`      | `starttls` | `starttls` \| `tls` \| `none`        |
//!
//! Every one of them is already in `config::REGISTRY`, and
//! `JWC_SMTP_PASSWORD` is already redacted by the `PASSWORD` needle.

use anyhow::{anyhow, bail, Context, Result};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use std::sync::{Mutex, OnceLock};

/// Whether the four required variables are set.
///
/// This is what `mail.enabled()` answers, so a program can branch instead
/// of raising when the mail leg is optional — the shape `redis.enabled()`
/// established.
pub fn is_configured() -> bool {
    [
        "JWC_SMTP_HOST",
        "JWC_SMTP_USER",
        "JWC_SMTP_PASSWORD",
        "JWC_SMTP_FROM",
    ]
    .iter()
    .all(|n| std::env::var(n).is_ok_and(|v| !v.trim().is_empty()))
}

#[derive(Clone, Copy)]
enum TlsMode {
    StartTls,
    ImplicitTls,
    None,
}

struct SmtpConfig {
    host: String,
    port: u16,
    user: String,
    password: String,
    tls: TlsMode,
}

fn required(name: &str) -> Result<String> {
    let v = std::env::var(name).map_err(|_| anyhow!("mail.send: {name} is not set"))?;
    if v.trim().is_empty() {
        bail!("mail.send: {name} is empty");
    }
    Ok(v)
}

fn load_config() -> Result<SmtpConfig> {
    let host = required("JWC_SMTP_HOST")?;
    let user = required("JWC_SMTP_USER")?;
    let password = required("JWC_SMTP_PASSWORD")?;
    let port: u16 = match std::env::var("JWC_SMTP_PORT") {
        Ok(s) if !s.trim().is_empty() => s
            .trim()
            .parse()
            .with_context(|| format!("mail.send: invalid JWC_SMTP_PORT '{s}'"))?,
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
        other => bail!("mail.send: invalid JWC_SMTP_TLS '{other}' (expected starttls|tls|none)"),
    };
    Ok(SmtpConfig {
        host,
        port,
        user,
        password,
        tls,
    })
}

fn build_transport() -> Result<SmtpTransport> {
    let cfg = load_config()?;
    let creds = Credentials::new(cfg.user.clone(), cfg.password.clone());
    let builder = match cfg.tls {
        TlsMode::ImplicitTls => {
            let params = TlsParameters::new(cfg.host.clone())
                .map_err(|e| anyhow!("mail.send: TLS setup failed: {e}"))?;
            SmtpTransport::builder_dangerous(&cfg.host).tls(Tls::Wrapper(params))
        }
        TlsMode::StartTls => {
            let params = TlsParameters::new(cfg.host.clone())
                .map_err(|e| anyhow!("mail.send: TLS setup failed: {e}"))?;
            SmtpTransport::builder_dangerous(&cfg.host).tls(Tls::Required(params))
        }
        TlsMode::None => SmtpTransport::builder_dangerous(&cfg.host),
    };
    Ok(builder.port(cfg.port).credentials(creds).build())
}

/// Built once and shared. Opening a TLS session per email would put a
/// handshake on the request path of every route that sends one.
fn transport() -> Result<&'static Mutex<SmtpTransport>> {
    static TRANSPORT: OnceLock<Mutex<SmtpTransport>> = OnceLock::new();
    if let Some(t) = TRANSPORT.get() {
        return Ok(t);
    }
    let built = build_transport()?;
    // Race-tolerant: if another thread won, ours is dropped.
    let _ = TRANSPORT.set(Mutex::new(built));
    TRANSPORT
        .get()
        .ok_or_else(|| anyhow!("mail.send: transport init raced and lost"))
}

/// Compose and deliver one HTML message.
///
/// `lettre` is built here without its `tokio1` feature, so the transport is
/// blocking. Calling it directly from the request future would park a
/// runtime worker for the whole SMTP conversation — a slow relay would eat
/// the executor a thread at a time — so the send moves to the blocking
/// pool and the caller awaits it.
pub async fn send(to: &str, subject: &str, body_html: &str) -> Result<()> {
    let (to, subject, body_html) = (to.to_string(), subject.to_string(), body_html.to_string());
    tokio::task::spawn_blocking(move || send_blocking(&to, &subject, &body_html))
        .await
        .map_err(|e| anyhow!("mail.send: worker failed: {e}"))?
}

fn send_blocking(to: &str, subject: &str, body_html: &str) -> Result<()> {
    // Read `FROM` per call rather than caching it with the transport, so
    // changing it does not need a restart.
    let from = required("JWC_SMTP_FROM")?;
    let msg = Message::builder()
        .from(
            from.parse()
                .map_err(|e| anyhow!("mail.send: invalid JWC_SMTP_FROM '{from}': {e}"))?,
        )
        .to(to
            .parse()
            .map_err(|e| anyhow!("mail.send: invalid recipient '{to}': {e}"))?)
        .subject(subject)
        .header(ContentType::TEXT_HTML)
        .body(body_html.to_string())
        .map_err(|e| anyhow!("mail.send: could not build the message: {e}"))?;

    let mailer = transport()?;
    let guard = crate::locks::lock_recover(mailer);
    guard
        .send(&msg)
        .map_err(|e| anyhow!("mail.send: SMTP delivery failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    /// `set_var`/`remove_var` are process-global; the tests that touch
    /// `JWC_SMTP_*` have to run one at a time or they clobber each other.
    fn env_lock() -> MutexGuard<'static, ()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        crate::locks::lock_recover(M.get_or_init(|| Mutex::new(())))
    }

    struct Restore(Vec<(&'static str, Option<String>)>);

    impl Restore {
        fn take(names: &[&'static str]) -> Self {
            let saved = names
                .iter()
                .map(|n| (*n, std::env::var(n).ok()))
                .collect::<Vec<_>>();
            for (n, _) in &saved {
                std::env::remove_var(n);
            }
            Self(saved)
        }
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            for (n, v) in &self.0 {
                match v {
                    Some(v) => std::env::set_var(n, v),
                    None => std::env::remove_var(n),
                }
            }
        }
    }

    const ALL: &[&str] = &[
        "JWC_SMTP_HOST",
        "JWC_SMTP_USER",
        "JWC_SMTP_PASSWORD",
        "JWC_SMTP_FROM",
        "JWC_SMTP_PORT",
        "JWC_SMTP_TLS",
    ];

    #[test]
    fn unconfigured_is_not_enabled() {
        let _g = env_lock();
        let _r = Restore::take(ALL);
        assert!(!is_configured());
    }

    #[test]
    fn all_four_present_is_enabled() {
        let _g = env_lock();
        let _r = Restore::take(ALL);
        std::env::set_var("JWC_SMTP_HOST", "smtp.test");
        std::env::set_var("JWC_SMTP_USER", "u");
        std::env::set_var("JWC_SMTP_PASSWORD", "p");
        std::env::set_var("JWC_SMTP_FROM", "a@b.test");
        assert!(is_configured());
    }

    #[test]
    fn blank_counts_as_missing() {
        let _g = env_lock();
        let _r = Restore::take(ALL);
        std::env::set_var("JWC_SMTP_HOST", "   ");
        std::env::set_var("JWC_SMTP_USER", "u");
        std::env::set_var("JWC_SMTP_PASSWORD", "p");
        std::env::set_var("JWC_SMTP_FROM", "a@b.test");
        assert!(!is_configured());
    }

    #[test]
    fn missing_host_names_the_variable() {
        let _g = env_lock();
        let _r = Restore::take(ALL);
        let Err(err) = load_config() else {
            panic!("no host configured, yet the config loaded");
        };
        assert!(
            err.to_string().contains("JWC_SMTP_HOST is not set"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn unknown_tls_mode_is_refused_by_name() {
        let _g = env_lock();
        let _r = Restore::take(ALL);
        std::env::set_var("JWC_SMTP_HOST", "smtp.test");
        std::env::set_var("JWC_SMTP_USER", "u");
        std::env::set_var("JWC_SMTP_PASSWORD", "p");
        std::env::set_var("JWC_SMTP_TLS", "ssl");
        let Err(err) = load_config() else {
            panic!("`ssl` is not one of the three modes, yet the config loaded");
        };
        assert!(err.to_string().contains("invalid JWC_SMTP_TLS 'ssl'"));
    }

    #[test]
    fn port_defaults_to_587_and_parses() {
        let _g = env_lock();
        let _r = Restore::take(ALL);
        std::env::set_var("JWC_SMTP_HOST", "smtp.test");
        std::env::set_var("JWC_SMTP_USER", "u");
        std::env::set_var("JWC_SMTP_PASSWORD", "p");
        assert_eq!(load_config().map(|c| c.port).unwrap_or(0), 587);
        std::env::set_var("JWC_SMTP_PORT", "465");
        assert_eq!(load_config().map(|c| c.port).unwrap_or(0), 465);
        std::env::set_var("JWC_SMTP_PORT", "not-a-port");
        assert!(load_config().is_err());
    }
}
