//! RTSP 与 UDP 组播代理（低内存、零依赖 RTP 解析、断连立即回收）。

use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures_util::StreamExt;
#[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
use local_ip_address::list_afinet_netifas;
use log::{debug, error, info};
use retina::client::{PacketItem, Session, SessionOptions};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::ops::Range;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;
use url::Url;

/// 每条代理流的内存队列上限（64 个 UDP/RTP 包 ≈ 85 KB），控制路由器内存占用
const STREAM_CHANNEL_CAP: usize = 64;
const RTSP_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const UDP_RECV_BUF_SIZE: usize = 2 * 1024 * 1024;
const SEQ_WINDOW: u16 = 3000;
const SEQ_RESYNC_THRESHOLD: u8 = 8;

/// RTP 序号乱序过滤（支持 16 位回绕与上游序号重置自动重同步）
struct SeqFilter {
    last: Option<u16>,
    out_of_window: u8,
}

impl SeqFilter {
    fn new() -> Self {
        Self {
            last: None,
            out_of_window: 0,
        }
    }

    fn accept(&mut self, next: u16) -> bool {
        let Some(cur) = self.last else {
            self.last = Some(next);
            self.out_of_window = 0;
            return true;
        };

        let diff = next.wrapping_sub(cur);
        if diff == 0 {
            // 重复包
            return false;
        }
        if diff <= SEQ_WINDOW {
            self.last = Some(next);
            self.out_of_window = 0;
            return true;
        }

        self.out_of_window = self.out_of_window.saturating_add(1);
        if self.out_of_window >= SEQ_RESYNC_THRESHOLD {
            // 连续多个包落在窗口外：上游可能重置了序号，自动重同步
            self.last = Some(next);
            self.out_of_window = 0;
            return true;
        }
        false
    }
}

enum PacketKind {
    /// 原始 MPEG-TS over UDP（直接透传，无 RTP 头）
    RawTs,
    /// RFC 3550 RTP 包：(sequence_number, payload 字节范围)
    Rtp(u16, Range<usize>),
}

/// 零依赖解析 UDP 载荷：支持原始 MPEG-TS (0x47) 与 RFC 3550 RTP v2（含 CSRC / 扩展头 / Padding）
fn parse_udp_packet(pkt: &[u8]) -> Option<PacketKind> {
    if pkt.is_empty() {
        return None;
    }

    // 原始 MPEG-TS over UDP：以 0x47 同步字节开头且长度为 188 的整数倍
    if pkt.len() >= 188 && pkt[0] == 0x47 && (pkt.len() / 188) * 188 == pkt.len() {
        return Some(PacketKind::RawTs);
    }

    // RFC 3550 RTP v2 至少 12 字节
    if pkt.len() < 12 {
        return None;
    }
    let version = pkt[0] >> 6;
    if version != 2 {
        return None;
    }

    // 忽略 RTCP (payload type 200..=204 复用在同一端口时)
    let pt = pkt[1] & 0x7f;
    if (72..=76).contains(&pt) {
        return None;
    }

    let has_padding = (pkt[0] & 0x20) != 0;
    let has_ext = (pkt[0] & 0x10) != 0;
    let csrc_count = (pkt[0] & 0x0f) as usize;
    let seq = u16::from_be_bytes([pkt[2], pkt[3]]);

    let mut start = 12 + csrc_count * 4;
    if start > pkt.len() {
        return None;
    }

    if has_ext {
        if start + 4 > pkt.len() {
            return None;
        }
        let ext_words = u16::from_be_bytes([pkt[start + 2], pkt[start + 3]]) as usize;
        start = start.checked_add(4 + ext_words * 4)?;
        if start > pkt.len() {
            return None;
        }
    }

    let mut end = pkt.len();
    if has_padding {
        let pad_len = *pkt.last()? as usize;
        if pad_len == 0 || pad_len > end.saturating_sub(start) {
            return None;
        }
        end -= pad_len;
    }

    if start >= end {
        return None;
    }

    Some(PacketKind::Rtp(seq, start..end))
}

/// 启动 RTSP 代理流，返回用于 HTTP 流式响应的 Receiver。
pub(crate) async fn rtsp(url: String, if_name: Option<String>) -> Result<mpsc::Receiver<Bytes>> {
    let mut options = SessionOptions::default().follow_redirects(true);

    #[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
    if let Some(ref iface) = if_name {
        let network_interfaces = list_afinet_netifas()?;
        for (name, ip) in network_interfaces.iter() {
            debug!("{}: {}", name, ip);
            if name == iface {
                options = options.bind(ip.to_string());
                break;
            }
        }
    }

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    if let Some(iface) = if_name {
        options = options.bind(iface);
    }

    let parsed = Url::parse(&url)?;
    let mut session = timeout(RTSP_SETUP_TIMEOUT, Session::describe(parsed, options))
        .await
        .map_err(|_| anyhow!("RTSP DESCRIBE 超时"))?
        .map_err(|e| anyhow!("RTSP DESCRIBE 失败: {}", e))?;

    info!("RTSP session created with {} stream(s)", session.streams().len());

    for i in 0..session.streams().len() {
        timeout(RTSP_SETUP_TIMEOUT, session.setup(i, Default::default()))
            .await
            .map_err(|_| anyhow!("RTSP SETUP 超时"))?
            .map_err(|e| anyhow!("RTSP SETUP 失败: {}", e))?;
    }

    let mut playing = timeout(RTSP_SETUP_TIMEOUT, session.play(Default::default()))
        .await
        .map_err(|_| anyhow!("RTSP PLAY 超时"))?
        .map_err(|e| anyhow!("RTSP PLAY 失败: {}", e))?;

    let (tx, rx) = mpsc::channel(STREAM_CHANNEL_CAP);

    tokio::spawn(async move {
        let mut seq_filter = SeqFilter::new();
        loop {
            tokio::select! {
                biased;
                _ = tx.closed() => {
                    debug!("RTSP 下游断连，结束会话");
                    break;
                }
                item = playing.next() => {
                    match item {
                        Some(Ok(PacketItem::Rtp(pkt))) => {
                            let next = pkt.sequence_number();
                            if seq_filter.accept(next)
                                && tx.send(pkt.into_payload_bytes()).await.is_err()
                            {
                                break;
                            }
                        }
                        // 修复：忽略 RTCP 等控制包，不中断视频流
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            error!("RTSP 流错误: {}", e);
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
        info!("RTSP proxy closed");
    });

    Ok(rx)
}

/// 启动 UDP 组播代理流，返回用于 HTTP 流式响应的 Receiver。
pub(crate) fn udp(
    multi_addr: SocketAddrV4,
    if_name: Option<&str>,
) -> Result<mpsc::Receiver<Bytes>> {
    let socket = bind_multicast_socket(multi_addr, if_name)?;
    info!("UDP proxy joined {}", multi_addr);

    let (tx, rx) = mpsc::channel(STREAM_CHANNEL_CAP);

    tokio::spawn(async move {
        let mut seq_filter = SeqFilter::new();
        let mut buf = vec![0u8; 2048];

        loop {
            tokio::select! {
                biased;
                _ = tx.closed() => {
                    debug!("UDP 下游断连，立即退出组播 {}", multi_addr);
                    break;
                }
                res = socket.recv(&mut buf) => {
                    let n = match res {
                        Ok(n) => n,
                        Err(e) => {
                            error!("UDP recv 失败 ({}): {}", multi_addr, e);
                            break;
                        }
                    };
                    match parse_udp_packet(&buf[..n]) {
                        Some(PacketKind::RawTs) => {
                            if tx.send(Bytes::copy_from_slice(&buf[..n])).await.is_err() {
                                break;
                            }
                        }
                        Some(PacketKind::Rtp(seq, range)) => {
                            if seq_filter.accept(seq)
                                && tx.send(Bytes::copy_from_slice(&buf[range])).await.is_err()
                            {
                                break;
                            }
                        }
                        // 忽略偶发非 RTP/RTCP 杂包，不中断播放
                        None => continue,
                    }
                }
            }
        }

        // socket Drop 时内核自动退组并发送 IGMP Leave
        drop(socket);
        info!("UDP proxy left {}", multi_addr);
    });

    Ok(rx)
}

fn bind_multicast_socket(multi_addr: SocketAddrV4, if_name: Option<&str>) -> Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    let _ = sock.set_recv_buffer_size(UDP_RECV_BUF_SIZE);
    sock.set_nonblocking(true)?;

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    {
        if let Some(iface) = if_name {
            if let Err(e) = sock.bind_device(Some(iface.as_bytes())) {
                log::warn!("UDP 绑定网卡 {} 失败（继续默认路由）: {}", iface, e);
            }
        }
        // Linux 绑定到具体组播地址可避免同端口不同组播流的串扰
        sock.bind(&multi_addr.into())?;
        let _ = sock.set_multicast_loop_v4(true);

        if let Some(iface) = if_name {
            let idx = interface_index_linux(iface);
            if idx != 0 {
                sock.join_multicast_v4_n(
                    multi_addr.ip(),
                    &socket2::InterfaceIndexOrAddress::Index(idx),
                )?;
            } else {
                log::warn!("未找到网卡 {} 索引，回退默认网卡加组", iface);
                sock.join_multicast_v4(multi_addr.ip(), &Ipv4Addr::UNSPECIFIED)?;
            }
        } else {
            sock.join_multicast_v4(multi_addr.ip(), &Ipv4Addr::UNSPECIFIED)?;
        }
    }

    #[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
    {
        sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, multi_addr.port()).into())?;
        let _ = sock.set_multicast_loop_v4(true);
        let iface_ip = resolve_iface_ipv4(if_name).unwrap_or(Ipv4Addr::UNSPECIFIED);
        sock.join_multicast_v4(multi_addr.ip(), &iface_ip)?;
    }

    Ok(UdpSocket::from_std(sock.into())?)
}

#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
fn interface_index_linux(iface: &str) -> u32 {
    let Ok(c_name) = std::ffi::CString::new(iface) else {
        return 0;
    };
    unsafe { libc::if_nametoindex(c_name.as_ptr()) }
}

#[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
fn resolve_iface_ipv4(if_name: Option<&str>) -> Option<Ipv4Addr> {
    let iface = if_name?;
    let list = list_afinet_netifas().ok()?;
    for (name, ip) in list {
        if let (true, std::net::IpAddr::V4(v4)) = (name == iface, ip) {
            return Some(v4);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seq_filter_wrap_and_resync() {
        let mut f = SeqFilter::new();
        assert!(f.accept(65534));
        assert!(f.accept(65535));
        assert!(f.accept(0));
        assert!(f.accept(1));
        // 重复与乱序丢弃
        assert!(!f.accept(1));
        assert!(!f.accept(65535));
        // 上游重启序号：连续 8 包后自动重同步
        for i in 0..7 {
            assert!(!f.accept(10000 + i));
        }
        assert!(f.accept(10007));
        assert!(f.accept(10008));
    }

    #[test]
    fn test_parse_rtp_and_raw_ts() {
        // 标准 RTP v2 + 4 字节 payload
        let mut pkt = vec![0x80, 0x21, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 0];
        pkt.extend_from_slice(b"MPEG");
        match parse_udp_packet(&pkt) {
            Some(PacketKind::Rtp(seq, range)) => {
                assert_eq!(seq, 0x0102);
                assert_eq!(&pkt[range], b"MPEG");
            }
            _ => panic!("expected RTP"),
        }

        // 原始 MPEG-TS
        let mut ts = vec![0u8; 188 * 2];
        ts[0] = 0x47;
        ts[188] = 0x47;
        assert!(matches!(parse_udp_packet(&ts), Some(PacketKind::RawTs)));
    }
}
