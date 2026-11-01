#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use flash_dance_web::app::{App, shell};
    use leptos::prelude::*;
    use leptos_axum::{LeptosRoutes, generate_route_list};
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // APP_LOG_LEVEL (default debug) for this crate, warn for everything else.
    let level = std::env::var("APP_LOG_LEVEL")
        .ok()
        .and_then(|l| l.parse().ok())
        .unwrap_or(tracing::Level::DEBUG);
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_target("flash_dance_web", level)
                .with_default(tracing::Level::WARN),
        )
        .init();

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
        })
        .await
        .expect("server failed");
}

/// The wasm half of this crate has no `main`, but cargo still wants one.
#[cfg(not(feature = "ssr"))]
fn main() {}
