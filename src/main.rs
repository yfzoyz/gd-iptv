mod auth;
mod config;
mod group;
mod http_client;
mod iptv;
mod logger;
mod m3u;
mod merge;
mod proxy;
mod xmltv;

use auth::{constant_time_eq, TokenStore};
use bytes::Bytes;
use config::{default_config_path, Config};
use http::{header, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use iptv::{get_channels, stream_xmltv, BaseUrlCache};
use log::{debug, error, info, warn};
use m3u::PlaylistOptions;
use serde::{Deserialize, Serialize};
use std::{
    convert::Infallible,
    net::{SocketAddr, SocketAddrV4},
    path::PathBuf,
    pin::Pin,
    str::FromStr,
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::Duration,
};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_API_BODY_BYTES: usize = 64 * 1024;
const INDEX_HTML: &str = include_str!("../static/index.html");

// =========================
// 统一响应体（静态 Full 或流式 mpsc）
// =========================

enum RespBody {
    Full(Full<Bytes>),
    Stream(mpsc::Receiver<Bytes>),
}

impl Body for RespBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            RespBody::Full(inner) => Pin::new(inner).poll_frame(cx),
            RespBody::Stream(rx) => match rx.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => Poll::Ready(Some(Ok(Frame::data(chunk)))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            RespBody::Full(inner) => inner.is_end_stream(),
            RespBody::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            RespBody::Full(inner) => inner.size_hint(),
            RespBody::Stream(_) => SizeHint::default(),
        }
    }
}

fn resp_bytes(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Response<RespBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
        .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
        .body(RespBody::Full(Full::new(body.into())))
        .unwrap()
}

fn resp_text(status: StatusCode, body: impl Into<String>) -> Response<RespBody> {
    resp_bytes(status, "text/plain; charset=utf-8", body.into())
}

fn resp_json<T: Serialize>(status: StatusCode, val: &T) -> Response<RespBody> {
    let data = serde_json::to_vec(val).unwrap_or_else(|_| b"{}".to_vec());
    resp_bytes(status, "application/json; charset=utf-8", data)
}

fn resp_stream(content_type: &str, rx: mpsc::Receiver<Bytes>) -> Response<RespBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
        .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
        .body(RespBody::Stream(rx))
        .unwrap()
}

// =========================
// 应用状态
// =========================

struct AppState {
    config: RwLock<Config>,
    config_path: PathBuf,
    tokens: TokenStore,
    base_url_cache: BaseUrlCache,
}

impl AppState {
    fn cfg_snapshot(&self) -> Config {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

// =========================
// 命令行参数（零依赖）
// =========================

struct CliArgs {
    config: PathBuf,
    bind: Option<String>,
    pidfile: Option<PathBuf>,
    healthcheck: Option<String>,
}

fn print_help() {
    println!(
        "gd-iptv-panel {VERSION} - 广东电信 IPTV 播放列表 + EPG + 内置代理（路由器轻量版）\n\n\
         用法: gd-iptv-panel [选项]\n\n\
         选项:\n  \
           -c, --config <PATH>      配置文件路径 [默认: $IPTV_CONFIG 或 ./config.json]\n  \
           -b, --bind <ADDR>        监听地址（覆盖配置文件，例如 0.0.0.0:4022）\n  \
           -p, --pidfile <PATH>     PID 文件路径（便于 BusyBox 等无 systemd 路由器管理进程）\n  \
               --healthcheck [ADDR] 健康检查（用于 Docker scratch 容器，默认 127.0.0.1:4022）\n  \
           -V, --version            显示版本\n  \
           -h, --help               显示帮助"
    );
}

fn parse_args() -> CliArgs {
    let mut config = default_config_path();
    let mut bind = None;
    let mut pidfile = None;
    let mut healthcheck = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("gd-iptv-panel {VERSION}");
                std::process::exit(0);
            }
            "-c" | "--config" => {
                let Some(val) = args.next() else {
                    eprintln!("参数 {arg} 缺少值");
                    std::process::exit(2);
                };
                config = PathBuf::from(val);
            }
            "-b" | "--bind" => {
                let Some(val) = args.next() else {
                    eprintln!("参数 {arg} 缺少值");
                    std::process::exit(2);
                };
                bind = Some(val);
            }
            "-p" | "--pidfile" => {
                let Some(val) = args.next() else {
                    eprintln!("参数 {arg} 缺少值");
                    std::process::exit(2);
                };
                pidfile = Some(PathBuf::from(val));
            }
            "-d" | "--cache-dir" => {
                // 兼容旧版参数，消费其值并忽略
                let _ = args.next();
            }
            "--healthcheck" => {
                let addr = args.next().unwrap_or_else(|| "127.0.0.1:4022".to_string());
                healthcheck = Some(addr);
            }
            other => {
                eprintln!("未知参数: {other}（运行 --help 查看用法）");
                std::process::exit(2);
            }
        }
    }

    CliArgs {
        config,
        bind,
        pidfile,
        healthcheck,
    }
}

/// 内置极简健康检查（用于无 curl/wget 的 scratch Docker 镜像）
fn run_healthcheck(addr: &str) -> i32 {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let Ok(mut stream) = TcpStream::connect(addr) else {
        return 1;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let req = format!(
        "GET /api/auth-check HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return 1;
    }
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(n) if n > 12 && std::str::from_utf8(&buf[..n]).unwrap_or("").contains("200") => 0,
        _ => 1,
    }
}

// =========================
// 请求辅助
// =========================

fn extract_token(req: &Request<Incoming>) -> &str {
    req.headers()
        .get("X-Auth-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

fn require_auth(state: &AppState, req: &Request<Incoming>) -> bool {
    state.tokens.check(extract_token(req))
}

fn extract_scheme_host(req: &Request<Incoming>, fallback_bind: &str) -> (String, String) {
    let scheme = req
        .headers()
        .get("X-Forwarded-Proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| *s == "http" || *s == "https")
        .unwrap_or("http")
        .to_string();

    let host = req
        .headers()
        .get("X-Forwarded-Host")
        .or_else(|| req.headers().get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback_bind)
        .to_string();

    (scheme, host)
}

async fn read_json_body<T: serde::de::DeserializeOwned>(req: Request<Incoming>) -> Result<T, String> {
    let collected = Limited::new(req.into_body(), MAX_API_BODY_BYTES)
        .collect()
        .await
        .map_err(|_| "请求体过大或读取失败".to_string())?;
    serde_json::from_slice(&collected.to_bytes()).map_err(|e| format!("JSON 解析失败: {e}"))
}

// =========================
// 路由处理
// =========================

#[derive(Deserialize)]
struct LoginPayload {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    ok: bool,
    token: String,
}

async fn handle_request(
    state: Arc<AppState>,
    req: Request<Incoming>,
) -> Result<Response<RespBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_string);

    // 跨域预检处理
    if method == Method::OPTIONS {
        return Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS")
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
            .header("Access-Control-Allow-Private-Network", "true")
            .body(RespBody::Full(Full::new(Bytes::new())))
            .unwrap());
    }

    let cfg = state.cfg_snapshot();

    // 插件关闭拦截：关闭后，阻断所有播放列表与流媒体代理；首页返回友好的停用说明
    if !cfg.enabled {
        let is_state_query = path == "/api/plugin/state"
            || path == "/api/plugin/enable"
            || path == "/favicon.ico";
        if !is_state_query {
            if path == "/" || path == "/index.html" {
                let disabled_html = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>IPTV 插件已停用</title>
<style>
  body{margin:0;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;background:#f5f7fa;display:flex;align-items:center;justify-content:center;min-height:100vh;color:#303133;}
  .card{background:#fff;border-radius:12px;box-shadow:0 6px 20px rgba(0,0,0,0.06);padding:40px 32px;max-width:460px;width:90%;text-align:center;}
  .icon{width:56px;height:56px;border-radius:50%;background:#f4f4f5;color:#909399;display:inline-flex;align-items:center;justify-content:center;margin:0 auto 20px;}
  h2{margin:0 0 12px;font-size:20px;color:#303133;}
  p{margin:0 0 20px;font-size:14px;line-height:1.6;color:#606266;}
  .badge{display:inline-block;padding:4px 12px;background:#fef0f0;color:#f56c6c;border-radius:20px;font-size:13px;font-weight:500;margin-bottom:16px;}
  .footer{font-size:12px;color:#909399;margin-top:24px;border-top:1px solid #ebeef5;padding-top:16px;}
</style>
</head>
<body>
<div class="card">
  <div class="icon">
    <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"></circle><line x1="4.93" y1="4.93" x2="19.07" y2="19.07"></line></svg>
  </div>
  <div class="badge">已在路由器后台关闭</div>
  <h2>IPTV 插件当前处于停用状态</h2>
  <p>您已在路由器管理后台「应用 → 插件 → IPTV插件」关闭了此插件。<br>在此状态下，已自动暂停播放列表、EPG 以及组播/RTSP 串流代理服务，以释放系统内存与带宽。</p>
  <div class="footer">如需使用，请前往路由器管理后台开启开关并点击「提交」。</div>
</div>
</body>
</html>"#;
                return Ok(Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .body(RespBody::Full(Full::new(Bytes::from_static(disabled_html.as_bytes()))))
                    .unwrap());
            }
            return Ok(resp_text(
                StatusCode::SERVICE_UNAVAILABLE,
                "IPTV 插件当前处于关闭状态。如需使用，请前往路由器管理后台「应用 → 插件 → IPTV插件」开启并点击提交。",
            ));
        }
    }

    let resp = match (method, path.as_str()) {
        // API: 插件状态查询 (供路由器管理页 1:1 开关同步状态)
        (Method::GET, "/api/plugin/state") => {
            resp_json(StatusCode::OK, &serde_json::json!({
                "ok": true,
                "enabled": cfg.enabled,
                "version": VERSION
            }))
        }

        // API: 开启插件 (供路由器前端 1:1 开关提交时调用)
        (Method::POST, "/api/plugin/enable") => {
            let mut new_cfg = cfg.clone();
            new_cfg.enabled = true;
            let _ = new_cfg.save(&state.config_path);
            *state.config.write().unwrap_or_else(|e| e.into_inner()) = new_cfg;
            info!("[IPTV-Plugin] IPTV 插件已被路由器前端开启");
            resp_json(StatusCode::OK, &serde_json::json!({ "ok": true, "enabled": true }))
        }

        // API: 关闭插件 (供路由器前端 1:1 开关提交时调用)
        (Method::POST, "/api/plugin/disable") => {
            let mut new_cfg = cfg.clone();
            new_cfg.enabled = false;
            let _ = new_cfg.save(&state.config_path);
            *state.config.write().unwrap_or_else(|e| e.into_inner()) = new_cfg;
            warn!("[IPTV-Plugin] IPTV 插件已被路由器前端关闭，停止所有播放流");
            resp_json(StatusCode::OK, &serde_json::json!({ "ok": true, "enabled": false }))
        }

        // 静态面板
        (Method::GET, "/") => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(RespBody::Full(Full::new(Bytes::from_static(
                INDEX_HTML.as_bytes(),
            ))))
            .unwrap(),

        // API: 登录
        (Method::POST, "/api/login") => {
            if state.tokens.is_rate_limited() {
                return Ok(resp_json(
                    StatusCode::TOO_MANY_REQUESTS,
                    &serde_json::json!({ "ok": false, "error": "登录尝试过于频繁，请 1 分钟后再试" }),
                ));
            }
            let body = match read_json_body::<LoginPayload>(req).await {
                Ok(b) => b,
                Err(e) => {
                    return Ok(resp_json(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "ok": false, "error": e }),
                    ));
                }
            };
            let cfg = state.cfg_snapshot();
            let user_ok = constant_time_eq(body.username.as_bytes(), cfg.admin_user.as_bytes());
            let pass_ok = constant_time_eq(body.password.as_bytes(), cfg.admin_pass.as_bytes());
            if user_ok && pass_ok {
                let token = state.tokens.issue();
                resp_json(StatusCode::OK, &LoginResponse { ok: true, token })
            } else {
                state.tokens.record_failure();
                tokio::time::sleep(Duration::from_millis(300)).await;
                resp_json(
                    StatusCode::UNAUTHORIZED,
                    &serde_json::json!({ "ok": false, "error": "用户名或密码错误" }),
                )
            }
        }

        // API: 登出
        (Method::POST, "/api/logout") => {
            state.tokens.revoke(extract_token(&req));
            resp_json(StatusCode::OK, &serde_json::json!({ "ok": true }))
        }

        // API: Token 校验
        (Method::GET, "/api/auth-check") => {
            let ok = require_auth(&state, &req);
            resp_json(StatusCode::OK, &serde_json::json!({ "ok": ok }))
        }

        // API: 读配置
        (Method::GET, "/api/config") => {
            if !require_auth(&state, &req) {
                return Ok(resp_text(StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            let cfg = state.cfg_snapshot();
            resp_json(StatusCode::OK, &cfg)
        }

        // API: 写配置
        (Method::POST, "/api/config") => {
            if !require_auth(&state, &req) {
                return Ok(resp_text(StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            let current_token = extract_token(&req).to_string();
            let mut cfg = match read_json_body::<Config>(req).await {
                Ok(c) => c,
                Err(e) => {
                    return Ok(resp_json(
                        StatusCode::BAD_REQUEST,
                        &serde_json::json!({ "ok": false, "error": e }),
                    ));
                }
            };
            cfg.sanitize();
            let creds_changed = {
                let old = state.config.read().unwrap_or_else(|e| e.into_inner());
                // bind 只在启动时生效，不允许运行时修改
                cfg.bind = old.bind.clone();
                if cfg.admin_user.is_empty() {
                    cfg.admin_user = old.admin_user.clone();
                }
                if cfg.admin_pass.is_empty() {
                    cfg.admin_pass = old.admin_pass.clone();
                }
                cfg.admin_user != old.admin_user || cfg.admin_pass != old.admin_pass
            };

            if let Err(e) = cfg.save(&state.config_path) {
                return Ok(resp_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &serde_json::json!({ "ok": false, "error": e.to_string() }),
                ));
            }
            *state.config.write().unwrap_or_else(|e| e.into_inner()) = cfg;
            state.base_url_cache.invalidate();
            if creds_changed {
                state.tokens.revoke_others(&current_token);
            }
            resp_json(
                StatusCode::OK,
                &serde_json::json!({ "ok": true, "message": "配置已保存" }),
            )
        }

        // API: 清理图标缓存（兼容保留，图标缓存已在 v0.3.5 彻底精简）
        (Method::POST, "/api/clear-logo-cache") => {
            if !require_auth(&state, &req) {
                return Ok(resp_text(StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            resp_json(StatusCode::OK, &serde_json::json!({ "ok": true, "message": "图标缓存已在当前版本移除，无需清理" }))
        }

        // API: 状态
        (Method::GET, "/api/status") => {
            if !require_auth(&state, &req) {
                return Ok(resp_text(StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            let cfg = state.cfg_snapshot();
            resp_json(
                StatusCode::OK,
                &serde_json::json!({
                    "ok": true,
                    "version": VERSION,
                    "groups": m3u::all_groups(),
                    "config_path": state.config_path.display().to_string(),
                    "bind": cfg.bind,
                }),
            )
        }

        // API: 测试 IPTV 认证并获取频道数
        (Method::POST, "/api/test") => {
            if !require_auth(&state, &req) {
                return Ok(resp_text(StatusCode::UNAUTHORIZED, "Unauthorized"));
            }
            let cfg = state.cfg_snapshot();
            let (scheme, host) = extract_scheme_host(&req, &cfg.bind);
            match get_channels(&cfg, &state.base_url_cache, &scheme, &host).await {
                Ok(ch) => resp_json(
                    StatusCode::OK,
                    &serde_json::json!({
                        "ok": true,
                        "channels": ch.len(),
                        "message": format!("认证成功，共获取到 {} 个频道", ch.len())
                    }),
                ),
                Err(e) => resp_json(
                    StatusCode::OK,
                    &serde_json::json!({
                        "ok": false,
                        "error": e.to_string()
                    }),
                ),
            }
        }

        // M3U 播放列表
        (Method::GET, "/playlist") => {
            let cfg = state.cfg_snapshot();
            if !cfg.output.enable_playlist {
                return Ok(resp_text(StatusCode::NOT_FOUND, "playlist disabled"));
            }
            let (scheme, host) = extract_scheme_host(&req, &cfg.bind);
            match get_channels(&cfg, &state.base_url_cache, &scheme, &host).await {
                Ok(channels) => {
                    let body = m3u::build_playlist(
                        channels,
                        &PlaylistOptions {
                            include_top: cfg.output.enable_top_channels,
                            include_catchup: cfg.proxy.include_catchup,
                            include_xmltv_header: cfg.output.enable_xmltv,
                            playseek_template: &cfg.proxy.playseek_template,
                            scheme: &scheme,
                            host: &host,
                        },
                    );
                    resp_bytes(
                        StatusCode::OK,
                        "application/vnd.apple.mpegurl; charset=utf-8",
                        body,
                    )
                }
                Err(e) => resp_text(StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e}")),
            }
        }

        // 流式 XMLTV EPG
        (Method::GET, "/xmltv") => {
            let cfg = state.cfg_snapshot();
            if !cfg.output.enable_xmltv {
                return Ok(resp_text(StatusCode::NOT_FOUND, "xmltv disabled"));
            }
            let (scheme, host) = extract_scheme_host(&req, &cfg.bind);
            match stream_xmltv(&cfg, &state.base_url_cache, &scheme, &host).await {
                Ok(rx) => resp_stream("text/xml; charset=utf-8", rx),
                Err(e) => resp_text(StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {e}")),
            }
        }

        (Method::GET, p) if p.starts_with("/rtsp/") => {
            let tail = &p["/rtsp/".len()..];
            if tail.is_empty() {
                return Ok(resp_text(StatusCode::BAD_REQUEST, "missing rtsp path"));
            }
            let rtsp_url = match query.as_deref() {
                Some(q) if !q.is_empty() => format!("rtsp://{tail}?{q}"),
                _ => format!("rtsp://{tail}"),
            };
            let iface = state.cfg_snapshot().iptv.interface;
            match proxy::rtsp(rtsp_url, iface).await {
                Ok(rx) => resp_stream("video/mp2t", rx),
                Err(e) => {
                    error!("RTSP 代理启动失败: {}", e);
                    resp_text(StatusCode::BAD_GATEWAY, format!("Error: {e}"))
                }
            }
        }

        (Method::GET, p) if p.starts_with("/rtp/") => {
            let addr_str = &p["/rtp/".len()..];
            let addr = match SocketAddrV4::from_str(addr_str) {
                Ok(a) => a,
                Err(e) => return Ok(resp_text(StatusCode::BAD_REQUEST, format!("Error: {e}"))),
            };
            let iface = state.cfg_snapshot().iptv.interface;
            match proxy::udp(addr, iface.as_deref()) {
                Ok(rx) => resp_stream("video/mp2t", rx),
                Err(e) => {
                    error!("UDP 代理启动失败 ({}): {}", addr, e);
                    resp_text(StatusCode::BAD_GATEWAY, format!("Error: {e}"))
                }
            }
        }

        _ => resp_text(StatusCode::NOT_FOUND, "Not Found"),
    };

    Ok(resp)
}

// =========================
// 入口（单线程 Tokio 运行时，最小化路由器线程与内存占用）
// =========================

fn main() -> std::io::Result<()> {
    let args = parse_args();

    if let Some(addr) = args.healthcheck {
        std::process::exit(run_healthcheck(&addr));
    }

    logger::init();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .thread_keep_alive(Duration::from_secs(10))
        .build()?;

    rt.block_on(async_main(args))
}

struct PidFileGuard(Option<PathBuf>);

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        if let Some(ref p) = self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

async fn async_main(args: CliArgs) -> std::io::Result<()> {
    let config_path = args.config;
    let mut cfg = Config::load(&config_path)
        .map_err(|e| std::io::Error::other(format!("加载配置失败: {e}")))?;

    if let Some(bind) = args.bind {
        cfg.bind = bind;
    }

    let pid_guard = if let Some(ref pf) = args.pidfile {
        if let Some(parent) = pf.parent().filter(|p| !p.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(pf, format!("{}\n", std::process::id())) {
            warn!("写入 PID 文件 {} 失败: {}", pf.display(), e);
        }
        PidFileGuard(Some(pf.clone()))
    } else {
        PidFileGuard(None)
    };
    let _pid_guard = pid_guard;

    let bind = cfg.bind.clone();
    info!("gd-iptv-panel v{}", VERSION);
    info!("配置文件：{}", config_path.display());
    info!("监听地址：{}", bind);

    let listen_addr: SocketAddr = bind
        .parse()
        .map_err(|e| std::io::Error::other(format!("无效监听地址 {bind}: {e}")))?;
    let listener = TcpListener::bind(listen_addr).await?;

    let state = Arc::new(AppState {
        config: RwLock::new(cfg),
        config_path,
        tokens: TokenStore::new(),
        base_url_cache: BaseUrlCache::new(),
    });

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("收到退出信号，正在停止服务");
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("accept 失败: {}", e);
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                let state = Arc::clone(&state);

                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        handle_request(Arc::clone(&state), req)
                    });
                    if let Err(e) = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(15))
                        .keep_alive(true)
                        .serve_connection(io, service)
                        .await
                    {
                        debug!("HTTP 连接关闭 ({}): {}", peer, e);
                    }
                });
            }
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
