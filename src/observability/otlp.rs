//! OTLP trace exporter wiring.
//!
//! Behaviour:
//!
//! - Runtime gate is the `JWC_OTLP_ENDPOINT` env var. Unset ⇒ this is a
//!   no-op and `init_otlp_from_env` returns `Ok(None)`.
//! - Compile-time gate is the `otlp` Cargo feature. Without the feature
//!   the function still exists (so callers don't need their own `cfg`),
//!   it just returns `Ok(None)` unconditionally and `OtlpGuard` is a ZST
//!   that does nothing on `Drop`.
//! - Service name comes from `JWC_SERVICE_NAME`, default `"jwc"`. It shows
//!   up as `service.name` on every exported span — that's what Tempo /
//!   Jaeger / Honeycomb key their service picker off, so leaving it on
//!   the default means every JWC pod blurs into one entry in the UI.
//! - The W3C Trace Context propagator is installed globally so inbound
//!   `traceparent` headers reattach to the upstream trace (the server's
//!   request handler already parses `traceparent` for the request id —
//!   this just makes the same value flow through any spans the runtime
//!   or downstream calls open).
//!
//! The guard's `Drop` flushes the batch span processor and shuts the
//! provider down. Without that, the last batch of spans never reaches
//! the collector when the process exits cleanly.

use anyhow::Result;

/// Lives for the lifetime of the server. Drop = flush + shut down.
pub struct OtlpGuard {
    #[cfg(feature = "otlp")]
    provider: Option<opentelemetry_sdk::trace::TracerProvider>,
}

impl Drop for OtlpGuard {
    fn drop(&mut self) {
        #[cfg(feature = "otlp")]
        if let Some(provider) = self.provider.take() {
            // `shutdown` flushes the BatchSpanProcessor's in-memory queue
            // and waits up to its configured timeout. Errors are logged
            // but not propagated — we're already on the drop path and
            // can't return them anywhere useful.
            if let Err(e) = provider.shutdown() {
                eprintln!("[JWC] OTLP shutdown failed: {e}");
            }
        }
    }
}

/// Read `JWC_OTLP_ENDPOINT` / `JWC_SERVICE_NAME` and, if the endpoint is
/// set, install a `tracing-opentelemetry` layer that forwards spans to
/// the configured OTLP collector over HTTP.
///
/// Must be called from inside a tokio runtime context — the OTLP batch
/// exporter spawns its background flush task on the current runtime.
pub fn init_otlp_from_env() -> Result<Option<OtlpGuard>> {
    let Ok(endpoint) = std::env::var("JWC_OTLP_ENDPOINT") else {
        return Ok(None);
    };
    let endpoint = endpoint.trim().to_string();
    if endpoint.is_empty() {
        return Ok(None);
    }

    init_with_endpoint(&endpoint)
}

#[cfg(not(feature = "otlp"))]
fn init_with_endpoint(_endpoint: &str) -> Result<Option<OtlpGuard>> {
    // Feature disabled at compile time — the env var was set but we have
    // no exporter to wire it into. Warn once so the operator notices a
    // misconfigured deployment (otherwise their dashboards stay empty
    // and they have no signal as to why).
    eprintln!(
        "[JWC] JWC_OTLP_ENDPOINT is set but this binary was built without \
the `otlp` Cargo feature — no traces will be exported. Rebuild with \
`cargo build --features otlp` to enable OTLP."
    );
    Ok(Some(OtlpGuard {}))
}

#[cfg(feature = "otlp")]
fn init_with_endpoint(endpoint: &str) -> Result<Option<OtlpGuard>> {
    use anyhow::Context;
    use opentelemetry::{global, KeyValue};
    use opentelemetry_otlp::{SpanExporter, WithExportConfig};
    use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::TracerProvider, Resource};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let service_name = std::env::var("JWC_SERVICE_NAME").unwrap_or_else(|_| "jwc".to_string());

    // Install the W3C Trace Context propagator globally. The server
    // already extracts the `trace-id` from inbound `traceparent` headers
    // for the request id; this makes the same context flow through any
    // span the runtime or a downstream HTTP call opens via the
    // `opentelemetry::global` propagator API.
    global::set_text_map_propagator(TraceContextPropagator::new());

    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .with_context(|| format!("building OTLP HTTP exporter for {endpoint}"))?;

    let resource = Resource::new(vec![KeyValue::new("service.name", service_name.clone())]);

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(resource)
        .build();

    let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "jwc");
    global::set_tracer_provider(provider.clone());

    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // `try_init` instead of `init` — if the host already installed a
    // global subscriber (some test harnesses do), don't panic. We still
    // hand back the guard so the provider gets shut down cleanly, even
    // though our layer didn't end up attached.
    let init_result = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .try_init();

    if let Err(e) = init_result {
        eprintln!(
            "[JWC] OTLP layer attached but global tracing subscriber was \
already set ({e}); spans from this process may not export."
        );
    }

    Ok(Some(OtlpGuard {
        provider: Some(provider),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `JWC_OTLP_ENDPOINT` unset, init must be a no-op and return
    /// `Ok(None)` regardless of the `otlp` feature state. This is what
    /// makes the call site in `server::serve` safe to invoke
    /// unconditionally.
    #[test]
    fn init_returns_none_when_endpoint_unset() {
        // Use a unique scope: the test runner shares the process env,
        // so explicitly remove the var first in case another test left
        // it behind. We don't try to restore — no other test reads it.
        // SAFETY: tests in this module are the only readers.
        std::env::remove_var("JWC_OTLP_ENDPOINT");
        let result = init_otlp_from_env().expect("init must not error when endpoint is unset");
        assert!(
            result.is_none(),
            "expected Ok(None) with no endpoint configured, got Some(_)"
        );
    }

    /// Empty / whitespace endpoint is treated as unset — operators sometimes
    /// leave `JWC_OTLP_ENDPOINT=""` in a k8s ConfigMap to "disable" tracing,
    /// and that should behave like the var was never set rather than fail
    /// to build an exporter against an empty URL.
    #[test]
    fn init_returns_none_when_endpoint_is_blank() {
        std::env::set_var("JWC_OTLP_ENDPOINT", "   ");
        let result = init_otlp_from_env().expect("blank endpoint must not error");
        assert!(
            result.is_none(),
            "expected Ok(None) for whitespace-only endpoint"
        );
        std::env::remove_var("JWC_OTLP_ENDPOINT");
    }
}
