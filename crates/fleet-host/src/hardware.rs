//! Cheap machine snapshot for the worker card. Not a monitoring series.

use serde_json::{Value, json};

pub fn heartbeat_hardware() -> Value {
    json!({
        "cpu_cores": cpu_cores(),
        "mem_total_bytes": mem_total_bytes(),
        "mem_used_pct": mem_used_pct(),
        "os": os_line(),
    })
}

fn cpu_cores() -> Option<u32> {
    std::thread::available_parallelism()
        .ok()
        .map(|n| n.get() as u32)
}

fn os_line() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match uname_release() {
        Some(rel) if !rel.is_empty() => format!("{os} {rel} {arch}"),
        _ => format!("{os} {arch}"),
    }
}

fn uname_release() -> Option<String> {
    let out = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn mem_total_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        return meminfo_kb("MemTotal").map(|kb| kb.saturating_mul(1024));
    }
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    {
        return sysctl_u64("hw.memsize").or_else(|| sysctl_u64("hw.physmem"));
    }
    #[cfg(windows)]
    {
        return win_mem().map(|m| m.0);
    }
    #[allow(unreachable_code)]
    None
}

fn mem_used_pct() -> Option<u8> {
    #[cfg(target_os = "linux")]
    {
        let total = meminfo_kb("MemTotal")?;
        let avail = meminfo_kb("MemAvailable")?;
        if total == 0 {
            return None;
        }
        let used = total.saturating_sub(avail);
        return Some(((used * 100) / total).min(100) as u8);
    }
    #[cfg(windows)]
    {
        return win_mem().map(|m| m.1);
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "linux")]
fn meminfo_kb(key: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_kb(&text, key)
}

fn parse_meminfo_kb(text: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}:");
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn sysctl_u64(name: &str) -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg(name)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(windows)]
fn win_mem() -> Option<(u64, u8)> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virt: u64,
        avail_virt: u64,
        avail_ext: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(s: *mut MemoryStatusEx) -> i32;
    }
    let mut s = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page: 0,
        avail_page: 0,
        total_virt: 0,
        avail_virt: 0,
        avail_ext: 0,
    };
    let ok = unsafe { GlobalMemoryStatusEx(&mut s) };
    if ok == 0 || s.total_phys == 0 {
        return None;
    }
    Some((s.total_phys, s.memory_load.min(100) as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_linux_meminfo() {
        let sample = "MemTotal:       32768000 kB\nMemAvailable:   16384000 kB\n";
        assert_eq!(parse_meminfo_kb(sample, "MemTotal"), Some(32768000));
        assert_eq!(parse_meminfo_kb(sample, "MemAvailable"), Some(16384000));
    }

    #[test]
    fn heartbeat_hardware_has_cores() {
        let v = heartbeat_hardware();
        assert!(v.get("cpu_cores").and_then(|c| c.as_u64()).unwrap_or(0) >= 1);
        assert!(v.get("os").and_then(|o| o.as_str()).is_some());
    }
}
