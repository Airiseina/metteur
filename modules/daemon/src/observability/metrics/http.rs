//! HTTP server exposing `/metrics` and `/healthz`.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;

use super::Metrics;

/// Serves GET /metrics and GET /healthz until the process exits.
pub async fn serve(addr: std::net::SocketAddr, metrics: Arc<Metrics>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("metrics server listening on {addr}");
    axum::serve(listener, router(metrics)).await
}

/// Builds the metrics router (also used by tests via `tower::ServiceExt`).
fn router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(metrics)
}

async fn metrics_handler(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], metrics.render())
}

async fn healthz_handler() -> &'static str {
    "ok\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use tower::ServiceExt;

    async fn request(app: Router, path: &str) -> axum::http::Response<axum::body::Body> {
        app.oneshot(
            axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn serves_metrics_healthz_and_404() {
        let m = Metrics::default();
        m.workspaces_active.store(2, std::sync::atomic::Ordering::Relaxed);
        let app = router(Arc::new(m));

        let res = request(app.clone(), "/metrics").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "text/plain; version=0.0.4");
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert!(std::str::from_utf8(&body).unwrap().contains("metteur_workspaces_active 2\n"));

        let res = request(app.clone(), "/healthz").await;
        assert_eq!(res.status(), StatusCode::OK);

        let res = request(app, "/nope").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
