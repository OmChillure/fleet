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

pub async fn catalog() -> LocalModelCatalog {
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
        Ok(_) | Err(_) => LocalModelCatalog {
            models: Vec::new(),
            source: "unavailable".into(),
        },
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
    if !library_slug_ok(&name) {
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
    if text.is_empty() { None } else { Some(text) }
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

    #[test]
    fn parse_library_keeps_popular_order_and_caps() {
        let html = r#"
          <a href="/library/llama3.1" class="x"><h2>llama3.1</h2>
            <p class="d">Meta&#39;s Llama 3.1 from Meta.</p></a>
          <a href="/library/deepseek-r1"><p class="d">Reasoning.</p></a>
          <a href="/library/llama3.1">dup</a>
          <a href="/library/user/custom">skip</a>
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
    fn parse_tags_reads_size_and_latest_badge() {
        let html = r#"
          <a href="/library/llama3.2:latest">llama3.2:latest</a>
          <span>a80c4f17acd5</span> • 2.0GB • 128K
          <a href="/library/llama3.2:3b">llama3.2:3b
            <span class="border">latest</span></a>
          <span>a80c4f17acd5</span> • 2.0GB • 8GB RAM
          <a href="/library/llama3.2:1b">llama3.2:1b</a>
          • 1.3GB •
          <a href="/library/llama3.2:3b">dup</a>
        "#;
        let tags = parse_tags_html("llama3.2", html);
        assert_eq!(
            tags.iter().map(|t| t.spec.as_str()).collect::<Vec<_>>(),
            vec!["llama3.2:latest", "llama3.2:3b", "llama3.2:1b"]
        );
        assert_eq!(tags[0].size_bytes, Some(2_000_000_000));
        assert!(tags[1].latest);
        assert_eq!(tags[1].ram_bytes, Some(8_000_000_000));
        assert_eq!(tags[2].size_bytes, Some(1_300_000_000));
    }

    #[test]
    fn slug_rejects_paths() {
        assert!(library_slug_ok("llama3.2"));
        assert!(!library_slug_ok("llama3.2/tags"));
        assert!(!library_slug_ok("hi;rm"));
    }
}
