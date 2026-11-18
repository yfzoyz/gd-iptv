//! 零依赖 XMLTV 序列化与 UTC+8 时间格式化（替代 chrono + xml-rs）。

use std::fmt::Write as _;

const CST_OFFSET_SECS: i64 = 8 * 3600;

/// 将 Unix 毫秒时间戳转换为北京时间（UTC+8）`YYYYMMDDHHMMSS`。
///
/// 采用 Howard Hinnant 的公历天数转换算法，零依赖、无浮点、精确处理闰年。
pub fn format_xmltv_time_cst(unix_millis: i64, out: &mut String) -> bool {
    let secs = unix_millis.div_euclid(1000).checked_add(CST_OFFSET_SECS);
    let Some(secs) = secs else {
        return false;
    };

    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400) as u32;

    let hour = sod / 3600;
    let minute = (sod % 3600) / 60;
    let second = sod % 60;

    // Civil date from days since 1970-01-01
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };

    if !(0..=9999).contains(&year) {
        return false;
    }

    let _ = write!(
        out,
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        year, m, d, hour, minute, second
    );
    true
}

/// 写入经过 XML 文本转义的字符串
pub fn write_xml_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // 过滤非法 XML 1.0 控制字符，避免播放器解析 EPG 报错
            c if (c < '\u{20}' && !matches!(c, '\t' | '\n' | '\r')) => {}
            c => out.push(c),
        }
    }
}

pub fn write_xmltv_header(out: &mut String, channels: &[(u64, &str)]) {
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push_str(
        r#"<tv generator-info-name="gd-iptv-panel" source-info-name="gd-iptv-panel">"#,
    );
    for &(id, name) in channels {
        let _ = write!(out, r#"<channel id="{}"><display-name>"#, id);
        write_xml_escaped(out, name);
        out.push_str("</display-name></channel>");
    }
}

pub fn write_programme(out: &mut String, channel_id: u64, start_ms: i64, stop_ms: i64, title: &str) {
    let mut start_str = String::with_capacity(14);
    let mut stop_str = String::with_capacity(14);
    if !format_xmltv_time_cst(start_ms, &mut start_str)
        || !format_xmltv_time_cst(stop_ms, &mut stop_str)
    {
        return;
    }

    let _ = write!(
        out,
        r#"<programme start="{} +0800" stop="{} +0800" channel="{}"><title lang="chi">"#,
        start_str, stop_str, channel_id
    );
    write_xml_escaped(out, title);
    out.push_str("</title>");
    if !title.is_empty() {
        out.push_str("<desc>");
        write_xml_escaped(out, title);
        out.push_str("</desc>");
    }
    out.push_str("</programme>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_xmltv_time_cst() {
        // 1970-01-01 00:00:00 UTC == 1970-01-01 08:00:00 CST
        let mut s = String::new();
        assert!(format_xmltv_time_cst(0, &mut s));
        assert_eq!(s, "19700101080000");

        // 2024-02-29 16:30:45 UTC (leap day) == 2024-03-01 00:30:45 CST
        s.clear();
        assert!(format_xmltv_time_cst(1_709_224_245_000, &mut s));
        assert_eq!(s, "20240301003045");

        // 2024-02-28 20:00:00 UTC == 2024-02-29 04:00:00 CST
        s.clear();
        assert!(format_xmltv_time_cst(1_709_150_400_000, &mut s));
        assert_eq!(s, "20240229040000");
    }

    #[test]
    fn test_xml_escape() {
        let mut s = String::new();
        write_xml_escaped(&mut s, "新闻 & <测试> \"A\" 'B'\u{0001}");
        assert_eq!(s, "新闻 &amp; &lt;测试&gt; &quot;A&quot; &apos;B&apos;");
    }
}
