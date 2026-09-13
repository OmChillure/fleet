//! Live Ollama library catalog for the runner install picker.
//! We do not pin third-party versions in-repo. Fetch fails → empty list.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

const LIBRARY_URL: &str = "https://ollama.com/library?sort=popular";
const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const TOP_N: usize = 50;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalModel {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalModelCatalog {
    pub models: Vec<LocalModel>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalModelTag {
    pub name: String,
    pub tag: String,
    pub spec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_bytes: Option<u64>,
    #[serde(default)]
    pub latest: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalModelTags {
    pub name: String,
    pub tags: Vec<LocalModelTag>,
    pub source: String,
}

struct Cache {
    at: Instant,
    models: Vec<LocalModel>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| {
            let v = v.trim().to_lowercase();
            !(v == "0" || v == "false" || v == "no" || v == "off")
        })
        .unwrap_or(default)
}

/// Public library fetch is only for an enabled local-model runtime.
/// `OLLAMA_ENABLED` defaults on (chat). Lab `ENABLE_OLLAMA` defaults off.
/// Both off → empty + "unavailable", no sockets (not ollama.com, not :1234).
fn local_models_enabled() -> bool {
    env_flag("OLLAMA_ENABLED", true) || env_flag("ENABLE_OLLAMA", false)
}

fn unavailable_catalog() -> LocalModelCatalog {
    LocalModelCatalog {
        models: Vec::new(),
        source: "unavailable".into(),
    }
}

pub async fn catalog() -> LocalModelCatalog {
    if !local_models_enabled() {
        return unavailable_catalog();
    }
    if let Ok(g) = CACHE.lock() {
        if let Some(c) = g.as_ref() {
            if c.at.elapsed() < CACHE_TTL {
                return LocalModelCatalog {
                    models: c.models.clone(),
                    source: "cache".into(),
                };
            }
        }
    }
    match fetch_library().await {
        Ok(models) if models.len() >= 5 => {
            if let Ok(mut g) = CACHE.lock() {
                *g = Some(Cache {
                    at: Instant::now(),
                    models: models.clone(),
                });
            }
            LocalModelCatalog {
                models,
                source: "ollama.com/library".into(),
            }
        }
        Ok(_) | Err(_) => unavailable_catalog(),
    }
}

async fn fetch_library() -> Result<Vec<LocalModel>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("agent-fleet/local-models")
        .build()
        .map_err(|e| e.to_string())?;
    let html = client
        .get(LIBRARY_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_library_html(&html))
}

pub fn library_slug_ok(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.len() <= 128
        && !n.contains('/')
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

static TAGS_CACHE: Mutex<Option<std::collections::HashMap<String, CacheTags>>> = Mutex::new(None);

struct CacheTags {
    at: Instant,
    tags: Vec<LocalModelTag>,
}

pub async fn tags_for(name: &str) -> LocalModelTags {
    let name = name.trim().to_string();
    if !library_slug_ok(&name) || !local_models_enabled() {
        return LocalModelTags {
            name,
            tags: Vec::new(),
            source: "unavailable".into(),
        };
    }
    if let Ok(g) = TAGS_CACHE.lock() {
        if let Some(map) = g.as_ref() {
            if let Some(c) = map.get(&name) {
                if c.at.elapsed() < CACHE_TTL {
                    return LocalModelTags {
                        name,
                        tags: c.tags.clone(),
                        source: "cache".into(),
                    };
                }
            }
        }
    }
    match fetch_tags(&name).await {
        Ok(tags) if !tags.is_empty() => {
            if let Ok(mut g) = TAGS_CACHE.lock() {
                g.get_or_insert_with(std::collections::HashMap::new).insert(
                    name.clone(),
                    CacheTags {
                        at: Instant::now(),
                        tags: tags.clone(),
                    },
                );
            }
            LocalModelTags {
                source: format!("ollama.com/library/{name}/tags"),
                name,
                tags,
            }
        }
        Ok(_) | Err(_) => LocalModelTags {
            name,
            tags: Vec::new(),
            source: "unavailable".into(),
        },
    }
}

async fn fetch_tags(name: &str) -> Result<Vec<LocalModelTag>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("agent-fleet/local-models")
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("https://ollama.com/library/{name}/tags");
    let html = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok(parse_tags_html(name, &html))
}

/// Parse `ollama.com/library/{name}/tags` cards. Order is the page order.
pub fn parse_tags_html(name: &str, html: &str) -> Vec<LocalModelTag> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let prefix = format!("href=\"/library/{name}:");
    let bytes = html.as_bytes();
    let needle = prefix.as_bytes();
    let mut i = 0;
    while let Some(rel) = find_sub(bytes, needle, i) {
        let start = rel + needle.len();
        let Some(end) = bytes[start..].iter().position(|&c| c == b'"') else {
            break;
        };
        let tag = html[start..start + end].trim();
        i = start + end + 1;
        if tag.is_empty() || tag.contains('/') || !seen.insert(tag.to_string()) {
            continue;
        }
        let window = &html[start + end..html.len().min(start + end + 700)];
        let size_bytes = parse_size_token(window);
        let ram_bytes = parse_ram_token(window);
        let latest = window.contains("latest</span>") || tag == "latest";
        out.push(LocalModelTag {
            name: name.to_string(),
            tag: tag.to_string(),
            spec: format!("{name}:{tag}"),
            size_bytes,
            ram_bytes,
            latest,
        });
    }
    out
}

fn parse_size_token(window: &str) -> Option<u64> {
    // " • 2.0GB • " — first size after the digest is the download size.
    let re = regex_lite_size(window)?;
    Some(re)
}

fn parse_ram_token(window: &str) -> Option<u64> {
    let lower = window.to_ascii_lowercase();
    let idx = lower.find(" ram")?;
    let before = &window[..idx];
    let tok = before
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .find(|s| !s.is_empty())?;
    parse_si_size(tok)
}

fn regex_lite_size(window: &str) -> Option<u64> {
    let bytes = window.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let num = &window[start..i];
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let rest = &window[i..];
            for (unit, _) in [("TB", 0), ("GB", 0), ("MB", 0), ("KB", 0)] {
                if rest.len() >= unit.len() && rest[..unit.len()].eq_ignore_ascii_case(unit) {
                    return parse_si_size(&format!("{num}{unit}"));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

pub fn parse_si_size(raw: &str) -> Option<u64> {
    let s = raw.trim().replace(' ', "");
    let (n, mul) = if let Some(n) = s.strip_suffix("TB").or_else(|| s.strip_suffix("tb")) {
        (n, 1_000_000_000_000u64)
    } else if let Some(n) = s.strip_suffix("GB").or_else(|| s.strip_suffix("gb")) {
        (n, 1_000_000_000)
    } else if let Some(n) = s.strip_suffix("MB").or_else(|| s.strip_suffix("mb")) {
        (n, 1_000_000)
    } else if let Some(n) = s.strip_suffix("KB").or_else(|| s.strip_suffix("kb")) {
        (n, 1_000)
    } else {
        return None;
    };
    let v: f64 = n.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    Some((v * mul as f64) as u64)
}

/// Parse `ollama.com/library?sort=popular` cards. Order is the page order.
pub fn parse_library_html(html: &str) -> Vec<LocalModel> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let bytes = html.as_bytes();
    let needle = b"href=\"/library/";
    let mut i = 0;
    while let Some(rel) = find_sub(bytes, needle, i) {
        let start = rel + needle.len();
        let Some(end) = bytes[start..].iter().position(|&c| c == b'"') else {
            break;
        };
        let slug = html[start..start + end].trim();
        i = start + end + 1;
        if slug.is_empty() || slug.contains('/') || !seen.insert(slug.to_string()) {
            continue;
        }
        let window = &html[start + end..html.len().min(start + end + 900)];
        let summary = extract_summary(window);
        out.push(LocalModel {
            name: slug.to_string(),
            summary,
        });
        if out.len() >= TOP_N {
            break;
        }
    }
    out
}

fn find_sub(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

fn extract_summary(window: &str) -> Option<String> {
    let open = window.find("<p ")?;
    let gt = window[open..].find('>')?;
    let body_at = open + gt + 1;
    let close = window[body_at..].find("</p>")?;
    let raw = window[body_at..body_at + close].trim();
    let text = decode_entities(&strip_tags(raw));
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut skip = false;
    for c in s.chars() {
        match c {
            '<' => skip = true,
            '>' => skip = false,
            _ if !skip => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    #[test]
    fn parse_library_keeps_popular_order_and_decoded_summary() {
        let html = r#"
          <a href="/library/llama3.1" class="x"><h2>llama3.1</h2>
            <p class="d">Meta&#39;s Llama 3.1 from Meta.</p></a>
          <a href="/library/deepseek-r1"><p class="d">Reasoning.</p></a>
        "#;
        let models = parse_library_html(html);
        assert_eq!(
            models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["llama3.1", "deepseek-r1"]
        );
        assert_eq!(
            models[0].summary.as_deref(),
            Some("Meta's Llama 3.1 from Meta.")
        );
    }

    #[test]
    fn parse_library_caps_at_top_n_50() {
        let html: String = (1..=51)
            .map(|n| format!(r#"<a href="/library/m{n}"></a>"#))
            .collect();
        let models = parse_library_html(&html);
        assert_eq!(models.len(), 50);
        assert_eq!(models.last().map(|m| m.name.as_str()), Some("m50"));
    }

    #[test]
    fn parse_library_empty_href_slug_skipped() {
        let html = r#"
          <a href="/library/"><h2>empty</h2></a>
          <a href="/library/llama3.1"><h2>llama3.1</h2></a>
        "#;
        let models = parse_library_html(html);
        assert_eq!(
            models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["llama3.1"]
        );
    }

    #[test]
    fn parse_library_summary_none_without_p() {
        let html = r#"<a href="/library/llama3.1"><h2>llama3.1</h2></a>"#;
        let models = parse_library_html(html);
        assert_eq!(models[0].summary, None);
    }

    #[test]
    fn parse_tags_reads_size_and_latest_badge() {
        let html = r#"
          <a href="/library/llama3.2:3b">llama3.2:3b
            <span class="border">latest</span></a>
        "#;
        let tags = parse_tags_html("llama3.2", html);
        assert!(tags[0].latest);
    }

    #[test]
    fn parse_tags_href_tag_latest_is_latest_without_span() {
        let html = r#"<a href="/library/m:latest">m:latest</a>"#;
        let tags = parse_tags_html("m", html);
        assert!(tags[0].latest);
    }

    #[test]
    fn parse_tags_skips_slash_in_tag() {
        let html = r#"
          <a href="/library/llama3.2:foo/bar">skip</a>
          <a href="/library/llama3.2:3b">ok</a>
        "#;
        let tags = parse_tags_html("llama3.2", html);
        assert_eq!(
            tags.iter().map(|t| t.spec.as_str()).collect::<Vec<_>>(),
            vec!["llama3.2:3b"]
        );
    }

    #[test]
    fn parse_si_size_tb_mb_kb_rejects_bare_and_negative() {
        assert_eq!(parse_si_size("1TB"), Some(1_000_000_000_000));
        assert_eq!(parse_si_size("1.5MB"), Some(1_500_000));
        assert_eq!(parse_si_size("500KB"), Some(500_000));
        assert_eq!(parse_si_size("128K"), None);
        assert_eq!(parse_si_size("-1GB"), None);
    }

    #[test]
    fn slug_rejects_paths() {
        let ok_128 = "x".repeat(128);
        let oversize = "x".repeat(129);
        for (slug, want) in [
            ("llama3.2", true),
            ("deepseek-r1", true),
            (ok_128.as_str(), true),
            (" llama3.2 ", true),
            ("llama3.2/tags", false),
            ("hi;rm", false),
            ("../etc", false),
            ("", false),
            (" ", false),
            (oversize.as_str(), false),
        ] {
            assert_eq!(library_slug_ok(slug), want, "library_slug_ok({slug:?})");
        }
    }

    /// OLLAMA_ENABLED=1 still fail-closes unsafe slugs (empty + unavailable)
    /// without opening :443 or :1234. HTTP 400 never reaches tags_for.
    #[tokio::test]
    async fn tags_for_unsafe_slug_unavailable_without_network() {
        let _env = EnvRestore::set(&[
            ("OLLAMA_ENABLED", "1"),
            ("OLLAMA_BASE_URL", "http://127.0.0.1:1234/v1"),
        ])
        .await;
        reset_caches();
        let guard = NoOutbound::arm();

        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let started = Instant::now();
        for slug in ["llama3.2/tags", "hi;rm"] {
            let tags = tokio::time::timeout(FAIL_CLOSED, tags_for(slug))
                .await
                .unwrap_or_else(|_| {
                    panic!("tags_for({slug:?}) must return immediately, not open :443/:1234")
                });
            assert_eq!(tags.name, slug);
            assert!(
                tags.tags.is_empty(),
                "unsafe slug tags_for({slug:?}) must be empty, got {} tags from {}",
                tags.tags.len(),
                tags.source
            );
            assert_eq!(
                tags.source, "unavailable",
                "unsafe slug tags_for({slug:?}) source must be unavailable, got {}",
                tags.source
            );
        }
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "unsafe slug tags_for must fail closed without a live fetch, took {:?}",
            started.elapsed()
        );
        assert!(
            caches_are_empty(),
            "unsafe slug tags_for must not populate the live fetch cache"
        );
        guard.assert_idle();
    }

    fn reset_caches() {
        if let Ok(mut g) = CACHE.lock() {
            *g = None;
        }
        if let Ok(mut g) = TAGS_CACHE.lock() {
            *g = None;
        }
    }

    fn caches_are_empty() -> bool {
        let cat = CACHE.lock().ok().is_some_and(|g| g.is_none());
        let tags = TAGS_CACHE.lock().ok().is_some_and(|g| match g.as_ref() {
            None => true,
            Some(m) => m.is_empty(),
        });
        cat && tags
    }

    /// Serializes OLLAMA_* / ENABLE_* mutation so parallel tests do not race.
    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    struct EnvRestore {
        prev: Vec<(&'static str, Option<String>)>,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl EnvRestore {
        async fn set(pairs: &[(&'static str, &str)]) -> Self {
            let lock = env_lock().lock().await;
            let mut prev = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                prev.push((*k, std::env::var(k).ok()));
                // SAFETY: held under env_lock; Drop restores prior values.
                unsafe { std::env::set_var(k, v) };
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (k, v) in self.prev.drain(..) {
                match v {
                    Some(val) => unsafe { std::env::set_var(k, val) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    fn this_process_socket_inodes() -> std::collections::HashSet<u64> {
        let mut out = std::collections::HashSet::new();
        let Ok(dir) = std::fs::read_dir("/proc/self/fd") else {
            return out;
        };
        for ent in dir.flatten() {
            let Ok(target) = std::fs::read_link(ent.path()) else {
                continue;
            };
            let s = target.to_string_lossy();
            let Some(n) = s.strip_prefix("socket:[") else {
                continue;
            };
            let Some(n) = n.strip_suffix(']') else {
                continue;
            };
            if let Ok(ino) = n.parse() {
                out.insert(ino);
            }
        }
        out
    }

    /// Rows in this netns with remote TCP `port` (any state, including TIME_WAIT).
    /// Key is local-rem-inode so a new connect is visible even after the fd closes.
    fn tcp_rows_to_port(port: u16) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for path in ["/proc/self/net/tcp", "/proc/self/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 10 {
                    continue;
                }
                let Some((_, rem_port)) = cols[2].rsplit_once(':') else {
                    continue;
                };
                let Ok(p) = u16::from_str_radix(rem_port, 16) else {
                    continue;
                };
                if p == port {
                    out.insert(format!("{}-{}-{}", cols[1], cols[2], cols[9]));
                }
            }
        }
        out
    }

    /// Remote TCP ports this process currently has an fd for (not LISTEN).
    fn this_process_remote_tcp_ports() -> std::collections::HashSet<u16> {
        let mut out = std::collections::HashSet::new();
        let mine = this_process_socket_inodes();
        if mine.is_empty() {
            return out;
        }
        for path in ["/proc/self/net/tcp", "/proc/self/net/tcp6"] {
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            for line in text.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 10 {
                    continue;
                }
                let inode: u64 = match cols[9].parse() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                if !mine.contains(&inode) {
                    continue;
                }
                let Some((_, rem_port)) = cols[2].rsplit_once(':') else {
                    continue;
                };
                let Ok(port) = u16::from_str_radix(rem_port, 16) else {
                    continue;
                };
                if port != 0 {
                    out.insert(port);
                }
            }
        }
        out
    }

    /// ENABLE off must not open :1234 (lab OpenAI-compat) and must not fetch.
    /// Bind when the port is free (connect → accept is a fail). When the lab
    /// already owns :1234, spin-poll this process + new netns rows instead.
    /// No pre-existing listener is required.
    struct NoOutbound {
        hits: std::sync::Arc<std::sync::atomic::AtomicU16>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        before_1234: std::collections::HashSet<String>,
        joins: Vec<std::thread::JoinHandle<()>>,
    }

    impl NoOutbound {
        fn arm() -> Self {
            use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
            use std::sync::Arc;

            let before_1234 = tcp_rows_to_port(1234);
            let hits = Arc::new(AtomicU16::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let mut joins = Vec::new();
            let mut bound = 0usize;

            for addr in ["127.0.0.1:1234", "[::1]:1234"] {
                let Ok(listener) = std::net::TcpListener::bind(addr) else {
                    continue;
                };
                bound += 1;
                let _ = listener.set_nonblocking(true);
                let hits = hits.clone();
                let stop = stop.clone();
                joins.push(std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok(_) => hits.store(1234, Ordering::SeqCst),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::yield_now();
                            }
                            Err(_) => std::thread::yield_now(),
                        }
                    }
                }));
            }

            let proc_ok = std::path::Path::new("/proc/self/net/tcp").is_file();
            assert!(
                bound > 0 || proc_ok,
                "need a :1234 bind or /proc to prove ENABLE-off does not connect"
            );

            let hits_p = hits.clone();
            let stop_p = stop.clone();
            let before_p = before_1234.clone();
            joins.push(std::thread::spawn(move || {
                while !stop_p.load(Ordering::Relaxed) {
                    let ports = this_process_remote_tcp_ports();
                    // Lab OpenAI-compat, or HTTPS fetch (ollama.com) while disabled.
                    for p in [1234_u16, 443] {
                        if ports.contains(&p) {
                            hits_p.store(p, Ordering::SeqCst);
                        }
                    }
                    if tcp_rows_to_port(1234)
                        .difference(&before_p)
                        .next()
                        .is_some()
                    {
                        hits_p.store(1234, Ordering::SeqCst);
                    }
                    std::thread::yield_now();
                }
            }));

            Self {
                hits,
                stop,
                before_1234,
                joins,
            }
        }

        fn assert_idle(&self) {
            let port = self.hits.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                port, 0,
                "ENABLE off must not open the network (connect to :{port} is a fail)"
            );
            let new_1234: Vec<_> = tcp_rows_to_port(1234)
                .difference(&self.before_1234)
                .cloned()
                .collect();
            assert!(
                new_1234.is_empty(),
                "ENABLE off must not open :1234 (new tcp rows {new_1234:?})"
            );
        }
    }

    impl Drop for NoOutbound {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            for h in self.joins.drain(..) {
                let _ = h.join();
            }
        }
    }

    /// Both Ollama flags off: catalog()/tags_for() fail closed (empty +
    /// unavailable) without opening ollama.com or the lab :1234 bait.
    #[tokio::test]
    async fn disabled_does_not_proxy_to_lmstudio_1234() {
        // ENABLE off. Bait OLLAMA_BASE_URL so a rewrite that GETs
        // http://127.0.0.1:1234/v1/models (format!/env) is a fail.
        let _env = EnvRestore::set(&[
            ("ENABLE_OLLAMA", "0"),
            ("OLLAMA_ENABLED", "0"),
            ("OLLAMA_BASE_URL", "http://127.0.0.1:1234/v1"),
        ])
        .await;
        reset_caches();
        let guard = NoOutbound::arm();

        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let cat = tokio::time::timeout(FAIL_CLOSED, catalog())
            .await
            .expect("disabled catalog() must return immediately, not open :1234");
        let tags = tokio::time::timeout(FAIL_CLOSED, tags_for("llama3.2"))
            .await
            .expect("disabled tags_for() must return immediately, not open :1234");

        assert!(
            cat.models.is_empty(),
            "disabled catalog must be empty, got {} models from {}",
            cat.models.len(),
            cat.source
        );
        assert_eq!(
            cat.source, "unavailable",
            "disabled catalog source must be unavailable, not a proxy: {}",
            cat.source
        );
        assert_eq!(tags.name, "llama3.2");
        assert!(
            tags.tags.is_empty(),
            "disabled tags_for must be empty, got {} tags from {}",
            tags.tags.len(),
            tags.source
        );
        assert_eq!(
            tags.source, "unavailable",
            "disabled tags_for source must be unavailable, not a proxy: {}",
            tags.source
        );

        guard.assert_idle();
    }

    /// Warm CACHE must not leak through when both Ollama flags are off.
    #[tokio::test]
    async fn disabled_catalog_ignores_warm_cache() {
        let _env = EnvRestore::set(&[("ENABLE_OLLAMA", "0"), ("OLLAMA_ENABLED", "0")]).await;
        if let Ok(mut g) = CACHE.lock() {
            *g = Some(Cache {
                at: Instant::now(),
                models: vec![LocalModel {
                    name: "llama3.2".into(),
                    summary: Some("warm".into()),
                }],
            });
        }

        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let cat = tokio::time::timeout(FAIL_CLOSED, catalog())
            .await
            .expect("disabled catalog() must return immediately, not read through cache into a fetch");
        assert!(
            cat.models.is_empty(),
            "disabled catalog must ignore warm cache, got {} models from {}",
            cat.models.len(),
            cat.source
        );
        assert_eq!(
            cat.source, "unavailable",
            "disabled catalog source must be unavailable, not cache: {}",
            cat.source
        );
        reset_caches();
    }

    /// Warm TAGS_CACHE must not leak through when both Ollama flags are off.
    #[tokio::test]
    async fn disabled_tags_for_ignores_warm_tags_cache() {
        let _env = EnvRestore::set(&[("ENABLE_OLLAMA", "0"), ("OLLAMA_ENABLED", "0")]).await;
        if let Ok(mut g) = TAGS_CACHE.lock() {
            let mut map = std::collections::HashMap::new();
            map.insert(
                "llama3.2".into(),
                CacheTags {
                    at: Instant::now(),
                    tags: vec![LocalModelTag {
                        name: "llama3.2".into(),
                        tag: "latest".into(),
                        spec: "llama3.2:latest".into(),
                        size_bytes: None,
                        ram_bytes: None,
                        latest: true,
                    }],
                },
            );
            *g = Some(map);
        }

        const FAIL_CLOSED: Duration = Duration::from_secs(2);
        let tags = tokio::time::timeout(FAIL_CLOSED, tags_for("llama3.2"))
            .await
            .expect("disabled tags_for() must return immediately, not read through cache into a fetch");
        assert_eq!(tags.name, "llama3.2");
        assert!(
            tags.tags.is_empty(),
            "disabled tags_for must ignore warm cache, got {} tags from {}",
            tags.tags.len(),
            tags.source
        );
        assert_eq!(
            tags.source, "unavailable",
            "disabled tags_for source must be unavailable, not cache: {}",
            tags.source
        );
        reset_caches();
    }
}
