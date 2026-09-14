//! Logs and traces. One call in `main`, one guard to drop at the end.
use anyhow::Context as _;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::config::TelemetrySettings;

/// Holds the tracer provider so spans can be flushed before the process exits.
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl TelemetryGuard {
    /// Flushes buffered spans. Skipping this loses every span still in the batch
    /// queue, which is most of them for a short-lived process.
    pub fn shutdown(self) {
        if let Err(error) = self.provider.shutdown() {
            tracing::warn!(%error, "tracer provider shutdown failed");
        }
    }
}

/// Installs the subscriber. Call once, before anything logs.
///
/// # Errors
/// The OTLP exporter cannot be built (bad endpoint, no tokio runtime).
pub fn init(settings: &TelemetrySettings) -> anyhow::Result<TelemetryGuard> {
    // `RUST_LOG` wins; "info" is the fallback so a bare `cargo run` still logs.
    // `OtelAxumLayer` emits its HTTP span at TRACE under `otel::tracing`; without
    // that directive the span is filtered out and every request logs a warning.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("otel::tracing=trace".parse()?);

    // Two different layer types, so both need boxing to share one variable.
    let fmt_layer = if settings.json_logs {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().pretty().boxed()
    };

    let provider = build_tracer_provider(settings.otlp_endpoint.as_deref())?;

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        // Always on, exporter or not: `OtelAxumLayer` needs a real span context
        // to attach `traceparent` to, and warns on every request without one.
        // Without `use opentelemetry::trace::TracerProvider as _` this method
        // does not exist.
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(env!("CARGO_PKG_NAME"))))
        .init();

    Ok(TelemetryGuard { provider })
}

/// The provider always exists so trace ids are minted and propagated; only the
/// exporter is optional. `None` means spans are created and dropped locally.
fn build_tracer_provider(endpoint: Option<&str>) -> anyhow::Result<SdkTracerProvider> {
    let mut builder = SdkTracerProvider::builder()
        // Picks up OTEL_SERVICE_NAME and the other OTEL_RESOURCE_ATTRIBUTES.
        .with_resource(Resource::builder().build());

    if let Some(endpoint) = endpoint {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("building the OTLP span exporter")?;
        builder = builder.with_batch_exporter(exporter);
    }

    let provider = builder.build();
    global::set_tracer_provider(provider.clone());
    // The default global propagator is a no-op: without this line no trace
    // context is read or written and nothing warns about it.
    global::set_text_map_propagator(TraceContextPropagator::new());
    Ok(provider)
}
