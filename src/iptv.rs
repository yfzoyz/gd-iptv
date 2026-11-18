use crate::config::Config;
use crate::http_client::HttpSession;
use crate::xmltv;
use anyhow::{anyhow, Result};
use bytes::Bytes;
use des::{
    cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyInit},
    TdesEde3,
};
use log::{debug, info, warn};
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use url::Url;

const HTTP_TIMEOUT: Duration = Duration::from_secs(6);
const BASE_URL_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_CHANNEL_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_EPG_BYTES: usize = 1024 * 1024;
const EPG_CONCURRENCY: usize = 6;

// =========================
// base_url 缓存（避免频繁请求 eds.iptv.gd.cn）
// =========================

struct CachedBaseUrl {
    user: String,
    interface: Option<String>,
    base_url: String,
    fetched_at: Instant,
}

#[derive(Default)]
pub struct BaseUrlCache {
    inner: Mutex<Option<CachedBaseUrl>>,
}

impl BaseUrlCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, user: &str, interface: Option<&str>) -> Option<String> {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.as_ref()?;
        if entry.user == user
            && entry.interface.as_deref() == interface
            && entry.fetched_at.elapsed() < BASE_URL_TTL
        {
            Some(entry.base_url.clone())
        } else {
            None
        }
    }

    pub fn set(&self, user: &str, interface: Option<&str>, base_url: String) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(CachedBaseUrl {
            user: user.to_string(),
            interface: interface.map(str::to_string),
            base_url,
            fetched_at: Instant::now(),
        });
    }

    pub fn invalidate(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
}

// =========================
// 数据结构
// =========================

pub(crate) struct Channel {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) rtsp: String,
    pub(crate) igmp: Option<String>,
}

#[derive(Deserialize)]
struct AuthJson {
    epgurl: String,
}

#[derive(Deserialize)]
struct TokenJson {
    #[serde(rename = "EncryToken")]
    encry_token: String,
}

#[derive(Deserialize)]
struct PlaybillList {
    #[serde(rename = "playbillLites", default)]
    list: Vec<Bill>,
}

#[derive(Deserialize)]
struct Bill {
    name: String,
    #[serde(rename = "startTime")]
    start_time: i64,
    #[serde(rename = "endTime")]
    end_time: i64,
}

// =========================
// 认证与基础 URL
// =========================

async fn fetch_base_url(session: &mut HttpSession, cfg: &Config) -> Result<String> {
    let url = Url::parse_with_params(
        "http://eds.iptv.gd.cn:8082/EDS/jsp/AuthenticationURL",
        [
            ("Action", "Login"),
            ("return_type", "1"),
            ("UserID", cfg.iptv.user.as_str()),
        ],
    )?;

    let auth: AuthJson = session.get_json(&url, 64 * 1024).await?;
    let epgurl = Url::parse(auth.epgurl.as_str())?;
    let base_url = format!(
        "{}://{}:{}",
        epgurl.scheme(),
        epgurl.host_str().ok_or_else(|| anyhow!("epgurl 缺少 host"))?,
        epgurl
            .port_or_known_default()
            .ok_or_else(|| anyhow!("epgurl 缺少 port"))?,
    );
    debug!("Got base_url {}", base_url);
    Ok(base_url)
}

async fn get_base_url_cached(
    session: &mut HttpSession,
    cfg: &Config,
    cache: &BaseUrlCache,
) -> Result<String> {
    if let Some(url) = cache.get(&cfg.iptv.user, cfg.iptv.interface.as_deref()) {
        return Ok(url);
    }
    let url = fetch_base_url(session, cfg).await?;
    cache.set(&cfg.iptv.user, cfg.iptv.interface.as_deref(), url.clone());
    Ok(url)
}

fn random_nonce() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::getrandom(&mut b);
    u32::from_ne_bytes(b) % 10_000_000
}

/// 解析 `Authentication.CTCSetConfig('Channel','...');`（零正则）
fn parse_channel_records(body: &str) -> Vec<HashMap<&str, &str>> {
    const PREFIX: &str = "Authentication.CTCSetConfig('Channel','";
    let mut records = Vec::new();
    let mut rest = body;

    while let Some(start) = rest.find(PREFIX) {
        let after_prefix = &rest[start + PREFIX.len()..];
        let Some(end) = after_prefix.find("')") else {
            break;
        };
        let payload = &after_prefix[..end];
        rest = &after_prefix[end + 2..];

        let mut map = HashMap::new();
        for pair in payload.split("\",") {
            if let Some((k, v)) = pair.split_once("=\"") {
                map.insert(k.trim(), v.trim_end_matches('"'));
            }
        }
        if !map.is_empty() {
            records.push(map);
        }
    }

    records
}

async fn authenticate_and_fetch_channels(
    cfg: &Config,
    cache: &BaseUrlCache,
    scheme: &str,
    host: &str,
) -> Result<(HttpSession, String, Vec<Channel>)> {
    cfg.validate()?;
    info!("Obtaining channels");

    let user = cfg.iptv.user.as_str();
    let passwd = cfg.iptv.passwd.as_str();
    let mac = cfg.iptv.mac.as_str();
    let imei = cfg.iptv.imei.as_str();
    let ip = cfg.iptv.address.as_str();

    let mut session = HttpSession::new(cfg.iptv.interface.clone(), HTTP_TIMEOUT);
    let base_url = fetch_base_url(&mut session, cfg).await?;
    cache.set(user, cfg.iptv.interface.as_deref(), base_url.clone());

    // 1. 获取 EncryToken
    let url = Url::parse_with_params(
        &format!("{base_url}/EPG/oauth/v2/authorize"),
        [
            ("response_type", "EncryToken"),
            ("client_id", "smcphone"),
            ("userid", user),
        ],
    )?;
    let token = session.get_json::<TokenJson>(&url, 64 * 1024).await?.encry_token;
    debug!("Got EncryToken");

    // 2. 3DES-ECB-PKCS7 加密 authinfo
    let md5_hex = format!("{:X}", md5::compute(passwd.as_bytes()));
    let enc = ecb::Encryptor::<TdesEde3>::new_from_slice(&md5_hex.as_bytes()[0..24])
        .map_err(|e| anyhow!("3DES init error: {e}"))?;
    let data = format!("{}${token}${user}${imei}${ip}${mac}$$CTC", random_nonce());
    let auth = hex::encode_upper(enc.encrypt_padded_vec_mut::<Pkcs7>(data.as_bytes()));

    // 3. 用 authinfo 换会话 Cookie
    let url = Url::parse_with_params(
        &format!("{base_url}/EPG/oauth/v2/token"),
        [
            ("client_id", "smcphone"),
            ("DeviceType", "deviceType"),
            ("UserID", user),
            ("DeviceVersion", "deviceVersion"),
            ("userdomain", "2"),
            ("datadomain", "3"),
            ("accountType", "1"),
            ("authinfo", auth.as_str()),
            ("grant_type", "EncryToken"),
        ],
    )?;
    let _ = session.get_bytes(&url, 64 * 1024).await?;

    // 4. 拉取频道列表
    let url = Url::parse(&format!("{base_url}/EPG/jsp/getchannellistHWCTC.jsp"))?;
    let res = session.get_text(&url, MAX_CHANNEL_LIST_BYTES).await?;

    // 5. 解析频道字段
    let records = parse_channel_records(&res);
    let rtsp_prefix = format!("{scheme}://{host}/rtsp/");
    let rtp_prefix = format!("{scheme}://{host}/rtp/");

    let mut channels = Vec::with_capacity(records.len());
    for m in records {
        let Some(id) = m.get("ChannelID").and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        let Some(&name) = m.get("ChannelName") else {
            continue;
        };
        let Some(&raw_url) = m.get("ChannelURL") else {
            continue;
        };

        let Some(rtsp_raw) = raw_url.split('|').find(|u| u.starts_with("rtsp")) else {
            continue;
        };
        let igmp_raw = raw_url.split('|').find(|u| u.starts_with("igmp"));

        let fcc_ip = m.get("ChannelFCCIP").copied().unwrap_or("");
        let fcc_port = m.get("ChannelFCCPort").copied().unwrap_or("");
        let channel_fcc = if !fcc_ip.is_empty() && !fcc_port.is_empty() {
            Some(format!("{fcc_ip}:{fcc_port}"))
        } else {
            None
        };
        let fec_port = m.get("ChannelFECPort").copied().filter(|s| !s.is_empty() && *s != "0");

        let rtsp = if cfg.proxy.rtsp_proxy {
            rtsp_raw.replace("rtsp://", &rtsp_prefix)
        } else {
            rtsp_raw.to_string()
        }
        .replace("zoneoffset=0", "zoneoffset=480");

        let igmp = igmp_raw.map(|igmp_url| {
            if cfg.proxy.udp_proxy {
                let mut url = igmp_url.replace("igmp://", &rtp_prefix);
                let fcc = channel_fcc
                    .as_deref()
                    .unwrap_or(cfg.proxy.fcc_server.as_str());
                let mut sep = '?';
                if !fcc.is_empty() {
                    url.push(sep);
                    url.push_str("fcc=");
                    url.push_str(fcc);
                    url.push_str("&fcc-type=");
                    url.push_str(&cfg.proxy.fcc_type);
                    sep = '&';
                }
                if let Some(fec) = fec_port {
                    url.push(sep);
                    url.push_str("fec=");
                    url.push_str(fec);
                }
                url
            } else {
                igmp_url.to_string()
            }
        });

        channels.push(Channel {
            id,
            name: name.to_string(),
            rtsp,
            igmp,
        });
    }

    info!("Got {} channel(s)", channels.len());
    Ok((session, base_url, channels))
}

pub(crate) async fn get_channels(
    cfg: &Config,
    cache: &BaseUrlCache,
    scheme: &str,
    host: &str,
) -> Result<Vec<Channel>> {
    let (_, _, channels) = authenticate_and_fetch_channels(cfg, cache, scheme, host).await?;
    Ok(channels)
}

/// 流式生成 XMLTV：
/// - 认证并拿到频道列表后立即发送 `<tv>` 与全部 `<channel>` 头
/// - 仅启用固定 `EPG_CONCURRENCY` 个 keep-alive worker 并发拉取各频道 EPG
/// - 每拿到一个频道的 EPG 就立即序列化输出并释放内存，避免低内存路由器 OOM
pub(crate) async fn stream_xmltv(
    cfg: &Config,
    cache: &BaseUrlCache,
    scheme: &str,
    host: &str,
) -> Result<mpsc::Receiver<Bytes>> {
    let (session, base_url, channels) =
        authenticate_and_fetch_channels(cfg, cache, scheme, host).await?;

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let begin_str = (now_ms - 86_400_000 * 2).to_string();
    let end_str = (now_ms + 86_400_000 * 5).to_string();

    let (tx, rx) = mpsc::channel::<Bytes>(16);

    // 先构造 XML 头与所有 <channel> 标签
    let mut header = String::with_capacity(channels.len().saturating_mul(64) + 128);
    let ch_refs: Vec<(u64, &str)> = channels.iter().map(|c| (c.id, c.name.as_str())).collect();
    xmltv::write_xmltv_header(&mut header, &ch_refs);

    let channel_ids: Vec<u64> = channels.into_iter().map(|c| c.id).collect();
    let total = channel_ids.len();

    tokio::spawn(async move {
        if tx.send(Bytes::from(header)).await.is_err() {
            return;
        }

        let queue = Arc::new(tokio::sync::Mutex::new(channel_ids.into_iter()));
        let base_url = Arc::new(base_url);
        let begin_str = Arc::new(begin_str);
        let end_str = Arc::new(end_str);

        let workers = EPG_CONCURRENCY.min(total.max(1));
        let mut handles = Vec::with_capacity(workers);

        for _ in 0..workers {
            let mut worker_session = session.fork();
            let queue = Arc::clone(&queue);
            let base_url = Arc::clone(&base_url);
            let begin_str = Arc::clone(&begin_str);
            let end_str = Arc::clone(&end_str);
            let tx = tx.clone();

            handles.push(tokio::spawn(async move {
                loop {
                    if tx.is_closed() {
                        break;
                    }
                    let next_id = {
                        let mut q = queue.lock().await;
                        q.next()
                    };
                    let Some(cid) = next_id else {
                        break;
                    };

                    let cid_str = cid.to_string();
                    let url = match Url::parse_with_params(
                        &format!("{base_url}/EPG/jsp/iptvsnmv3/en/play/ajax/_ajax_getPlaybillList.jsp"),
                        [
                            ("channelId", cid_str.as_str()),
                            ("begin", begin_str.as_str()),
                            ("end", end_str.as_str()),
                        ],
                    ) {
                        Ok(u) => u,
                        Err(e) => {
                            warn!("EPG URL 构造失败 (ch={}): {}", cid, e);
                            continue;
                        }
                    };

                    match worker_session.get_json::<PlaybillList>(&url, MAX_EPG_BYTES).await {
                        Ok(pb) => {
                            if pb.list.is_empty() {
                                continue;
                            }
                            let mut chunk = String::with_capacity(pb.list.len() * 110);
                            for bill in pb.list {
                                xmltv::write_programme(
                                    &mut chunk,
                                    cid,
                                    bill.start_time,
                                    bill.end_time,
                                    &bill.name,
                                );
                            }
                            if !chunk.is_empty() && tx.send(Bytes::from(chunk)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("拉取 EPG 失败 (ch={}): {}", cid, e);
                        }
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        let _ = tx.send(Bytes::from_static(b"</tv>")).await;
        info!("Finished streaming XMLTV for {} channel(s)", total);
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_channel_records() {
        let sample = r#"
            Authentication.CTCSetConfig('Channel','ChannelID="1001",ChannelName="CCTV-1高清",ChannelURL="igmp://239.0.0.1:1000|rtsp://10.0.0.1/ch1?zoneoffset=0",ChannelFCCIP="115.233.40.137",ChannelFCCPort="8027",ChannelFECPort="0"');
            Authentication.CTCSetConfig('Channel','ChannelID="1002",ChannelName="广东珠江",ChannelURL="rtsp://10.0.0.1/ch2"');
        "#;
        let recs = parse_channel_records(sample);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].get("ChannelID"), Some(&"1001"));
        assert_eq!(recs[0].get("ChannelName"), Some(&"CCTV-1高清"));
        assert_eq!(recs[1].get("ChannelID"), Some(&"1002"));
    }
}
