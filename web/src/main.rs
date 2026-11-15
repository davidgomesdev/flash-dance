#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use flash_dance_web::app::{App, shell};
    use leptos::prelude::*;
    use leptos_axum::{LeptosRoutes, generate_route_list};

    // APP_LOG_LEVEL (default debug) for this crate and the project's libraries,
    // warn for everything else.
    let level = std::env::var("APP_LOG_LEVEL")
        .ok()
        .and_then(|l| l.parse().ok())
        .unwrap_or(tracing::Level::DEBUG);
    let telemetry = telemetry(level);

    let config = get_configuration(None).expect("no leptos configuration");
    let options = config.leptos_options;
    let address = options.site_addr;
    let routes = generate_route_list(App);

    let app = Router::new()
        .leptos_routes(&options, routes, {
            let options = options.clone();
            move || shell(options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(options);

    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .unwrap_or_else(|e| panic!("cannot listen on {address}: {e}"));
    tracing::info!("flash-dance on http://{address}");
    if let Err(e) = flash_dance_web::runner::start(flash_dance_web::runner::PANEL, None) {
        tracing::warn!("cannot start the panel: {e}");
    }
    let bridge = flash_dance_web::mqtt::options().map(|options| {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        (
            stop,
            tokio::spawn(flash_dance_web::mqtt::run(options, stopped)),
        )
    });
    if bridge.is_none() {
        tracing::info!("mqtt: off (set FLASH_DANCE_MQTT to a broker to turn it on)");
    }

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down (Ctrl+C again to force it)");
            // A way out if anything below ever hangs: a second Ctrl+C exits at once,
            // at the price of whatever has not been cleaned up yet.
            tokio::spawn(async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::warn!("forced exit; the Launchpad may be left lit");
                std::process::exit(130);
            });
            // The bridge first, so no "on" from HA starts a session after the stop.
            if let Some((stop, bridge)) = bridge {
                let _ = stop.send(());
                let _ = bridge.await;
            }
            // Stop the session and wait for it, so its `Drop` clears the LEDs and
            // leaves programmer mode before the process goes. A SIGKILL skips this
            // and leaves the device lit — nothing in-process can help with that.
            flash_dance_web::runner::stop();
            telemetry.shutdown().await;
        })
        .await
        .expect("server failed");
}

/// Stdout always. `LOKI_URL` adds Loki, `OTLP_ENDPOINT` (gRPC, e.g. http://localhost:4317)
/// adds traces. Both off when unset.
#[cfg(feature = "ssr")]
struct Telemetry {
    loki: Option<(
        tracing_loki::BackgroundTaskController,
        tokio::task::JoinHandle<()>,
    )>,
    otel: Option<opentelemetry_sdk::trace::TracerProvider>,
}

#[cfg(feature = "ssr")]
impl Telemetry {
    /// Flushes both, giving each [`FLUSH`] at most: an unreachable collector must
    /// not keep the process alive, and what it has not taken by then is lost.
    async fn shutdown(self) {
        use std::time::Duration;
        const FLUSH: Duration = Duration::from_secs(2);

        if let Some(otel) = self.otel {
            // `shutdown` blocks until the batch exporter has flushed, and with no
            // collector listening it never does. A plain thread, not
            // `spawn_blocking`: the runtime waits for its blocking tasks before the
            // process can exit, so one stuck there would hang the exit instead.
            let (done, flushed) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = done.send(otel.shutdown());
            });
            match tokio::time::timeout(FLUSH, flushed).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => tracing::warn!("OTLP did not flush: {error}"),
                Ok(Err(_)) | Err(_) => {
                    tracing::warn!("OTLP did not flush in {FLUSH:?}; leaving it")
                }
            }
        }
        if let Some((controller, task)) = self.loki {
            // Logged before Loki stops, so this one still has a chance of arriving.
            let flushed = tokio::time::timeout(FLUSH, async {
                controller.shutdown().await;
                let _ = task.await;
            });
            if flushed.await.is_err() {
                tracing::warn!("Loki did not flush in {FLUSH:?}; leaving it");
            }
        }
    }
}

#[cfg(feature = "ssr")]
fn telemetry(level: tracing::Level) -> Telemetry {
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_otlp::WithExportConfig;
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // ponytail: no Loki reachability probe; the Loki task retries on its own.
    let loki = std::env::var("LOKI_URL").ok().map(|url| {
        tracing_loki::builder()
            .label("service", "flash-dance")
            .expect("bad Loki label")
            .build_controller_url(url.parse().expect("invalid LOKI_URL"))
            .expect("cannot build the Loki layer")
    });
    // Logged once the subscriber exists: before that, nothing would hear it.
    let mut otel_failed = None;
    let otel = std::env::var("OTLP_ENDPOINT").ok().and_then(|endpoint| {
        opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint),
            )
            .with_trace_config(opentelemetry_sdk::trace::Config::default().with_resource(
                opentelemetry_sdk::Resource::new(vec![opentelemetry::KeyValue::new(
                    "service.name",
                    "flash-dance",
                )]),
            ))
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .inspect_err(|e| otel_failed = Some(e.to_string()))
            .ok()
    });
    let otel_layer = otel
        .as_ref()
        .map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("flash-dance")));
    let (loki_layer, loki) = match loki {
        Some((layer, controller, task)) => (Some(layer), Some((controller, tokio::spawn(task)))),
        None => (None, None),
    };

    // The OTel layer goes first: it only implements Layer<Registry>.
    tracing_subscriber::registry()
        .with(otel_layer)
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_target("flash_dance_web", level)
                .with_target("flash_dance_effects", level)
                .with_target("flash_dance_audio", level)
                .with_target("flash_dance", level)
                .with_default(tracing::Level::WARN),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(loki_layer)
        .init();
    if let Some(error) = otel_failed {
        tracing::warn!("OTLP exporter failed: {error}; continuing without it");
    }
    Telemetry { loki, otel }
}

/// The wasm half of this crate has no `main`, but cargo still wants one.
#[cfg(not(feature = "ssr"))]
fn main() {}
