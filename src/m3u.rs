use crate::group::{self, NaturalToken};
use crate::iptv::Channel;
use crate::merge;
use std::fmt::Write as _;

/// 一个分组的预计算排序信息。
struct GroupSortInfo {
    group_idx: usize,
    group_name: &'static str,
    is_cctv: bool,
    cctv_key: (u32, u16, String),
    quality: u8,
    natural: Vec<NaturalToken>,
    id: u64,
}

pub struct PlaylistOptions<'a> {
    pub include_top: bool,
    pub include_catchup: bool,
    pub include_xmltv_header: bool,
    pub playseek_template: &'a str,
    pub scheme: &'a str,
    pub host: &'a str,
}

/// 生成 M3U 播放列表。
///
/// 排序优先级：
///   1. 分组（置顶 / 央视 / 广东 / 卫视 / 少儿 / CGTN / 超清4K / 其他）
///   2. 央视组内：按 CCTV 号数字排序（CCTV-1 ~ CCTV-17，CCTV-4K/5+ 插入对应位置）
///   3. 其他组内：画质 + 自然排序
pub fn build_playlist(channels_in: Vec<Channel>, opts: &PlaylistOptions<'_>) -> String {
    let est_cap = channels_in.len().saturating_mul(220).max(1024);

    // 1. 按基准名合并多画质线路
    let groups = merge::group_by_base_name(channels_in);

    // 2. 预计算排序信息
    let mut ranked: Vec<(GroupSortInfo, Vec<Channel>)> = groups
        .into_iter()
        .map(|chs| {
            let name = &chs[0].name;
            let gt = group::group_title(name);
            let is_cctv = gt == "央视";
            let info = GroupSortInfo {
                group_idx: group::group_index(gt),
                group_name: gt,
                is_cctv,
                cctv_key: if is_cctv {
                    extract_cctv_key(name)
                } else {
                    (0, 0, String::new())
                },
                quality: if is_cctv {
                    0
                } else {
                    merge::quality_rank(name)
                },
                natural: if is_cctv {
                    Vec::new()
                } else {
                    group::natural_key(name)
                },
                id: chs[0].id,
            };
            (info, chs)
        })
        .collect();

    // 3. 排序
    ranked.sort_by(|(a, _), (b, _)| {
        a.group_idx.cmp(&b.group_idx).then_with(|| {
            if a.is_cctv {
                a.cctv_key
                    .cmp(&b.cctv_key)
                    .then_with(|| a.id.cmp(&b.id))
            } else {
                a.quality
                    .cmp(&b.quality)
                    .then_with(|| a.natural.cmp(&b.natural))
                    .then_with(|| a.id.cmp(&b.id))
            }
        })
    });

    let mut out = String::with_capacity(est_cap);
    if opts.include_xmltv_header {
        let _ = writeln!(
            out,
            r#"#EXTM3U x-tvg-url="{}://{}/xmltv""#,
            opts.scheme, opts.host
        );
    } else {
        out.push_str("#EXTM3U\n");
    }

    // 4. 置顶副本（修复：忽略未匹配项 None，避免普通变体取消整组置顶）
    if opts.include_top {
        let mut tops: Vec<(&(GroupSortInfo, Vec<Channel>), u8)> = ranked
            .iter()
            .filter_map(|entry| {
                let rank = entry
                    .1
                    .iter()
                    .filter_map(|c| group::match_top(&c.name))
                    .min()?;
                Some((entry, rank))
            })
            .collect();

        tops.sort_by_cached_key(|(entry, rank)| (*rank, group::natural_key(&entry.1[0].name)));

        for (entry, _) in tops {
            write_group(&mut out, &entry.1, "置顶", opts);
        }
    }

    // 5. 正常分组
    for (info, chs) in &ranked {
        write_group(&mut out, chs, info.group_name, opts);
    }

    out
}

/// 央视排序键 `(CCTV号, 变体标志, 兜底名)`
fn extract_cctv_key(name: &str) -> (u32, u16, String) {
    let name_norm = name.replace('＋', "+");
    let nl = name_norm.to_lowercase();

    if nl.contains("cctv4k") || nl.contains("cctv-4k") {
        return (4, 2, "cctv4k".to_string());
    }

    if let Some((n, has_plus)) = merge::parse_cctv_num(&nl) {
        let variant = if has_plus { 1u16 } else { 0u16 };
        let tail = if has_plus { "cctvNplus" } else { "cctvN" };
        return (n, variant, tail.to_string());
    }

    (9999, 999, nl)
}

fn write_group(out: &mut String, chs: &[Channel], group_override: &str, opts: &PlaylistOptions<'_>) {
    if chs.is_empty() {
        return;
    }

    let best = &chs[0];
    let display_name = clean_display_name(&best.name);
    let safe_name = display_name.replace('"', "'");

    let _ = write!(
        out,
        r#"#EXTINF:-1 tvg-id="{}" tvg-name="{}" tvg-chno="{}""#,
        best.id, safe_name, best.id
    );

    if opts.include_catchup && !best.rtsp.is_empty() {
        let sep = if best.rtsp.contains('?') { '&' } else { '?' };
        let _ = write!(
            out,
            r#" catchup="default" catchup-source="{}{}playseek={}""#,
            best.rtsp.replace(',', "%2C"),
            sep,
            opts.playseek_template
        );
    }

    let _ = writeln!(out, r#" group-title="{}",{}"#, group_override, display_name);

    for ch in chs {
        let url = stream_url(ch);
        if !url.is_empty() {
            out.push_str(url);
            out.push('\n');
        }
    }
}

/// 显示名清洗
pub(crate) fn clean_display_name(name: &str) -> String {
    let name_norm = name.replace('＋', "+");
    let nl = name_norm.to_lowercase();

    if nl.contains("cctv4k") || nl.contains("cctv-4k") {
        return "CCTV-4K".to_string();
    }

    if let Some((n, has_plus)) = merge::parse_cctv_num(&nl) {
        let suffix_str = if has_plus { "+" } else { "" };
        for suffix in ["欧洲", "美洲", "香港", "澳门", "台湾"] {
            if name_norm.contains(suffix) {
                return format!("CCTV-{}{}{}", n, suffix_str, suffix);
            }
        }
        return format!("CCTV-{}{}", n, suffix_str);
    }

    // 非 CCTV：去画质 / 版本 / 帧率后缀（保留 4K）
    let mut n = name.replace("4k", "4K");

    for w in [
        "超高清",
        "超清",
        "高清",
        "标清",
        "HD",
        "hd",
        "开机",
        "测试",
        "时移专用",
        "窄色域",
        "1M",
        "2M",
        "4M",
        "8M",
        "25P",
        "25p",
        "50P",
        "50p",
        "30P",
        "30p",
        "60P",
        "60p",
    ] {
        n = n.replace(w, "");
    }

    let stripped = merge::strip_trailing_digits(&n);
    let cleaned = stripped
        .trim()
        .trim_end_matches(['-', '_', ' '])
        .trim()
        .to_string();

    if cleaned.is_empty() {
        name.to_string()
    } else {
        cleaned
    }
}

fn stream_url(ch: &Channel) -> &str {
    if let Some(igmp) = ch.igmp.as_deref().filter(|u| u.starts_with("http")) {
        return igmp;
    }
    ch.rtsp.as_str()
}

pub fn all_groups() -> &'static [&'static str] {
    group::GROUP_ORDER
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_ch(id: u64, name: &str, url: &str) -> Channel {
        Channel {
            id,
            name: name.to_string(),
            rtsp: format!("rtsp://10.0.0.1/{id}"),
            igmp: Some(url.to_string()),
        }
    }

    #[test]
    fn test_clean_display_name() {
        assert_eq!(clean_display_name("广东4K超高清 窄色域 30"), "广东4K");
        assert_eq!(clean_display_name("CCTV-5超清"), "CCTV-5");
        assert_eq!(clean_display_name("CCTV5＋体育高清-测试"), "CCTV-5+");
        assert_eq!(clean_display_name("CCTV4K-25P"), "CCTV-4K");
        assert_eq!(clean_display_name("CCTV-3综艺"), "CCTV-3");
    }

    #[test]
    fn test_playlist_header_and_top() {
        let channels = vec![
            mk_ch(1, "CCTV-11高清", "http://host/rtp/239.0.0.1:1000"),
            mk_ch(2, "CCTV-11戏曲", "http://host/rtp/239.0.0.2:1000"),
            mk_ch(3, "CCTV-1高清", "http://host/rtp/239.0.0.3:1000"),
        ];
        let opts = PlaylistOptions {
            include_top: true,
            include_catchup: false,
            include_xmltv_header: true,
            playseek_template: "",
            scheme: "http",
            host: "192.168.1.1:4022",
        };
        let m3u = build_playlist(channels, &opts);
        assert!(m3u.starts_with(r#"#EXTM3U x-tvg-url="http://192.168.1.1:4022/xmltv""#));
        assert!(!m3u.contains("tvg-logo="));
        assert!(m3u.contains(r#"group-title="置顶",CCTV-11"#));
    }
}
