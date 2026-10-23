use crate::config::{Settings, MAX_PROBE_BYTES, MAX_REQUEST_BYTES};
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Method, Request, Response, StatusCode, Uri},
    routing::{any, get},
    Json, Router,
};
use bytes::Bytes;
use futures_util::{stream, StreamExt};
use rand::Rng;
use reqwest::{Client, Url};
use serde::Serialize;
use std::{io, sync::{atomic::{AtomicU64, Ordering}, Arc, RwLock}, time::Duration};
use tokio::sync::oneshot;

#[derive(Default)]
pub struct Metrics {
    pub requests: AtomicU64,
    pub retries: AtomicU64,
    pub recovered: AtomicU64,
    pub exhausted: AtomicU64,
    pub capacity: AtomicU64,
    pub early_streams: AtomicU64,
    pub active: AtomicU64,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    pub requests: u64, pub retries: u64, pub recovered: u64,
    pub exhausted: u64, pub capacity: u64, pub early_streams: u64, pub active: u64,
}
impl Metrics {
    pub fn snapshot(&self) -> Counters {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Counters { requests:get(&self.requests), retries:get(&self.retries),
            recovered:get(&self.recovered), exhausted:get(&self.exhausted),
            capacity:get(&self.capacity), early_streams:get(&self.early_streams),
            active:get(&self.active) }
    }
}
pub struct ProxyContext {
    pub settings: Arc<RwLock<Settings>>,
    pub metrics: Arc<Metrics>,
    client: Client,
}
impl ProxyContext {
    pub fn new(settings: Arc<RwLock<Settings>>, metrics: Arc<Metrics>) -> Result<Arc<Self>, String> {
        let client = Client::builder()
            .no_gzip().no_brotli().no_deflate()
            .timeout(Duration::from_secs(360))
            .build().map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Arc::new(Self { settings, metrics, client }))
    }
}

pub async fn run_server(listener:tokio::net::TcpListener, context:Arc<ProxyContext>, shutdown:oneshot::Receiver<()>) -> Result<(), String> {
    let routes = Router::new().route("/__health", get(health))
        .fallback(any(proxy_request)).with_state(context);
    axum::serve(listener, routes)
        .with_graceful_shutdown(async move { let _ = shutdown.await; })
        .await.map_err(|e| format!("Proxy stopped with error: {e}"))
}
async fn health(State(ctx): State<Arc<ProxyContext>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok":true,
        "service":"codex-recovery-manager",
        "retryEnabled":ctx.settings.read().map(|g|g.retry_enabled).unwrap_or(false),
        "metrics":ctx.metrics.snapshot()
    }))
}
fn skip_header(name: &str, response: bool) -> bool {
    matches!(name, "host" | "connection" | "keep-alive" | "proxy-authenticate" |
        "proxy-authorization" | "te" | "trailers" | "transfer-encoding" | "upgrade") ||
        (response && name == "content-length") ||
        (!response && (name == "accept-encoding" || name == "content-length"))
}
fn copy_headers(dest:&mut HeaderMap, source:&HeaderMap, response:bool) {
    for (key,val) in source.iter() {
        if !skip_header(key.as_str(),response) { dest.append(key.clone(),val.clone()); }
    }
}
fn error_response(status:StatusCode, message:impl AsRef<str>) -> Response<Body> {
    let text = serde_json::json!({"error":{"type":"recovery_proxy_error","message":message.as_ref()}});
    Response::builder().status(status)
        .header("content-type","application/json")
        .body(Body::from(text.to_string())).unwrap_or_else(|_|Response::new(Body::empty()))
}
fn mapped_url(base: &str, uri: &Uri) -> Result<Url,String> {
    let path=uri.path();
    if path!="/v1" && !path.starts_with("/v1/") { return Err("Unsupported API path".into()); }
    let mut url=Url::parse(base).map_err(|e| format!("Invalid upstream URL: {e}"))?;
    let base_path=url.path().trim_end_matches('/');
    // Explicit prefix mapping: localhost /v1/responses -> upstream /custom/api/responses.
    let new_path=format!("{}{}",base_path, &path[3..]);
    url.set_path(if new_path.is_empty(){"/"}else{&new_path});
    url.set_query(uri.query());
    Ok(url)
}
fn capacity_message(text: &str) -> bool {
    let text=text.to_ascii_lowercase();
    ["selected model is at capacity","server_overloaded","server is overloaded",
     "servers are currently overloaded","temporarily at capacity",
     "serveroverloaded"].iter().any(|needle|text.contains(needle))
}
fn is_capacity_json(value: &serde_json::Value, event: &str) -> bool {
    let category = value.get("type").and_then(|x|x.as_str()).unwrap_or(event);
    let error=value.get("error");
    if error.is_none() && !["error","response.failed","turn.failed"].contains(&category) {
        return false;
    }
    let detail=error.unwrap_or(value);
    capacity_message(&detail.to_string())
}
fn next_sse_frame(buffer: &[u8]) -> Option<(usize,usize)> {
    // Handles both CRLF and LF SSE delimiters, including chunk boundaries.
    for i in 0..buffer.len().saturating_sub(1) {
        if &buffer[i..i+2]==b"\n\n" { return Some((i,i+2)); }
        if i+3<buffer.len() && &buffer[i..i+4]==b"\r\n\r\n" { return Some((i,i+4)); }
    }
    None
}
fn sse_decision(frame: &[u8]) -> SseDecision {
    let s=String::from_utf8_lossy(frame);
    let mut event="";
    let mut data=Vec::new();
    for line in s.lines() {
        if let Some(x)=line.strip_prefix("event:") { event=x.trim(); }
        if let Some(x)=line.strip_prefix("data:") { data.push(x.trim_start()); }
    }
    let raw=data.join("\n");
    let value=serde_json::from_str::<serde_json::Value>(&raw).ok();
    let event_name=if !event.is_empty() {event}
        else {value.as_ref().and_then(|x|x.get("type")).and_then(|x|x.as_str()).unwrap_or("")};
    if let Some(value)=&value {
        if is_capacity_json(value,event_name) {return SseDecision::Capacity;}
    } else if event_name.is_empty() && capacity_message(&raw) {
        return SseDecision::Capacity;
    }
    if raw=="[DONE]" { return SseDecision::Commit; }
    if ["","response.created","response.in_progress","response.metadata",
        "codex.response.metadata","response.rate_limits.updated","ping",
        "keepalive","responsesapi.websocket_timing"].contains(&event_name) {
        return SseDecision::Buffer;
    }
    SseDecision::Commit
}
#[derive(PartialEq,Debug)]
enum SseDecision {Buffer,Capacity,Commit}
#[derive(Debug)]
enum RetryReason {Capacity {status:StatusCode,headers:HeaderMap,body:Vec<u8>},EarlyStream}
enum Attempt {Done(Response<Body>),Retry(RetryReason)}
fn respond_with(status:StatusCode, headers:&HeaderMap, body:Body) -> Response<Body> {
    let mut response=Response::builder().status(status).body(body)
        .unwrap_or_else(|_|Response::new(Body::empty()));
    copy_headers(response.headers_mut(), headers, true);
    response
}
async fn once(
    ctx:&Arc<ProxyContext>, method:&Method,headers:&HeaderMap,body:&Bytes,
    dest:&Url, inspect:bool
) -> Result<Attempt,String> {
    let mut outbound=HeaderMap::new();
    copy_headers(&mut outbound,headers,false);
    outbound.insert("accept-encoding", reqwest::header::HeaderValue::from_static("identity"));
    let response = ctx.client.request(method.clone(),dest.clone())
        .headers(outbound).body(body.clone())
        .send().await.map_err(|e|format!("Upstream connection failed: {e}"))?;
    let status=response.status();
    let response_headers=response.headers().clone();
    let content_type=response.headers().get("content-type")
        .and_then(|x|x.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
    let sse=content_type.contains("text/event-stream");
    let json=content_type.contains("application/json") || content_type.contains("+json");
    let encoded=response.headers().get("content-encoding").is_some();
    if !inspect || encoded || (!sse && !json) {
        return Ok(Attempt::Done(respond_with(status,&response_headers,Body::from_stream(
            response.bytes_stream().map(|b|b.map_err(io::Error::other))
        ))));
    }

    let mut stream=response.bytes_stream();
    let mut prefix=Vec::new();
    let mut tail=Vec::new();
    let mut saw_model_output=false;
    loop {
        let next=tokio::time::timeout(Duration::from_secs(65),stream.next()).await;
        let chunk=match next {
            Ok(Some(Ok(data)))=>Some(data),
            Ok(Some(Err(_)))|Err(_)=>{
                if sse && !saw_model_output {return Ok(Attempt::Retry(RetryReason::EarlyStream));}
                return Err("Upstream stream failed".into());
            }
            Ok(None)=>None,
        };
        if let Some(chunk)=chunk {
            prefix.extend_from_slice(&chunk);
            if sse {
                tail.extend_from_slice(&chunk);
                while let Some((index,end))=next_sse_frame(&tail) {
                    let decision=sse_decision(&tail[..index]);
                    tail.drain(..end);
                    match decision {
                        SseDecision::Capacity => return Ok(Attempt::Retry(RetryReason::Capacity {
                            status,headers:response_headers,body:prefix,
                        })),
                        SseDecision::Commit => {saw_model_output=true;break;}
                        SseDecision::Buffer => {}
                    }
                }
            }
            if saw_model_output || prefix.len()>=MAX_PROBE_BYTES {
                // Never replay the request once bytes are delivered to Codex.
                let first=Bytes::from(prefix);
                let remaining=stream.map(|b|b.map_err(io::Error::other));
                let combined=stream::once(async move {Ok::<Bytes,io::Error>(first)}).chain(remaining);
                return Ok(Attempt::Done(respond_with(status,&response_headers,Body::from_stream(combined))));
            }
        } else {
            if json {
                if let Ok(payload)=serde_json::from_slice::<serde_json::Value>(&prefix) {
                    if is_capacity_json(&payload,"") {
                        return Ok(Attempt::Retry(RetryReason::Capacity { status,headers:response_headers,body:prefix }));
                    }
                }
            }
            if sse {
                if !tail.is_empty() && sse_decision(&tail)==SseDecision::Capacity {
                    return Ok(Attempt::Retry(RetryReason::Capacity {status,headers:response_headers,body:prefix}));
                }
                if !saw_model_output {
                    return Ok(Attempt::Retry(RetryReason::EarlyStream));
                }
            }
            return Ok(Attempt::Done(respond_with(status,&response_headers,Body::from(prefix))));
        }
    }
}
struct ActiveGuard(Arc<Metrics>);
impl Drop for ActiveGuard {
    fn drop(&mut self) { self.0.active.fetch_sub(1,Ordering::Relaxed); }
}
fn track_response(response:Response<Body>,guard:ActiveGuard) -> Response<Body> {
    // Keep the request active until the SSE/body stream is consumed or dropped,
    // not merely until Axum has produced the HTTP response headers.
    let (parts,body)=response.into_parts();
    let stream=Box::pin(body.into_data_stream());
    let tracked=stream::unfold((stream,guard),|(mut stream,guard)| async move {
        stream.next().await.map(|item|(item,(stream,guard)))
    });
    Response::from_parts(parts,Body::from_stream(tracked))
}
async fn proxy_request(State(ctx):State<Arc<ProxyContext>>,request:Request<Body>) -> Response<Body> {
    let (parts,body)=request.into_parts();
    if parts.uri.path()!="/v1" && !parts.uri.path().starts_with("/v1/") {
        return error_response(StatusCode::NOT_FOUND,"Not a model API route");
    }
    ctx.metrics.requests.fetch_add(1,Ordering::Relaxed);
    ctx.metrics.active.fetch_add(1,Ordering::Relaxed);
    let active_guard=ActiveGuard(ctx.metrics.clone());
    let cfg=match ctx.settings.read() {
        Ok(guard)=>guard.clone(),
        Err(_)=>return error_response(StatusCode::INTERNAL_SERVER_ERROR,"Recovery state unavailable")
    };
    let base=match cfg.upstream_base_url.as_deref() {
        Some(v)=>v,None=>return error_response(StatusCode::BAD_GATEWAY,"No upstream configured")
    };
    let url=match mapped_url(base,&parts.uri) {
        Ok(u)=>u,Err(e)=>return error_response(StatusCode::BAD_REQUEST,e)
    };
    let body=match to_bytes(body,MAX_REQUEST_BYTES).await {
        Ok(b)=>b,Err(_)=>return error_response(StatusCode::PAYLOAD_TOO_LARGE,"Request exceeds 32 MB")
    };
    let inspect=parts.method==Method::POST && parts.uri.path()=="/v1/responses" && cfg.retry_enabled;
    let mut last_retry=None;
    for attempt in 0..=cfg.max_retries {
        match once(&ctx,&parts.method,&parts.headers,&body,&url,inspect).await {
            Ok(Attempt::Done(response))=>{
                if attempt>0 {ctx.metrics.recovered.fetch_add(1,Ordering::Relaxed);}
                return track_response(response,active_guard);
            }
            Ok(Attempt::Retry(reason))=>{
                match &reason {
                    RetryReason::Capacity{..}=>{ctx.metrics.capacity.fetch_add(1,Ordering::Relaxed);}
                    RetryReason::EarlyStream=>{ctx.metrics.early_streams.fetch_add(1,Ordering::Relaxed);}
                }
                last_retry=Some(reason);
                if attempt>=cfg.max_retries {break;}
                ctx.metrics.retries.fetch_add(1,Ordering::Relaxed);
                let shift=(attempt as u32).min(9);
                let raw=cfg.base_delay_ms.saturating_mul(1u64 << shift);
                let capped=raw.min(cfg.max_delay_ms);
                let jitter=rand::thread_rng().gen_range(80u64..=120u64);
                tokio::time::sleep(Duration::from_millis(capped.saturating_mul(jitter)/100)).await;
            }
            Err(error)=>return error_response(StatusCode::BAD_GATEWAY,error)
        }
    }
    ctx.metrics.exhausted.fetch_add(1,Ordering::Relaxed);
    let response=match last_retry {
        Some(RetryReason::Capacity{status,headers,body})=>{
            // Preserve the final upstream failure, but suppress the misleading 200 SSE success status.
            if status.is_success(){
                error_response(StatusCode::SERVICE_UNAVAILABLE,"Model remains at capacity after bounded retries")
            }else {
                respond_with(status,&headers,Body::from(body))
            }
        }
        _=>error_response(StatusCode::BAD_GATEWAY,"Upstream stream ended before model output")
    };
    track_response(response,active_guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sse_error_only_and_safe_commit() {
        assert_eq!(sse_decision(br#"event: response.created
data: {"type":"response.created"}"#),SseDecision::Buffer);
        assert_eq!(sse_decision(br#"event: response.metadata
data: {"type":"response.metadata"}"#),SseDecision::Buffer);
        assert_eq!(sse_decision(br#"event: error
data: {"type":"error","error":{"code":"server_overloaded","message":"Selected model is at capacity"}}"#),SseDecision::Capacity);
        assert_eq!(sse_decision(br#"event: response.output_item.added
data: {"type":"response.output_item.added"}"#),SseDecision::Commit);
        assert_eq!(sse_decision(br#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"Selected model is at capacity"}"#),SseDecision::Commit);
    }
    #[test]
    fn preserves_nonstandard_provider_prefix() {
        let u=mapped_url("https://gateway.example/custom/api/v1",&"/v1/responses?test=1".parse::<Uri>().unwrap()).unwrap();
        assert_eq!(u.as_str(),"https://gateway.example/custom/api/v1/responses?test=1");
    }

    #[tokio::test]
    async fn proxy_retries_capacity_without_resending_a_user_turn() {
        use axum::{routing::post, response::IntoResponse};
        use std::sync::atomic::AtomicUsize;
        let calls=Arc::new(AtomicUsize::new(0));
        let counter=calls.clone();
        let upstream=Router::new()
            .route("/compatible/api/responses",post(move |headers:HeaderMap| {
                let tracker=counter.clone();
                async move {
                    assert_eq!(headers.get("authorization").and_then(|x|x.to_str().ok()),Some("Bearer mock"));
                    let count=tracker.fetch_add(1,Ordering::Relaxed);
                    if count==0 {
                        (StatusCode::SERVICE_UNAVAILABLE,Json(serde_json::json!({
                            "error":{"code":"server_overloaded","message":"Selected model is at capacity"}
                        }))).into_response()
                    } else {
                        (StatusCode::OK,Json(serde_json::json!({"id":"completed"}))).into_response()
                    }
                }
            }));
        let upstream_listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port=upstream_listener.local_addr().unwrap().port();
        let upstream_job=tokio::spawn(async move {
            axum::serve(upstream_listener,upstream).await.unwrap();
        });
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port=listener.local_addr().unwrap().port();
        let settings=Settings {
            provider:Some("mock".into()),
            upstream_base_url:Some(format!("http://127.0.0.1:{upstream_port}/compatible/api")),
            local_port:port,retry_enabled:true,enabled:true,max_retries:2,
            base_delay_ms:200,max_delay_ms:1000,
        };
        let metric=Arc::new(Metrics::default());
        let ctx=ProxyContext::new(Arc::new(RwLock::new(settings)),metric.clone()).unwrap();
        let (shutdown,receiver)=oneshot::channel();
        let job=tokio::spawn(run_server(listener,ctx,receiver));
        let reply=reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/responses"))
            .header("Authorization","Bearer mock").body(r#"{"input":"test"}"#)
            .send().await.unwrap();
        assert_eq!(reply.status(),StatusCode::OK);
        assert_eq!(reply.json::<serde_json::Value>().await.unwrap()["id"],"completed");
        assert_eq!(calls.load(Ordering::Relaxed),2);
        assert_eq!(metric.retries.load(Ordering::Relaxed),1);
        assert_eq!(metric.recovered.load(Ordering::Relaxed),1);
        let _=shutdown.send(());
        job.await.unwrap().unwrap();
        upstream_job.abort();
    }

    #[tokio::test]
    async fn proxy_does_not_replay_after_model_output() {
        use axum::{routing::post, response::IntoResponse};
        use std::sync::atomic::AtomicUsize;
        let calls=Arc::new(AtomicUsize::new(0));
        let counter=calls.clone();
        let upstream=Router::new().route("/v1/responses",post(move || {
            let counter=counter.clone();
            async move {
                counter.fetch_add(1,Ordering::Relaxed);
                (StatusCode::OK,[("content-type","text/event-stream")],
                    "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n\
                     event: error\ndata: {\"type\":\"error\",\"error\":{\"code\":\"server_overloaded\"}}\n\n"
                ).into_response()
            }
        }));
        let upstream_listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port=upstream_listener.local_addr().unwrap().port();
        let upstream_job=tokio::spawn(async move {axum::serve(upstream_listener,upstream).await.unwrap();});
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port=listener.local_addr().unwrap().port();
        let settings=Settings {
            provider:Some("mock".into()),
            upstream_base_url:Some(format!("http://127.0.0.1:{upstream_port}/v1")),
            local_port:port,retry_enabled:true,enabled:true,max_retries:3,
            base_delay_ms:200,max_delay_ms:1000,
        };
        let metric=Arc::new(Metrics::default());
        let ctx=ProxyContext::new(Arc::new(RwLock::new(settings)),metric.clone()).unwrap();
        let (shutdown,receiver)=oneshot::channel();
        let job=tokio::spawn(run_server(listener,ctx,receiver));
        let reply=reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/v1/responses"))
            .body("{}").send().await.unwrap();
        let text=reply.text().await.unwrap();
        assert!(text.contains("partial"));
        assert_eq!(calls.load(Ordering::Relaxed),1);
        assert_eq!(metric.retries.load(Ordering::Relaxed),0);
        let _=shutdown.send(());
        job.await.unwrap().unwrap();
        upstream_job.abort();
    }
}
