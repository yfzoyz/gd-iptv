//! 轻量级 HTTP/1.1 客户端（基于 hyper 1.x）。
//!
//! 专为路由器低资源环境设计：
//! - 复用与服务端相同的 hyper 1.x 协议栈，零 reqwest / cookie_store / publicsuffix / TLS 负担
//! - 支持 keep-alive 连接复用、Cookie 持久化、30x 重定向、响应体上限保护与出站网卡绑定

use anyhow::{anyhow, bail, Result};
use bytes::Bytes;
use http::{header, Method, Request, StatusCode};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::client::conn::http1::{self, SendRequest};
use hyper_util::rt::TokioIo;
#[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
use local_ip_address::list_afinet_netifas;
use log::debug;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{lookup_host, TcpSocket, TcpStream};
use tokio::time::timeout;
use url::Url;

const MAX_REDIRECTS: usize = 5;

pub struct HttpSession {
    interface: Option<String>,
    timeout: Duration,
    cookies: HashMap<String, String>,
    /// 复用单条 keep-alive 连接：(host:port, SendRequest)
    conn: Option<(String, SendRequest<Empty<Bytes>>)>,
}

impl HttpSession {
    pub fn new(interface: Option<String>, timeout: Duration) -> Self {
        Self {
            interface,
            timeout,
            cookies: HashMap::new(),
            conn: None,
        }
    }

    /// 克隆认证后的 Cookie 与网络配置（不共享底层 TCP 连接，供并发 worker 各自 keep-alive）
    pub fn fork(&self) -> Self {
        Self {
            interface: self.interface.clone(),
            timeout: self.timeout,
            cookies: self.cookies.clone(),
            conn: None,
        }
    }

    /// 执行 HTTP GET（带 Cookie、自动重定向、超时与最大响应体限制）
    pub async fn get_bytes(&mut self, initial_url: &Url, max_bytes: usize) -> Result<Bytes> {
        let mut current = initial_url.clone();

        for hop in 0..=MAX_REDIRECTS {
            if current.scheme() != "http" {
                bail!("仅支持 HTTP 协议，收到: {}", current.scheme());
            }

            let host = current
                .host_str()
                .ok_or_else(|| anyhow!("URL 缺少主机名: {}", current))?
                .to_string();
            let port = current.port_or_known_default().unwrap_or(80);
            let authority_key = format!("{}:{}", host, port);
            let host_header = if port == 80 {
                host.clone()
            } else {
                authority_key.clone()
            };

            let path_and_query = match current.query() {
                Some(q) => format!("{}?{}", current.path(), q),
                None => current.path().to_string(),
            };

            let cookie_header = self.build_cookie_header();

            let resp = self
                .send_get_with_retry(
                    &host,
                    port,
                    &authority_key,
                    &host_header,
                    &path_and_query,
                    cookie_header.as_deref(),
                )
                .await?;

            // 更新 Cookie
            for val in resp.headers().get_all(header::SET_COOKIE) {
                let Ok(s) = val.to_str() else { continue };
                let Some(pair) = s.split(';').next() else {
                    continue;
                };
                let Some((k, v)) = pair.split_once('=') else {
                    continue;
                };
                let k = k.trim();
                if !k.is_empty() {
                    self.cookies.insert(k.to_string(), v.trim().to_string());
                }
            }

            let status = resp.status();

            // 处理 30x 重定向
            if matches!(
                status,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                if hop == MAX_REDIRECTS {
                    bail!("重定向次数过多: {}", initial_url);
                }
                let loc = resp
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow!("重定向缺少 Location 头"))?;
                current = current.join(loc)?;
                // 丢弃当前响应体以便连接可复用
                let _ = timeout(self.timeout, Limited::new(resp.into_body(), 64 * 1024).collect())
                    .await;
                continue;
            }

            if !status.is_success() {
                let _ = timeout(
                    Duration::from_secs(2),
                    Limited::new(resp.into_body(), 16 * 1024).collect(),
                )
                .await;
                bail!("HTTP {} ({})", status.as_u16(), current);
            }

            let body = timeout(
                self.timeout,
                Limited::new(resp.into_body(), max_bytes).collect(),
            )
            .await
            .map_err(|_| anyhow!("读取响应超时: {}", current))?
            .map_err(|e| anyhow!("读取响应失败 ({}): {}", current, e))?;

            return Ok(body.to_bytes());
        }

        bail!("重定向异常结束")
    }

    pub async fn get_text(&mut self, url: &Url, max_bytes: usize) -> Result<String> {
        let bytes = self.get_bytes(url, max_bytes).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &mut self,
        url: &Url,
        max_bytes: usize,
    ) -> Result<T> {
        let bytes = self.get_bytes(url, max_bytes).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn build_cookie_header(&self) -> Option<String> {
        if self.cookies.is_empty() {
            return None;
        }
        let mut s = String::new();
        for (i, (k, v)) in self.cookies.iter().enumerate() {
            if i > 0 {
                s.push_str("; ");
            }
            s.push_str(k);
            s.push('=');
            s.push_str(v);
        }
        Some(s)
    }

    async fn send_get_with_retry(
        &mut self,
        host: &str,
        port: u16,
        authority_key: &str,
        host_header: &str,
        path_and_query: &str,
        cookie_header: Option<&str>,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        // 先尝试复用已有 keep-alive 连接
        if let Some((ref cached_key, ref mut sender)) = self.conn {
            if cached_key == authority_key && sender.ready().await.is_ok() {
                let req = build_get_request(host_header, path_and_query, cookie_header)?;
                match timeout(self.timeout, sender.send_request(req)).await {
                    Ok(Ok(resp)) => return Ok(resp),
                    _ => {
                        // 对端可能已关闭空闲连接，丢弃后重连
                        self.conn = None;
                    }
                }
            } else {
                self.conn = None;
            }
        }

        // 建立新连接
        let stream = connect_tcp(host, port, self.interface.as_deref(), self.timeout).await?;
        let io = TokioIo::new(stream);
        let (mut sender, conn) = timeout(self.timeout, http1::handshake(io))
            .await
            .map_err(|_| anyhow!("HTTP 握手超时: {}:{}", host, port))?
            .map_err(|e| anyhow!("HTTP 握手失败 ({}:{}): {}", host, port, e))?;

        tokio::spawn(async move {
            if let Err(e) = conn.await {
                debug!("HTTP 客户端连接关闭: {}", e);
            }
        });

        let req = build_get_request(host_header, path_and_query, cookie_header)?;
        let resp = timeout(self.timeout, sender.send_request(req))
            .await
            .map_err(|_| anyhow!("HTTP 请求超时: {}:{}{}", host, port, path_and_query))?
            .map_err(|e| anyhow!("HTTP 请求失败: {}", e))?;

        self.conn = Some((authority_key.to_string(), sender));
        Ok(resp)
    }
}

fn build_get_request(
    host_header: &str,
    path_and_query: &str,
    cookie_header: Option<&str>,
) -> Result<Request<Empty<Bytes>>> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(path_and_query)
        .header(header::HOST, host_header)
        .header(header::CONNECTION, "keep-alive");

    if let Some(cookie) = cookie_header {
        builder = builder.header(header::COOKIE, cookie);
    }

    Ok(builder.body(Empty::new())?)
}

async fn connect_tcp(
    host: &str,
    port: u16,
    if_name: Option<&str>,
    req_timeout: Duration,
) -> Result<TcpStream> {
    let addrs: Vec<SocketAddr> = timeout(req_timeout, lookup_host((host, port)))
        .await
        .map_err(|_| anyhow!("DNS 解析超时: {}:{}", host, port))??
        .collect();

    if addrs.is_empty() {
        bail!("无法解析主机: {}:{}", host, port);
    }

    let mut last_err = None;
    for addr in addrs {
        let res = timeout(req_timeout, connect_one(addr, if_name)).await;
        match res {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(e)) => last_err = Some(e),
            Err(_) => last_err = Some(anyhow!("连接超时: {}", addr)),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("无法连接 {}:{}", host, port)))
}

async fn connect_one(addr: SocketAddr, if_name: Option<&str>) -> Result<TcpStream> {
    let sock = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };

    if let Some(iface) = if_name {
        bind_tcp_interface(&sock, addr, iface)?;
    }

    Ok(sock.connect(addr).await?)
}

#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
fn bind_tcp_interface(sock: &TcpSocket, _addr: SocketAddr, iface: &str) -> Result<()> {
    if let Err(e) = sock.bind_device(Some(iface.as_bytes())) {
        log::warn!("TCP 绑定网卡 {} 失败（继续默认路由）: {}", iface, e);
    }
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
fn bind_tcp_interface(sock: &TcpSocket, addr: SocketAddr, iface: &str) -> Result<()> {
    let network_interfaces = list_afinet_netifas()?;
    for (name, ip) in network_interfaces.iter() {
        if name == iface && ip.is_ipv4() == addr.is_ipv4() {
            debug!("TCP 绑定本地地址 {}: {}", name, ip);
            sock.bind(SocketAddr::new(*ip, 0))?;
            break;
        }
    }
    Ok(())
}
