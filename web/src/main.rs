#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use flash_dance_web::app::{App, shell};
    use leptos::prelude::*;
    use leptos_axum::{LeptosRoutes, generate_route_list};

    // APP_LOG_LEVEL (default debug) for this crate, warn for everything else.
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
    async fn shutdown(self) {
        if let Some(otel) = self.otel {
            let _ = otel.shutdown();
        }
        if let Some((controller, task)) = self.loki {
            controller.shutdown().await;
            let _ = task.await;
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
            .inspect_err(|e| eprintln!("OTLP exporter failed: {e}; continuing without it"))
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
                .with_default(tracing::Level::WARN),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(loki_layer)
        .init();
    Telemetry { loki, otel }
}

/// The wasm half of this crate has no `main`, but cargo still wants one.
#[cfg(not(feature = "ssr"))]
fn main() {}
