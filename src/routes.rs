use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::store::{BinInfo, BinStore};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const SERVICE_WORKER_JS: &str = include_str!("../assets/service-worker.js");
const FAVICON_ICO: &[u8] = include_bytes!("../assets/favicon.ico");
const MAX_ISSUER_LEN: usize = 8;

#[derive(Clone)]
struct AppState {
    store: Arc<BinStore>,
}

#[derive(Deserialize)]
struct LookupQuery {
    issuer: Option<String>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

pub fn app_router(store: Arc<BinStore>) -> Router {
    let state = AppState { store };

    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/favicon.ico", get(favicon))
        .route("/service-worker.js", get(service_worker))
        .route("/api", get(lookup_bin))
        .route("/api/", get(lookup_bin))
        .fallback(not_found)
        .layer(middleware::from_fn(security_headers))
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

async fn favicon() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/x-icon"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        FAVICON_ICO,
    )
        .into_response()
}

async fn service_worker() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        SERVICE_WORKER_JS,
    )
        .into_response()
}

async fn lookup_bin(
    State(state): State<AppState>,
    query: Result<Query<LookupQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "invalid query string",
            }),
        )
            .into_response();
    };

    let Some(issuer) = query.issuer else {
        return (StatusCode::NOT_FOUND, "404").into_response();
    };

    if issuer.is_empty()
        || issuer.len() > MAX_ISSUER_LEN
        || !issuer.bytes().all(|b| b.is_ascii_digit())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "issuer must be 1 to 8 numeric digits",
            }),
        )
            .into_response();
    }

    match state.store.lookup(&issuer) {
        Some(info) => (StatusCode::OK, Json([info])).into_response(),
        None => (StatusCode::OK, Json::<[BinInfo<'_>; 0]>([])).into_response(),
    }
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "404").into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    fn test_app() -> Router {
        let csv_data = "\
BIN,Brand,Type,Category,Issuer,IssuerPhone,IssuerUrl,isoCode2,isoCode3,CountryName
002102,\"PRIVATE LABEL\",CREDIT,STANDARD,\"CHINA MERCHANTS BANK\",95555,https://english.cmbchina.com,CN,CHN,CHINA
457173,VISA/DANKORT,DEBIT,CLASSIC,\"VESTJYSK BANK A/S\",+4596632000,http://www.vestjyskbank.dk,DK,DNK,DENMARK
";
        let store = BinStore::from_reader(csv_data.as_bytes()).expect("store should load");
        app_router(Arc::new(store))
    }

    async fn send_request(request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let response = test_app().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, bytes)
    }

    async fn send(uri: &str) -> (StatusCode, String, String) {
        let (status, headers, bytes) =
            send_request(Request::builder().uri(uri).body(Body::empty()).unwrap()).await;
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = String::from_utf8_lossy(&bytes).into_owned();
        (status, content_type, body)
    }

    #[tokio::test]
    async fn serves_index_favicon_and_service_worker_with_headers() {
        let (status, headers, bytes) =
            send_request(Request::builder().uri("/").body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(
            headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        assert_eq!(headers.get(header::REFERRER_POLICY).unwrap(), "no-referrer");
        assert_eq!(headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
        assert!(String::from_utf8_lossy(&bytes).contains("BIN Lookup Service"));

        let (icon_status, icon_headers, icon_bytes) = send_request(
            Request::builder()
                .uri("/favicon.ico")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(icon_status, StatusCode::OK);
        assert_eq!(
            icon_headers.get(header::CONTENT_TYPE).unwrap(),
            "image/x-icon"
        );
        assert_eq!(&icon_bytes[..4], &[0x00, 0x00, 0x01, 0x00]);

        let (sw_status, sw_headers, sw_bytes) = send_request(
            Request::builder()
                .uri("/service-worker.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(sw_status, StatusCode::OK);
        assert!(
            sw_headers
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("text/javascript")
        );
        assert_eq!(sw_headers.get(header::CACHE_CONTROL).unwrap(), "no-cache");
        assert!(String::from_utf8_lossy(&sw_bytes).contains("ASSETS_TO_CACHE"));
    }

    #[tokio::test]
    async fn api_returns_properly_mapped_bin_array() {
        for path in ["/api?issuer=457173&extra=ignored", "/api/?issuer=457173"] {
            let (status, content_type, body) = send(path).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(content_type, "application/json");
            let parsed: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                parsed,
                serde_json::json!([{
                    "BIN": "457173",
                    "Brand": "VISA/DANKORT",
                    "Type": "DEBIT",
                    "Category": "CLASSIC",
                    "Issuer": "VESTJYSK BANK A/S",
                    "IssuerPhone": "+4596632000",
                    "IssuerUrl": "http://www.vestjyskbank.dk",
                    "isoCode2": "DK",
                    "isoCode3": "DNK",
                    "CountryName": "DENMARK"
                }])
            );
        }
    }

    #[tokio::test]
    async fn serves_healthz_with_security_headers() {
        let (status, headers, bytes) = send_request(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(bytes.is_empty());
        assert_eq!(
            headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        assert_eq!(headers.get(header::REFERRER_POLICY).unwrap(), "no-referrer");
        assert_eq!(headers.get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    }

    #[tokio::test]
    async fn api_handles_not_found_invalid_and_missing_queries() {
        let (status, _, body) = send("/api?issuer=000000").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "[]");

        let (status, _, body) = send("/api?issuer=45717300").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "[]");

        let (status, _, body) = send("/api?issuer=002102").await;
        assert_eq!(status, StatusCode::OK);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed[0]["BIN"], "002102");

        for invalid_uri in [
            "/api?issuer=",
            "/api?issuer=DEBIT",
            "/api?issuer=123456789",
            "/api?issuer=%ZZ",
            "/api?issuer=457173&issuer=002102",
        ] {
            let (status, content_type, body) = send(invalid_uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "uri: {invalid_uri}");
            assert_eq!(content_type, "application/json", "uri: {invalid_uri}");
            let parsed: Value = serde_json::from_str(&body).unwrap();
            assert!(
                parsed.get("error").and_then(Value::as_str).is_some(),
                "uri: {invalid_uri}"
            );
        }

        let (status, _, body) = send("/api").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, "404");

        let (status, _, body) = send("/api?bin=457173").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, "404");

        let (status, _, body) = send("/unknown").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, "404");
    }

    #[tokio::test]
    async fn supports_head_and_gzip_compression() {
        let (head_status, _, head_bytes) = send_request(
            Request::builder()
                .method(Method::HEAD)
                .uri("/api?issuer=457173")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(head_status, StatusCode::OK);
        assert!(head_bytes.is_empty());

        let (gzip_status, gzip_headers, gzip_bytes) = send_request(
            Request::builder()
                .uri("/")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(gzip_status, StatusCode::OK);
        assert_eq!(gzip_headers.get(header::CONTENT_ENCODING).unwrap(), "gzip");
        assert_eq!(&gzip_bytes[..2], &[0x1f, 0x8b]);
    }
}
