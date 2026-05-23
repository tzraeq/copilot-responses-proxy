use crate::config::AppConfig;
use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{HeaderName, HeaderValue};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::error::Error;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

type BoxError = Box<dyn Error + Send + Sync>;
type ResponseBody = BoxBody<Bytes, BoxError>;

pub type SharedConfig = Arc<RwLock<AppConfig>>;

#[derive(Clone)]
pub struct ProxyState {
    config: SharedConfig,
    client: reqwest::Client,
    log_dir: PathBuf,
}

#[derive(Debug)]
pub struct ForwardBody {
    pub body: Vec<u8>,
    pub rewrites: Vec<&'static str>,
    pub summary: Option<Value>,
    pub forwarded_summary: Option<Value>,
}

pub async fn serve(shared_config: SharedConfig, log_dir: PathBuf) -> Result<()> {
    let bind_config = read_config(&shared_config);
    let addr: SocketAddr = format!("{}:{}", bind_config.listen_host, bind_config.listen_port)
        .parse()
        .with_context(|| {
            format!(
                "invalid listen address {}:{}",
                bind_config.listen_host, bind_config.listen_port
            )
        })?;

    let client = reqwest::Client::builder()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()?;

    let state = ProxyState {
        config: shared_config,
        client,
        log_dir,
    };

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("Copilot Responses proxy listening on http://{addr}/v1/responses");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let state = state.clone();

        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle_request(state, request).await) }
            });

            let builder = AutoBuilder::new(TokioExecutor::new());
            if let Err(error) = builder.serve_connection(io, service).await {
                eprintln!("proxy connection error: {error}");
            }
        });
    }
}

async fn handle_request(state: ProxyState, request: Request<Incoming>) -> Response<ResponseBody> {
    if request.uri().path() == "/health" && request.method() == Method::GET {
        return health_response(state);
    }

    proxy_handler(state, request).await
}

fn health_response(state: ProxyState) -> Response<ResponseBody> {
    let config = read_config(&state.config);
    json_response(
        StatusCode::OK,
        json!({
            "status": "ok",
            "endpoint": config.endpoint(),
            "upstream_url": config.upstream_url,
            "drop_truncation": config.drop_truncation,
            "active_token": config.active_token,
            "token_count": config.tokens.len(),
            "log_requests": config.log_requests,
        }),
    )
}

async fn proxy_handler(state: ProxyState, request: Request<Incoming>) -> Response<ResponseBody> {
    let started_at = now_millis();
    let (parts, body) = request.into_parts();
    let raw_body = match body.collect().await {
        Ok(body) => body.to_bytes(),
        Err(error) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                format!("failed to read request body: {error}"),
            );
        }
    };

    let config = read_config(&state.config);
    let forward = prepare_forward_body(&raw_body, config.drop_truncation);
    let method = reqwest_method(&parts.method);
    let active_token = config.active_token_value().map(str::to_string);
    let upstream_url = config.upstream_url.clone();

    let mut outbound = state.client.request(method, &upstream_url);
    outbound = apply_request_headers(outbound, &parts.headers, active_token.as_deref());
    if parts.method != Method::GET && parts.method != Method::HEAD {
        outbound = outbound.body(forward.body.clone());
    }

    let upstream = match outbound.send().await {
        Ok(response) => response,
        Err(error) => {
            write_request_log(
                &state.log_dir,
                &config,
                started_at,
                None,
                &forward,
                Some(error.to_string()),
            )
            .await;
            return json_error(
                StatusCode::BAD_GATEWAY,
                format!("proxy upstream failure: {error}"),
            );
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = response_headers(upstream.headers());

    write_request_log(
        &state.log_dir,
        &config,
        started_at,
        Some(status.as_u16()),
        &forward,
        None,
    )
    .await;

    let stream = upstream
        .bytes_stream()
        .map_ok(Frame::data)
        .map_err(|error| -> BoxError { Box::new(error) });
    let mut response = Response::new(StreamBody::new(stream).boxed());
    *response.status_mut() = status;
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_bytes(&value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }

    response
}

pub fn prepare_forward_body(raw_body: &[u8], drop_truncation: bool) -> ForwardBody {
    let parsed = serde_json::from_slice::<Value>(raw_body).ok();
    let summary = parsed.as_ref().map(summarize_body);

    if !drop_truncation {
        return ForwardBody {
            body: raw_body.to_vec(),
            rewrites: Vec::new(),
            summary,
            forwarded_summary: None,
        };
    }

    let Some(Value::Object(mut object)) = parsed else {
        return ForwardBody {
            body: raw_body.to_vec(),
            rewrites: Vec::new(),
            summary,
            forwarded_summary: None,
        };
    };

    if object.remove("truncation").is_none() {
        return ForwardBody {
            body: raw_body.to_vec(),
            rewrites: Vec::new(),
            summary,
            forwarded_summary: None,
        };
    }

    let forwarded = Value::Object(object);
    let body = serde_json::to_vec(&forwarded).unwrap_or_else(|_| raw_body.to_vec());
    ForwardBody {
        body,
        rewrites: vec!["drop_truncation"],
        summary,
        forwarded_summary: Some(summarize_body(&forwarded)),
    }
}

fn read_config(shared_config: &SharedConfig) -> AppConfig {
    shared_config.read().expect("config lock poisoned").clone()
}

fn reqwest_method(method: &Method) -> reqwest::Method {
    reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST)
}

fn apply_request_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
    active_token: Option<&str>,
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        if should_skip_request_header(name.as_str()) {
            continue;
        }
        if name == header::AUTHORIZATION && active_token.is_some() {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }

    if let Some(token) = active_token {
        builder = builder.bearer_auth(token);
    }

    builder
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, Vec<u8>)> {
    headers
        .iter()
        .filter(|(name, _)| !should_skip_response_header(name.as_str()))
        .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
        .collect()
}

fn should_skip_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

fn should_skip_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "content-encoding"
    )
}

fn summarize_body(value: &Value) -> Value {
    let input_item_count = value
        .get("input")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let tool_count = value
        .get("tools")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);

    json!({
        "model": value.get("model").cloned().unwrap_or(Value::Null),
        "stream": value.get("stream").cloned().unwrap_or(Value::Null),
        "store": value.get("store").cloned().unwrap_or(Value::Null),
        "truncation": value.get("truncation").cloned().unwrap_or(Value::Null),
        "max_output_tokens": value.get("max_output_tokens").cloned().unwrap_or(Value::Null),
        "input_item_count": input_item_count,
        "tool_count": tool_count,
    })
}

async fn write_request_log(
    log_dir: &Path,
    config: &AppConfig,
    started_at: u128,
    status: Option<u16>,
    forward: &ForwardBody,
    error: Option<String>,
) {
    if !config.log_requests {
        return;
    }

    let entry = json!({
        "ts_ms": started_at,
        "elapsed_ms": now_millis().saturating_sub(started_at),
        "upstream_url": config.upstream_url,
        "status": status,
        "rewrites": forward.rewrites,
        "summary": forward.summary,
        "forwarded_summary": forward.forwarded_summary,
        "error": error,
    });

    if let Err(error) = append_jsonl(log_dir, &entry).await {
        eprintln!("failed to write proxy log: {error}");
    }
}

async fn append_jsonl(log_dir: &Path, entry: &Value) -> Result<()> {
    tokio::fs::create_dir_all(log_dir).await?;
    let path = log_dir.join("proxy.jsonl");
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(serde_json::to_string(entry)?.as_bytes())
        .await?;
    file.write_all(b"\n").await?;
    Ok(())
}

fn json_response(status: StatusCode, value: Value) -> Response<ResponseBody> {
    let mut response = Response::new(full_body(value.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response<ResponseBody> {
    let body = json!({
        "error": {
            "message": message.into(),
            "type": "proxy_error"
        }
    });

    json_response(status, body)
}

fn full_body(body: impl Into<Bytes>) -> ResponseBody {
    Full::new(body.into())
        .map_err(|never| match never {})
        .boxed()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_top_level_truncation_when_enabled() {
        let raw = br#"{"model":"gpt-5.5","truncation":"disabled","stream":true}"#;
        let forward = prepare_forward_body(raw, true);
        let value: Value = serde_json::from_slice(&forward.body).unwrap();

        assert_eq!(forward.rewrites, vec!["drop_truncation"]);
        assert!(value.get("truncation").is_none());
        assert_eq!(value["model"], "gpt-5.5");
    }

    #[test]
    fn preserves_truncation_when_disabled() {
        let raw = br#"{"truncation":"disabled"}"#;
        let forward = prepare_forward_body(raw, false);

        assert!(forward.rewrites.is_empty());
        assert_eq!(forward.body, raw);
    }
}
