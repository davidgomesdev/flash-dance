#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use flash_dance_web::app::{App, shell};
    use leptos::prelude::*;
    use leptos_axum::{LeptosRoutes, generate_route_list};

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
    println!("flash-dance on http://{address}");

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            // Ctrl+C would otherwise leave the effect running, holding the MIDI
            // port and the device lit — its stdin closing is what stops it.
            flash_dance_web::runner::stop();
        })
        .await
        .expect("server failed");
}

/// The wasm half of this crate has no `main`, but cargo still wants one.
#[cfg(not(feature = "ssr"))]
fn main() {}
