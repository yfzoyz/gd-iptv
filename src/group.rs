//! 频道分组、置顶和自然排序逻辑（零正则实现）。

use crate::merge;

/// 置顶优先级（越小越靠前，`None` = 不置顶）
pub fn match_top(name: &str) -> Option<u8> {
    if name.contains("广东珠江") {
        return Some(1);
    }
    if name.contains("岭南戏曲") {
        return Some(2);
    }
    if name.contains("大湾区") {
        return Some(3);
    }
    if name.contains("广东卫视") {
        return Some(4);
    }
    let nl = name.to_lowercase();
    if name.contains("广东") && nl.contains("4k") && !name.contains("卫视") {
        return Some(5);
    }
    if matches!(merge::parse_cctv_num(&nl), Some((11, false))) {
        return Some(6);
    }
    None
}

/// 央视特色频道（全部按小写存储）
const CCTV_SPECIAL: &[&str] = &[
    "风云音乐",
    "风云足球",
    "风云剧场",
    "风云",
    "第一剧场",
    "怀旧剧场",
    "世界地理",
    "女性时尚",
    "兵器科技",
    "电视指南",
    "央视精品",
    "央视台球",
    "高尔夫网球",
    "cctv4k",
    "cctv-4k",
    "cctv4欧洲",
    "cctv4美洲",
];

const GD_LOCAL: &[&str] = &[
    "云浮", "肇庆", "汕头", "潮州", "梅州", "河源", "揭阳", "清远", "茂名", "阳江", "湛江", "中山",
    "珠海", "江门", "佛山", "东莞", "韶关", "惠州", "广州", "深圳",
];

const GD_KEYWORDS: &[&str] = &["广东", "珠江", "岭南", "大湾区", "嘉佳卡通", "南方"];

const KIDS: &[&str] = &["少儿", "卡通", "kaku"];

pub const GROUP_ORDER: &[&str] = &[
    "置顶", "央视", "广东", "卫视", "少儿", "CGTN", "超清4K", "其他",
];

/// 根据频道名返回分组标题
pub fn group_title(name: &str) -> &'static str {
    let nl = name.to_lowercase();

    if nl.contains("cgtn") {
        return "CGTN";
    }

    for k in CCTV_SPECIAL {
        if nl.contains(k) && !name.contains("广东") {
            return "央视";
        }
    }

    if matches!(merge::parse_cctv_num(&nl), Some((1..=17, _))) {
        return "央视";
    }

    if name.contains("央视") && !name.contains("风云") && !name.contains("特色") {
        return "央视";
    }

    for k in KIDS {
        if nl.contains(k) {
            return "少儿";
        }
    }

    for k in GD_LOCAL {
        if name.contains(k) {
            return "广东";
        }
    }
    for k in GD_KEYWORDS {
        if name.contains(k) {
            return "广东";
        }
    }

    if name.contains("卫视") {
        return "卫视";
    }

    if nl.contains("4k") || name.contains("超高清") || name.contains("超清") {
        return "超清4K";
    }

    "其他"
}

/// 分组排序索引：按 `GROUP_ORDER` 返回位置，未匹配的排最后
pub fn group_index(title: &str) -> usize {
    GROUP_ORDER
        .iter()
        .position(|&g| g == title)
        .unwrap_or(GROUP_ORDER.len())
}

/// 频道自然排序 key："CCTV2" 排在 "CCTV10" 前面
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum NaturalToken {
    Num(u64),
    Text(String),
}

pub fn natural_key(name: &str) -> Vec<NaturalToken> {
    let mut key = Vec::new();
    let mut chars = name.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_digit() {
            let mut val: u64 = 0;
            while let Some(&d) = chars.peek() {
                if let Some(digit) = d.to_digit(10) {
                    val = val.saturating_mul(10).saturating_add(digit as u64);
                    chars.next();
                } else {
                    break;
                }
            }
            key.push(NaturalToken::Num(val));
        } else {
            let mut text = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    break;
                }
                chars.next();
                if !c.is_whitespace() && !matches!(c, '-' | '+' | '_' | '/') {
                    for lc in c.to_lowercase() {
                        text.push(lc);
                    }
                }
            }
            if !text.is_empty() {
                key.push(NaturalToken::Text(text));
            }
        }
    }

    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_top() {
        assert_eq!(match_top("广东珠江高清"), Some(1));
        assert_eq!(match_top("岭南戏曲"), Some(2));
        assert_eq!(match_top("大湾区卫视"), Some(3));
        assert_eq!(match_top("广东卫视高清"), Some(4));
        assert_eq!(match_top("广东4K超高清"), Some(5));
        assert_eq!(match_top("广东卫视4K超高清"), Some(4));
        assert_eq!(match_top("CCTV-11戏曲"), Some(6));
        assert_eq!(match_top("CCTV-110"), None);
        assert_eq!(match_top("CCTV-1综合"), None);
    }

    #[test]
    fn test_group_title() {
        assert_eq!(group_title("CCTV-1高清"), "央视");
        assert_eq!(group_title("CCTV4K-25P"), "央视");
        assert_eq!(group_title("世界地理"), "央视");
        assert_eq!(group_title("CGTN纪录"), "CGTN");
        assert_eq!(group_title("KAKU少儿"), "少儿");
        assert_eq!(group_title("深圳卫视"), "广东");
        assert_eq!(group_title("湖南卫视"), "卫视");
        assert_eq!(group_title("求索纪录4K"), "超清4K");
    }

    #[test]
    fn test_natural_key_order() {
        assert!(natural_key("CCTV2") < natural_key("CCTV10"));
        assert!(natural_key("广东1") < natural_key("广东12"));
    }
}
