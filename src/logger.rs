//! 零依赖轻量日志器，专为低资源路由器设计。
//!
//! 支持 `RUST_LOG` 环境变量（例如 `info,hyper=warn,retina=warn`），
//! 默认为 `info,hyper=warn,retina=warn`。

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::io::Write;
use std::sync::OnceLock;

struct Rule {
    target: String,
    level: LevelFilter,
}

struct SimpleLogger {
    default_level: LevelFilter,
    rules: Vec<Rule>,
}

static LOGGER: OnceLock<SimpleLogger> = OnceLock::new();

fn parse_level(s: &str) -> Option<LevelFilter> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

fn build_logger(spec: &str) -> (SimpleLogger, LevelFilter) {
    let mut default_level = LevelFilter::Info;
    let mut rules = Vec::new();
    let mut max_level = LevelFilter::Off;

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((target, level)) = part
            .split_once('=')
            .and_then(|(t, l)| parse_level(l).map(|lvl| (t, lvl)))
        {
            if level > max_level {
                max_level = level;
            }
            rules.push(Rule {
                target: target.trim().to_string(),
                level,
            });
        } else if let Some(level) = parse_level(part) {
            default_level = level;
            if level > max_level {
                max_level = level;
            }
        }
    }

    if default_level > max_level {
        max_level = default_level;
    }

    // 较长前缀优先匹配
    rules.sort_by_key(|a| std::cmp::Reverse(a.target.len()));

    (
        SimpleLogger {
            default_level,
            rules,
        },
        max_level,
    )
}

impl SimpleLogger {
    fn level_for(&self, target: &str) -> LevelFilter {
        for r in &self.rules {
            if target == r.target
                || (target.starts_with(&r.target)
                    && target.as_bytes().get(r.target.len()) == Some(&b':'))
            {
                return r.level;
            }
        }
        self.default_level
    }
}

impl Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let lvl = match record.level() {
            Level::Error => "ERR ",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DBG ",
            Level::Trace => "TRC ",
        };
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "[{lvl}] {}: {}", record.target(), record.args());
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

pub fn init() {
    let spec = std::env::var("RUST_LOG")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "info,hyper=warn,retina=warn".to_string());

    let (logger, max_level) = build_logger(&spec);
    let logger = LOGGER.get_or_init(|| logger);
    if log::set_logger(logger).is_ok() {
        log::set_max_level(max_level);
    }
}
