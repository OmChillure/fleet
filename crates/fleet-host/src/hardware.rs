//! Cheap machine snapshot for the worker card. Not a monitoring series.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Process-local previous CPU counters so heartbeats can report a delta
/// without sleeping on the heartbeat path. First sample is `None`.
static PREV_CPU: Mutex<Option<CpuTimes>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Gpu {
    name: String,
    vram_bytes: Option<u64>,
    used_pct: Option<u8>,
}

struct StaticHw {
    cpu_model: Option<String>,
    mem_speed_mts: Option<u32>,
    mem_type: Option<String>,
    gpus: Vec<Gpu>,
}

pub fn heartbeat_hardware() -> Value {
    let mut obj = json!({
        "cpu_cores": cpu_cores(),
        "mem_total_bytes": mem_total_bytes(),
        "mem_used_pct": mem_used_pct(),
        "os": os_line(),
    });
    let Some(map) = obj.as_object_mut() else {
        return obj;
    };
    let st = static_hw();
    if let Some(model) = st
        .cpu_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        map.insert("cpu_model".into(), json!(model));
    }
    if let Some(pct) = cpu_used_pct() {
        map.insert("cpu_used_pct".into(), json!(pct));
    }
    if let Some(mts) = st.mem_speed_mts.filter(|s| *s > 0) {
        map.insert("mem_speed_mts".into(), json!(mts));
    }
    if let Some(kind) = st
        .mem_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        map.insert("mem_type".into(), json!(kind));
    }
    if !st.gpus.is_empty() {
        let mut gpus = st.gpus.clone();
        attach_gpu_util(&mut gpus);
        let json_gpus: Vec<Value> = gpus
            .iter()
            .filter(|g| !g.name.trim().is_empty())
            .map(gpu_json)
            .collect();
        if !json_gpus.is_empty() {
            map.insert("gpus".into(), Value::Array(json_gpus));
        }
        if let Some(pct) = gpus.iter().filter_map(|g| g.used_pct).max() {
            map.insert("gpu_used_pct".into(), json!(pct));
        }
    } else if let Some(pct) = live_gpu_used_pct() {
        map.insert("gpu_used_pct".into(), json!(pct));
    }
    if let Some((total, free)) = disk_space() {
        map.insert("disk_total_bytes".into(), json!(total));
        map.insert("disk_free_bytes".into(), json!(free));
    }
    let addrs = local_addrs();
    if !addrs.is_empty() {
        map.insert("addrs".into(), json!(addrs));
    }
    obj
}

fn disk_space() -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::mem::MaybeUninit;
        let path = std::ffi::CString::new("/").ok()?;
        let mut buf = MaybeUninit::<libc::statvfs>::uninit();
        let rc = unsafe { libc::statvfs(path.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        let st = unsafe { buf.assume_init() };
        let frsize = st.f_frsize as u64;
        let total = st.f_blocks.saturating_mul(frsize as _) as u64;
        let free = st.f_bavail.saturating_mul(frsize as _) as u64;
        if total == 0 {
            return None;
        }
        return Some((total, free));
    }
    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDiskFreeSpaceExW(
                lp_directory_name: *const u16,
                lp_free_bytes_available_to_caller: *mut u64,
                lp_total_number_of_bytes: *mut u64,
                lp_total_number_of_free_bytes: *mut u64,
            ) -> i32;
        }
        let path: Vec<u16> = OsStr::new("C:\\").encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        let mut total = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(path.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
        };
        if ok == 0 || total == 0 {
            None
        } else {
            Some((total, free))
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn local_addrs() -> Vec<String> {
    let mut out = Vec::new();
    // Primary outbound IPv4.
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if s.connect("1.1.1.1:80").is_ok() {
            if let Ok(addr) = s.local_addr() {
                let ip = addr.ip();
                if !ip.is_loopback() {
                    out.push(ip.to_string());
                }
            }
        }
    }
    out
}

fn gpu_json(g: &Gpu) -> Value {
    let mut o = json!({ "name": g.name.trim() });
    if let Some(m) = o.as_object_mut() {
        if let Some(v) = g.vram_bytes.filter(|n| *n > 0) {
            m.insert("vram_bytes".into(), json!(v));
        }
        if let Some(p) = g.used_pct {
            m.insert("used_pct".into(), json!(p));
        }
    }
    o
}

fn static_hw() -> &'static StaticHw {
    static CACHED: std::sync::OnceLock<StaticHw> = std::sync::OnceLock::new();
    CACHED.get_or_init(|| {
        let (mem_speed_mts, mem_type) = mem_speed();
        let cpu_model = cpu_model();
        StaticHw {
            cpu_model: cpu_model.clone(),
            mem_speed_mts,
            mem_type,
            gpus: collect_gpus(cpu_model.as_deref()),
        }
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
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("uname");
        crate::platform::hide_console(&mut cmd);
        let out = cmd.arg("-r").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

fn cpu_model() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
            if let Some(m) = parse_cpuinfo_model(&text) {
                return Some(m);
            }
        }
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        for key in ["machdep.cpu.brand_string", "hw.model", "hw.machine"] {
            if let Some(s) = sysctl_string(key) {
                return Some(s);
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(s) = win_cpu_model() {
            return Some(s);
        }
    }
    None
}

fn parse_cpuinfo_model(text: &str) -> Option<String> {
    const KEYS: &[&str] = &["model name", "Processor", "cpu model", "Hardware"];
    for key in KEYS {
        for line in text.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            if !k.trim().eq_ignore_ascii_case(key) {
                continue;
            }
            let s = collapse_ws(v);
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn cpu_used_pct() -> Option<u8> {
    let now = read_cpu_times()?;
    let mut prev = PREV_CPU.lock().ok()?;
    let Some(last) = *prev else {
        *prev = Some(now);
        return None;
    };
    *prev = Some(now);
    cpu_used_pct_from_delta(last, now)
}

fn read_cpu_times() -> Option<CpuTimes> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        return parse_proc_stat_cpu(&text);
    }
    #[cfg(windows)]
    {
        return win_cpu_times();
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

fn parse_proc_stat_cpu(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let mut nums = Vec::new();
    for tok in line.split_whitespace().skip(1) {
        nums.push(tok.parse::<u64>().ok()?);
    }
    // user nice system idle iowait irq softirq steal guest guest_nice
    // guest* is already inside user/nice — sum the first 8 only.
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3].saturating_add(nums.get(4).copied().unwrap_or(0));
    let total: u64 = nums.iter().take(8).copied().sum();
    if total == 0 {
        return None;
    }
    Some(CpuTimes { idle, total })
}

fn cpu_used_pct_from_delta(prev: CpuTimes, now: CpuTimes) -> Option<u8> {
    let total_d = now.total.saturating_sub(prev.total);
    if total_d == 0 {
        return None;
    }
    let idle_d = now.idle.saturating_sub(prev.idle);
    let used = total_d.saturating_sub(idle_d);
    Some(((used.saturating_mul(100)) / total_d).min(100) as u8)
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
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    {
        None
    }
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
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
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

/// `(speed_mts, type)` — omit either when unknown. Never requires root.
fn mem_speed() -> (Option<u32>, Option<String>) {
    #[cfg(target_os = "linux")]
    {
        if let Some(pair) = linux_mem_speed() {
            return pair;
        }
    }
    #[cfg(windows)]
    {
        if let Some(pair) = win_mem_speed() {
            return pair;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(pair) = macos_mem_speed() {
            return pair;
        }
    }
    (None, None)
}

#[cfg(target_os = "linux")]
fn linux_mem_speed() -> Option<(Option<u32>, Option<String>)> {
    if let Ok(bytes) = std::fs::read("/sys/firmware/dmi/tables/DMI") {
        if let Some(parsed) = parse_smbios_mem(&bytes) {
            if parsed.0.is_some() || parsed.1.is_some() {
                return Some(parsed);
            }
        }
    }
    if let Some(text) = cmd_stdout_timeout("dmidecode", &["-t", "17"], Duration::from_millis(800)) {
        if let Some(parsed) = parse_dmidecode_memory(&text) {
            if parsed.0.is_some() || parsed.1.is_some() {
                return Some(parsed);
            }
        }
    }
    None
}

fn parse_dmidecode_memory(text: &str) -> Option<(Option<u32>, Option<String>)> {
    let mut speeds = Vec::new();
    let mut types = Vec::new();
    let mut installed = false;
    let mut speed = None;
    let mut configured = None;
    let mut kind = None;

    let mut flush = |installed: &mut bool,
                     speed: &mut Option<u32>,
                     configured: &mut Option<u32>,
                     kind: &mut Option<String>| {
        if *installed {
            if let Some(s) = configured.or(*speed).filter(|n| *n > 0) {
                speeds.push(s);
            }
            if let Some(t) = kind.take() {
                types.push(t);
            }
        }
        *installed = false;
        *speed = None;
        *configured = None;
        *kind = None;
    };

    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Handle ") || t.eq_ignore_ascii_case("Memory Device") {
            flush(&mut installed, &mut speed, &mut configured, &mut kind);
            continue;
        }
        if let Some(rest) = t.strip_prefix("Size:") {
            let rest = rest.trim();
            installed = !rest.is_empty()
                && !rest.eq_ignore_ascii_case("No Module Installed")
                && !rest.eq_ignore_ascii_case("Not Installed")
                && !rest.starts_with('0');
        } else if let Some(rest) = t.strip_prefix("Configured Memory Speed:") {
            configured = parse_mts(rest);
        } else if let Some(rest) = t.strip_prefix("Configured Clock Speed:") {
            configured = parse_mts(rest);
        } else if let Some(rest) = t.strip_prefix("Speed:") {
            speed = parse_mts(rest);
        } else if let Some(rest) = t.strip_prefix("Type:") {
            let tok = rest.split_whitespace().next().unwrap_or("").trim();
            if is_ddr_type(tok) {
                kind = Some(tok.to_string());
            }
        }
    }
    flush(&mut installed, &mut speed, &mut configured, &mut kind);

    let mts = speeds.into_iter().min();
    let typ = types.into_iter().next();
    if mts.is_none() && typ.is_none() {
        None
    } else {
        Some((mts, typ))
    }
}

fn parse_mts(s: &str) -> Option<u32> {
    let tok = s
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| !c.is_ascii_digit());
    if tok.is_empty() {
        return None;
    }
    tok.parse().ok().filter(|n| *n > 0)
}

fn is_ddr_type(s: &str) -> bool {
    let u = s.to_ascii_uppercase();
    u.starts_with("DDR") || u.starts_with("LPDDR")
}

/// Walk a raw SMBIOS/DMI blob for Type 17 (Memory Device).
fn parse_smbios_mem(dmi: &[u8]) -> Option<(Option<u32>, Option<String>)> {
    let mut speeds = Vec::new();
    let mut types = Vec::new();
    let mut off = 0usize;
    while let Some((start, next)) = next_smbios_struct(dmi, off) {
        off = next;
        let typ = dmi[start];
        let len = dmi[start + 1] as usize;
        if typ != 17 || len < 0x17 {
            continue;
        }
        let size = u16::from_le_bytes([dmi[start + 0x0C], dmi[start + 0x0D]]);
        if size == 0 || size == 0xFFFF {
            continue;
        }
        let speed = u16::from_le_bytes([dmi[start + 0x15], dmi[start + 0x16]]);
        let configured = if len >= 0x22 {
            u16::from_le_bytes([dmi[start + 0x20], dmi[start + 0x21]])
        } else {
            0
        };
        let mts = if configured > 0 { configured } else { speed };
        if mts > 0 {
            speeds.push(mts as u32);
        }
        if let Some(name) = smbios_mem_type_name(dmi[start + 0x12]) {
            types.push(name.to_string());
        }
    }
    let mts = speeds.into_iter().min();
    let typ = types.into_iter().next();
    if mts.is_none() && typ.is_none() {
        None
    } else {
        Some((mts, typ))
    }
}

fn next_smbios_struct(data: &[u8], off: usize) -> Option<(usize, usize)> {
    if off + 2 > data.len() {
        return None;
    }
    let len = data[off + 1] as usize;
    if len < 4 || off + len > data.len() {
        return None;
    }
    let mut p = off + len;
    if p >= data.len() {
        return None;
    }
    // Strings: NUL-terminated, table ends with an extra NUL.
    if data[p] == 0 {
        p += 1;
        if p < data.len() && data[p] == 0 {
            p += 1;
        }
        return Some((off, p));
    }
    while p < data.len() {
        if data[p] == 0 {
            p += 1;
            if p < data.len() && data[p] == 0 {
                p += 1;
                break;
            }
        } else {
            p += 1;
        }
    }
    Some((off, p))
}

fn smbios_mem_type_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x20 => "HBM",
        0x21 => "HBM2",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        0x24 => "HBM3",
        _ => return None,
    })
}

fn collect_gpus(cpu_model: Option<&str>) -> Vec<Gpu> {
    let nv = nvidia_gpus();
    let have_nv = !nv.is_empty();
    let mut out = nv;
    for g in platform_gpus() {
        if have_nv && looks_nvidia(&g.name) {
            merge_vram(&mut out, &g);
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|e| names_match(&e.name, &g.name)) {
            if existing.vram_bytes.is_none() {
                existing.vram_bytes = g.vram_bytes;
            }
            continue;
        }
        out.push(g);
    }
    for g in &mut out {
        g.name = tidy_gpu_name(&g.name);
    }
    out.retain(|g| !g.name.trim().is_empty() && !is_dummy_gpu(&g.name));
    if out.is_empty() {
        if let Some(name) = cpu_model.and_then(infer_igpu_from_cpu) {
            out.push(Gpu {
                name,
                vram_bytes: None,
                used_pct: None,
            });
        }
    }
    out
}

fn nvidia_gpus() -> Vec<Gpu> {
    let text = cmd_stdout_timeout(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,utilization.gpu",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_millis(1500),
    )
    .or_else(|| {
        cmd_stdout_timeout(
            "nvidia-smi",
            &[
                "--query-gpu=name,memory.total",
                "--format=csv,noheader,nounits",
            ],
            Duration::from_millis(1500),
        )
    });
    text.map(|t| parse_nvidia_smi_csv(&t)).unwrap_or_default()
}

fn parse_csv_u64(tok: &str) -> Option<u64> {
    tok.trim()
        .trim_matches('"')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .parse()
        .ok()
}

fn parse_nvidia_smi_csv(text: &str) -> Vec<Gpu> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("NVIDIA-SMI")
            || line.starts_with("Failed")
            || line.to_ascii_lowercase().starts_with("name,")
        {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.is_empty() {
            continue;
        }
        // Optional trailing utilization.gpu (0–100). Name may contain commas.
        let (name_mem, used_pct) = if parts.len() >= 3 {
            if let Some(u) = parse_csv_u64(parts[parts.len() - 1]).filter(|n| *n <= 100) {
                let joined = parts[..parts.len() - 1].join(",");
                (joined, Some(u as u8))
            } else {
                (line.to_string(), None)
            }
        } else {
            (line.to_string(), None)
        };
        let Some((name, mem)) = name_mem.rsplit_once(',') else {
            let name = name_mem.trim().trim_matches('"').trim();
            if !name.is_empty() {
                out.push(Gpu {
                    name: name.to_string(),
                    vram_bytes: None,
                    used_pct,
                });
            }
            continue;
        };
        let name = name.trim().trim_matches('"').trim();
        if name.is_empty() {
            continue;
        }
        let vram_bytes = parse_csv_u64(mem)
            .filter(|n| *n > 0)
            .map(|mib| mib.saturating_mul(1024 * 1024));
        out.push(Gpu {
            name: name.to_string(),
            vram_bytes,
            used_pct,
        });
    }
    out
}

fn platform_gpus() -> Vec<Gpu> {
    #[cfg(target_os = "linux")]
    {
        return linux_gpus();
    }
    #[cfg(windows)]
    {
        return win_gpus();
    }
    #[cfg(target_os = "macos")]
    {
        return macos_gpus();
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn linux_gpus() -> Vec<Gpu> {
    let mut from_pci = Vec::new();
    if let Some(text) = cmd_stdout_timeout("lspci", &["-nn"], Duration::from_millis(500)) {
        from_pci = parse_lspci_gpus(&text);
    } else if let Some(text) = cmd_stdout_timeout("lspci", &[], Duration::from_millis(500)) {
        from_pci = parse_lspci_gpus(&text);
    }
    let drm = linux_drm_gpus();
    if from_pci.is_empty() {
        return drm;
    }
    // Attach amdgpu/intel VRAM from sysfs when we can match a single extra GPU
    // or when only one DRM VRAM reading exists.
    let vrams: Vec<u64> = drm.iter().filter_map(|g| g.vram_bytes).collect();
    if vrams.len() == 1 {
        let v = vrams[0];
        let need = from_pci.iter().filter(|g| g.vram_bytes.is_none()).count();
        if need == 1 {
            if let Some(g) = from_pci.iter_mut().find(|g| g.vram_bytes.is_none()) {
                g.vram_bytes = Some(v);
            }
        }
    }
    for extra in drm {
        if extra.vram_bytes.is_none() {
            continue;
        }
        if from_pci
            .iter()
            .any(|g| names_match(&g.name, &extra.name) || looks_same_vendor(g, &extra))
        {
            if let Some(g) = from_pci.iter_mut().find(|g| {
                g.vram_bytes.is_none()
                    && (names_match(&g.name, &extra.name) || looks_same_vendor(g, &extra))
            }) {
                g.vram_bytes = extra.vram_bytes;
            }
            continue;
        }
    }
    from_pci
}

fn parse_lspci_gpus(text: &str) -> Vec<Gpu> {
    let mut out = Vec::new();
    for line in text.lines() {
        let name_part = after_pci_class(line, "VGA compatible controller")
            .or_else(|| after_pci_class(line, "3D controller"))
            .or_else(|| after_pci_class(line, "Display controller"));
        let Some(raw) = name_part else {
            continue;
        };
        let raw = raw.split(" (rev ").next().unwrap_or(raw);
        let name = compact_gpu_name(&pretty_pci_gpu_name(raw));
        if name.is_empty() || is_dummy_gpu(&name) {
            continue;
        }
        out.push(Gpu {
            name,
            vram_bytes: None,
            used_pct: None,
        });
    }
    out
}

/// `lspci` / `lspci -nn`: `VGA compatible controller:` or `VGA compatible controller [0300]:`.
fn after_pci_class<'a>(line: &'a str, class: &str) -> Option<&'a str> {
    let rest = line.split_once(class)?.1.trim_start();
    let rest = if rest.starts_with('[') {
        rest.split_once(']')?.1.trim_start()
    } else {
        rest
    };
    rest.strip_prefix(':').map(str::trim)
}

fn pretty_pci_gpu_name(raw: &str) -> String {
    let raw = collapse_ws(raw);
    let mut marketing: Option<String> = None;
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(rel) = raw[i + 1..].find(']') {
                let inner = raw[i + 1..i + 1 + rel].trim();
                if !inner.is_empty() && !is_pci_id_token(inner) && !is_vendor_tag(inner) {
                    marketing = Some(inner.to_string());
                }
                i += 2 + rel;
                continue;
            }
        }
        i += 1;
    }
    if let Some(m) = marketing {
        return m;
    }
    raw
}

fn is_pci_id_token(s: &str) -> bool {
    let s = s.trim();
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    if s.len() == 9 {
        let b = s.as_bytes();
        return b[4] == b':'
            && s[..4].chars().all(|c| c.is_ascii_hexdigit())
            && s[5..].chars().all(|c| c.is_ascii_hexdigit());
    }
    false
}

fn is_vendor_tag(s: &str) -> bool {
    matches!(
        s.to_ascii_uppercase().as_str(),
        "AMD/ATI" | "AMD" | "ATI" | "NVIDIA" | "INTEL" | "INTEL CORPORATION"
    )
}

fn compact_gpu_name(name: &str) -> String {
    if let Some((a, b)) = name.split_once(" / ") {
        let a = a.trim();
        let b = b.trim();
        if !a.is_empty() && (b.starts_with(a) || a.len() <= b.len()) {
            return a.to_string();
        }
    }
    name.trim().to_string()
}

#[cfg(target_os = "linux")]
fn linux_drm_gpus() -> Vec<Gpu> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return out;
    };
    for ent in entries.flatten() {
        let fname = ent.file_name();
        let name = fname.to_string_lossy();
        if !name.starts_with("card") {
            continue;
        }
        let rest = &name[4..];
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let dev = ent.path().join("device");
        let vram_bytes = std::fs::read_to_string(dev.join("mem_info_vram_total"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0);
        let vendor = std::fs::read_to_string(dev.join("vendor"))
            .ok()
            .map(|s| s.trim().to_ascii_lowercase());
        let label = vendor_gpu_label(vendor.as_deref());
        if label.is_none() && vram_bytes.is_none() {
            continue;
        }
        out.push(Gpu {
            name: label.unwrap_or_else(|| "GPU".into()),
            vram_bytes,
            used_pct: None,
        });
    }
    out
}

fn vendor_gpu_label(vendor: Option<&str>) -> Option<String> {
    let v = vendor?.trim();
    let v = v.strip_prefix("0x").unwrap_or(v);
    Some(
        match v.to_ascii_lowercase().as_str() {
            "10de" => "NVIDIA GPU",
            "1002" | "1022" => "AMD GPU",
            "8086" => "Intel Graphics",
            "106b" => "Apple GPU",
            "17cb" | "5143" => "Qualcomm Adreno",
            "13b5" => "ARM Mali",
            "14e4" => "Broadcom GPU",
            "1ed5" => "Moore Threads GPU",
            "1e3e" => "Iluvatar GPU",
            "19e5" => "HiSilicon GPU",
            "1414" => return None, // Microsoft Basic
            _ => return None,
        }
        .to_string(),
    )
}

fn looks_amd(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("amd") || n.contains("radeon") || n.contains("vega")
}

fn looks_intel(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("intel")
        || n.contains("iris")
        || n.contains("uhd graphics")
        || n.contains("hd graphics")
        || n.contains("arc graphics")
        || n.contains("xe graphics")
}

fn attach_gpu_util(gpus: &mut [Gpu]) {
    if gpus.iter().all(|g| g.used_pct.is_some()) {
        return;
    }
    let Some(busy) = linux_drm_gpu_busy_max() else {
        return;
    };
    let igpu: Vec<usize> = gpus
        .iter()
        .enumerate()
        .filter(|(_, g)| g.used_pct.is_none() && (looks_amd(&g.name) || looks_intel(&g.name)))
        .map(|(i, _)| i)
        .collect();
    if igpu.len() == 1 {
        gpus[igpu[0]].used_pct = Some(busy);
        return;
    }
    if gpus.len() == 1 && gpus[0].used_pct.is_none() {
        gpus[0].used_pct = Some(busy);
        return;
    }
    if igpu.is_empty() {
        if let Some(g) = gpus.iter_mut().find(|g| g.used_pct.is_none()) {
            g.used_pct = Some(busy);
        }
    }
}

fn live_gpu_used_pct() -> Option<u8> {
    linux_drm_gpu_busy_max()
}

fn linux_drm_gpu_busy_max() -> Option<u8> {
    #[cfg(not(target_os = "linux"))]
    {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let mut best: Option<u8> = None;
        let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
            return None;
        };
        for ent in entries.flatten() {
            let name = ent.file_name();
            let n = name.to_string_lossy();
            if !n.starts_with("card") {
                continue;
            }
            let rest = &n[4..];
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let path = ent.path().join("device/gpu_busy_percent");
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let Some(p) = parse_busy_percent(&text) else {
                continue;
            };
            best = Some(best.map_or(p, |b| b.max(p)));
        }
        best
    }
}

fn parse_busy_percent(s: &str) -> Option<u8> {
    let n: u32 = s.trim().split_whitespace().next()?.parse().ok()?;
    if n > 100 { None } else { Some(n as u8) }
}

fn looks_nvidia(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("nvidia")
        || n.contains("geforce")
        || n.contains("tesla")
        || n.contains("quadro")
        || n.contains("rtx ")
        || n.contains("gtx ")
}

fn looks_same_vendor(a: &Gpu, b: &Gpu) -> bool {
    let an = a.name.to_ascii_lowercase();
    let bn = b.name.to_ascii_lowercase();
    (an.contains("amd") || an.contains("radeon") || an.contains("vega"))
        && (bn.contains("amd") || bn.contains("radeon") || bn == "amd gpu")
        || (looks_nvidia(&a.name) && (looks_nvidia(&b.name) || bn == "nvidia gpu"))
        || (looks_intel(&a.name) && (looks_intel(&b.name) || bn == "intel graphics"))
}

fn names_match(a: &str, b: &str) -> bool {
    collapse_ws(a).eq_ignore_ascii_case(&collapse_ws(b))
}

fn merge_vram(out: &mut [Gpu], extra: &Gpu) {
    if extra.vram_bytes.is_none() {
        return;
    }
    if let Some(g) = out.iter_mut().find(|e| {
        e.vram_bytes.is_none() && (names_match(&e.name, &extra.name) || looks_same_vendor(e, extra))
    }) {
        g.vram_bytes = extra.vram_bytes;
    }
}

fn tidy_gpu_name(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    for tok in ["(R)", "(TM)", "(C)", "®", "™"] {
        s = s.replace(tok, "");
    }
    s = collapse_ws(&s);
    for prefix in [
        "Intel Corporation ",
        "NVIDIA Corporation ",
        "Advanced Micro Devices, Inc. ",
        "Qualcomm Technologies, Inc. ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim().to_string();
        }
    }
    let lower = s.to_ascii_lowercase();
    if (lower.contains("uhd graphics")
        || lower.contains("hd graphics")
        || lower.contains("iris")
        || lower.contains("arc graphics")
        || lower.contains("xe graphics"))
        && !lower.contains("intel")
    {
        return format!("Intel {s}");
    }
    s
}

/// When the box listed no GPU, name the usual iGPU for this CPU.
fn infer_igpu_from_cpu(cpu: &str) -> Option<String> {
    let n = cpu.to_ascii_lowercase();
    if n.contains("snapdragon") || n.contains("qualcomm") {
        return Some("Qualcomm Adreno".into());
    }
    if n.contains("apple") && n.contains('m') {
        for tag in [
            "m4 max", "m4 pro", "m4", "m3 max", "m3 pro", "m3", "m2", "m1",
        ] {
            if n.contains(tag) {
                let pretty = tag
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            Some(f) => format!("{}{}", f.to_ascii_uppercase(), c.as_str()),
                            None => String::new(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                return Some(format!("Apple {pretty} GPU"));
            }
        }
    }
    if n.contains("ryzen") || n.contains("amd") {
        if n.contains("hx 370") {
            return Some("AMD Radeon 890M".into());
        }
        if n.contains("7840") || n.contains("7940") || n.contains("8840") || n.contains("8700g") {
            return Some("AMD Radeon 780M".into());
        }
        if n.contains("5700g") || n.contains("5600g") || n.contains("with radeon") {
            return Some("AMD Radeon Vega".into());
        }
        if n.contains('u') || n.contains('h') || n.contains("hx") {
            return Some("AMD Radeon Graphics".into());
        }
        return None;
    }
    infer_intel_igpu(&n)
}

fn infer_intel_igpu(n: &str) -> Option<String> {
    if n.contains("ultra") {
        return Some("Intel Arc Graphics".into());
    }
    if n.contains("xeon") && !n.contains("e3-") && !n.contains("e-") {
        return None;
    }
    // i7-10610u / i7-1165g7
    let bytes = n.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'i' && matches!(bytes[i + 1], b'3' | b'5' | b'7' | b'9') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j] == b'-' || bytes[j] == b' ') {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j - start >= 4 {
                let num = &n[start..j];
                let family: u16 = if num.len() >= 5 {
                    num[..2].parse().ok()?
                } else {
                    let two: u16 = num[..2].parse().ok()?;
                    if (10..=16).contains(&two) {
                        two
                    } else {
                        num[..1].parse().ok()?
                    }
                };
                let mut end = j;
                while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
                    end += 1;
                }
                let sku = &n[start..end];
                let g7 = sku.contains("g7") || sku.contains("g4");
                return Some(
                    match family {
                        2 => "Intel HD Graphics 3000",
                        3 => "Intel HD Graphics 4000",
                        4..=7 => "Intel HD Graphics",
                        8 | 9 => "Intel UHD Graphics",
                        10 if g7 => "Intel Iris Plus",
                        10 => "Intel UHD Graphics",
                        11..=14 => "Intel Iris Xe",
                        _ => "Intel Graphics",
                    }
                    .into(),
                );
            }
        }
        i += 1;
    }
    if n.contains("intel") {
        return Some("Intel Graphics".into());
    }
    None
}

fn is_dummy_gpu(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("aspeed")
        || n.contains("cirrus")
        || n.contains("qxl")
        || n.contains("virtio")
        || n.contains("bochs")
        || n.contains("basic render")
        || n.contains("microsoft basic")
        || n.contains("standard vga")
        || n.contains("dummy")
        || n.contains("vmware")
        || n.contains("virtualbox")
        || n.contains("llvmpipe")
        || n.contains("svga")
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cmd_stdout_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(program);
    crate::platform::hide_console(&mut cmd);
    let mut child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut buf = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut buf);
                }
                if !status.success() {
                    return None;
                }
                let s = String::from_utf8_lossy(&buf).trim().to_string();
                return if s.is_empty() { None } else { Some(s) };
            }
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(15));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn sysctl_string(name: &str) -> Option<String> {
    let out = Command::new("sysctl").arg("-n").arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn sysctl_u64(name: &str) -> Option<u64> {
    let out = Command::new("sysctl").arg("-n").arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn macos_mem_speed() -> Option<(Option<u32>, Option<String>)> {
    let text = cmd_stdout_timeout(
        "system_profiler",
        &["SPMemoryDataType", "-detailLevel", "mini"],
        Duration::from_millis(2000),
    )?;
    parse_macos_memory(&text)
}

#[cfg(any(test, target_os = "macos"))]
fn parse_macos_memory(text: &str) -> Option<(Option<u32>, Option<String>)> {
    let mut speeds = Vec::new();
    let mut types = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Speed:").or_else(|| t.strip_prefix("Type:")) {
            if t.starts_with("Speed:") {
                if let Some(s) = parse_mts(rest) {
                    speeds.push(s);
                }
            } else {
                let tok = rest.split_whitespace().next().unwrap_or("").trim();
                if is_ddr_type(tok) {
                    types.push(tok.to_string());
                }
            }
        }
    }
    let mts = speeds.into_iter().min();
    let typ = types.into_iter().next();
    if mts.is_none() && typ.is_none() {
        None
    } else {
        Some((mts, typ))
    }
}

#[cfg(target_os = "macos")]
fn macos_gpus() -> Vec<Gpu> {
    let Some(text) = cmd_stdout_timeout(
        "system_profiler",
        &["SPDisplaysDataType", "-detailLevel", "mini"],
        Duration::from_millis(2000),
    ) else {
        return Vec::new();
    };
    parse_macos_displays(&text)
}

#[cfg(any(test, target_os = "macos"))]
fn parse_macos_displays(text: &str) -> Vec<Gpu> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut vram = None;
    let flush = |out: &mut Vec<Gpu>, name: &mut Option<String>, vram: &mut Option<u64>| {
        if let Some(n) = name.take().filter(|s| !s.is_empty() && !is_dummy_gpu(s)) {
            out.push(Gpu {
                name: n,
                vram_bytes: vram.take(),
                used_pct: None,
            });
        }
        *vram = None;
    };
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Chipset Model:") {
            flush(&mut out, &mut name, &mut vram);
            let n = collapse_ws(rest);
            if !n.is_empty() {
                name = Some(n);
            }
        } else if let Some(rest) = t
            .strip_prefix("VRAM (Total):")
            .or_else(|| t.strip_prefix("VRAM (Dynamic, Max):"))
            .or_else(|| t.strip_prefix("VRAM:"))
        {
            vram = parse_macos_vram(rest);
        }
    }
    flush(&mut out, &mut name, &mut vram);
    out
}

#[cfg(any(test, target_os = "macos"))]
fn parse_macos_vram(s: &str) -> Option<u64> {
    let mut toks = s.split_whitespace();
    let n: f64 = toks.next()?.replace(',', "").parse().ok()?;
    if n <= 0.0 {
        return None;
    }
    let unit = toks.next().unwrap_or("MB").to_ascii_uppercase();
    let bytes = if unit.starts_with("GB") {
        n * 1_073_741_824.0
    } else if unit.starts_with("TB") {
        n * 1_099_511_627_776.0
    } else {
        n * 1_048_576.0
    };
    Some(bytes as u64)
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

#[cfg(windows)]
fn win_cpu_times() -> Option<CpuTimes> {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        lo: u32,
        hi: u32,
    }
    impl FileTime {
        fn as_u64(self) -> u64 {
            ((self.hi as u64) << 32) | (self.lo as u64)
        }
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    }
    let mut idle = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }
    let idle_t = idle.as_u64();
    let kernel_t = kernel.as_u64();
    let user_t = user.as_u64();
    // Kernel includes idle on Windows.
    let total = kernel_t.saturating_add(user_t);
    if total == 0 {
        return None;
    }
    Some(CpuTimes {
        idle: idle_t,
        total,
    })
}

#[cfg(windows)]
fn win_cpu_model() -> Option<String> {
    if let Some(text) = cmd_stdout_timeout(
        "reg",
        &[
            "query",
            r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "/v",
            "ProcessorNameString",
        ],
        Duration::from_millis(800),
    ) {
        for line in text.lines() {
            if line.to_ascii_uppercase().contains("PROCESSORNAMESTRING") {
                if let Some((_, rest)) = line.split_once("REG_SZ") {
                    let s = collapse_ws(rest);
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    if let Some(text) = cmd_stdout_timeout(
        "wmic",
        &["cpu", "get", "Name", "/value"],
        Duration::from_millis(1500),
    ) {
        return parse_wmic_value(&text, "Name");
    }
    None
}

#[cfg(windows)]
fn win_mem_speed() -> Option<(Option<u32>, Option<String>)> {
    let text = cmd_stdout_timeout(
        "wmic",
        &[
            "memorychip",
            "get",
            "Speed,SMBIOSMemoryType,MemoryType",
            "/format:list",
        ],
        Duration::from_millis(1500),
    )?;
    parse_wmic_memory(&text)
}

#[cfg(any(test, windows))]
fn parse_wmic_memory(text: &str) -> Option<(Option<u32>, Option<String>)> {
    let mut speeds = Vec::new();
    let mut types = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Speed=") {
            if let Some(s) = parse_mts(rest) {
                speeds.push(s);
            }
        } else if let Some(rest) = t.strip_prefix("SMBIOSMemoryType=") {
            if let Ok(code) = rest.trim().parse::<u8>() {
                if let Some(name) = smbios_mem_type_name(code) {
                    types.push(name.to_string());
                }
            }
        }
    }
    let mts = speeds.into_iter().min();
    let typ = types.into_iter().next();
    if mts.is_none() && typ.is_none() {
        None
    } else {
        Some((mts, typ))
    }
}

#[cfg(windows)]
fn parse_wmic_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix(&prefix) {
            let s = collapse_ws(rest);
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

#[cfg(windows)]
fn win_gpus() -> Vec<Gpu> {
    // Win11 24H2 dropped WMIC. CIM first; list format matches parse_wmic_gpus.
    if let Some(text) = win_cim_videocontrollers() {
        let gpus = parse_wmic_gpus(&text);
        if !gpus.is_empty() {
            return gpus;
        }
    }
    if let Some(text) = cmd_stdout_timeout(
        "wmic",
        &[
            "path",
            "win32_VideoController",
            "get",
            "Name,AdapterRAM",
            "/format:list",
        ],
        Duration::from_millis(1500),
    ) {
        return parse_wmic_gpus(&text);
    }
    Vec::new()
}

#[cfg(windows)]
fn win_cim_videocontrollers() -> Option<String> {
    let script = "Get-CimInstance -ClassName Win32_VideoController | ForEach-Object { 'Name=' + $_.Name; if ($null -ne $_.AdapterRAM) { 'AdapterRAM=' + $_.AdapterRAM }; '' }";
    for bin in ["powershell", "pwsh"] {
        if let Some(text) = cmd_stdout_timeout(
            bin,
            &["-NoProfile", "-NonInteractive", "-Command", script],
            Duration::from_millis(2500),
        ) {
            if text.contains("Name=") {
                return Some(text);
            }
        }
    }
    None
}

#[cfg(any(test, windows))]
fn parse_wmic_gpus(text: &str) -> Vec<Gpu> {
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut vram = None;
    let flush = |out: &mut Vec<Gpu>, name: &mut Option<String>, vram: &mut Option<u64>| {
        if let Some(n) = name.take().filter(|s| !s.is_empty() && !is_dummy_gpu(s)) {
            out.push(Gpu {
                name: n,
                vram_bytes: vram.take(),
                used_pct: None,
            });
        }
        *vram = None;
    };
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            flush(&mut out, &mut name, &mut vram);
            continue;
        }
        if let Some(rest) = t.strip_prefix("Name=") {
            if name.is_some() {
                flush(&mut out, &mut name, &mut vram);
            }
            let n = collapse_ws(rest);
            if !n.is_empty() {
                name = Some(n);
            }
        } else if let Some(rest) = t.strip_prefix("AdapterRAM=") {
            vram = rest
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0 && *n != u32::MAX as u64);
        }
    }
    flush(&mut out, &mut name, &mut vram);
    out
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
    fn parse_cpuinfo_prefers_model_name() {
        let sample = "\
processor	: 0
vendor_id	: AuthenticAMD
cpu family	: 25
model name	: AMD Ryzen 7 5700G with Radeon Graphics
stepping	: 0
";
        assert_eq!(
            parse_cpuinfo_model(sample).as_deref(),
            Some("AMD Ryzen 7 5700G with Radeon Graphics")
        );
    }

    #[test]
    fn parse_cpuinfo_arm_hardware() {
        let sample = "\
Processor	: ARMv7 Processor rev 4 (v7l)
Hardware	: BCM2835
";
        // Processor is listed first in KEYS after model name; Hardware is the board.
        // model name is absent, so Processor wins (more specific than board).
        assert_eq!(
            parse_cpuinfo_model(sample).as_deref(),
            Some("ARMv7 Processor rev 4 (v7l)")
        );
    }

    #[test]
    fn proc_stat_delta_is_used_over_total() {
        // 90% used since boot; next 100 ticks are 10% used so interval ≠ lifetime.
        let first = parse_proc_stat_cpu("cpu  90 0 0 10 0 0 0 0 0 0\n").expect("first");
        let second = parse_proc_stat_cpu("cpu  100 0 0 100 0 0 0 0 0 0\n").expect("second");
        // idle 10→100 (90), total 100→200 (100) → 10% used (not boot 90% or 50%)
        assert_eq!(cpu_used_pct_from_delta(first, second), Some(10));
        assert_eq!(cpu_used_pct_from_delta(first, first), None);
    }

    #[test]
    fn parse_proc_stat_cpu_none_without_aggregate_cpu_line() {
        assert_eq!(
            parse_proc_stat_cpu("cpu0 100 0 50 850\ncpu1 100 0 50 850\nintr 123 0 1\n"),
            None
        );
    }

    #[test]
    fn parse_proc_stat_cpu_excludes_guest_from_total() {
        let a = parse_proc_stat_cpu("cpu  90 0 0 10 0 0 0 0 0 0\n").unwrap();
        let b = parse_proc_stat_cpu("cpu  100 0 0 100 0 0 0 0 50 50\n").unwrap();
        // guest* already in user/nice; summing past 8 would be 55% not 10%
        assert_eq!(cpu_used_pct_from_delta(a, b), Some(10));
    }

    #[test]
    fn cpu_used_pct_from_delta_counts_steal_as_used() {
        let a = parse_proc_stat_cpu("cpu  0 0 0 100 0 0 0 0 0 0\n").unwrap();
        let b = parse_proc_stat_cpu("cpu  0 0 0 100 0 0 0 100 0 0\n").unwrap();
        // idle unchanged; steal +100 is in take(8) → Some(100); drop steal → total_d 0
        assert_eq!(cpu_used_pct_from_delta(a, b), Some(100));
    }

    #[test]
    fn proc_stat_includes_iowait_as_idle() {
        let a = parse_proc_stat_cpu("cpu  0 0 0 100 50 0 0 0\n").unwrap();
        let b = parse_proc_stat_cpu("cpu  0 0 0 200 150 0 0 0\n").unwrap();
        assert_eq!(cpu_used_pct_from_delta(a, b), Some(0));
    }

    #[test]
    fn nvidia_smi_csv_parses_mib() {
        let text = "NVIDIA GeForce RTX 3080, 10240\nTesla V100-SXM2-16GB, 16160\n";
        let gpus = parse_nvidia_smi_csv(text);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3080");
        assert_eq!(gpus[0].vram_bytes, Some(10240 * 1024 * 1024));
        assert_eq!(gpus[0].used_pct, None);
        assert_eq!(gpus[1].name, "Tesla V100-SXM2-16GB");
        assert_eq!(gpus[1].vram_bytes, Some(16160 * 1024 * 1024));
    }

    #[test]
    fn nvidia_smi_csv_parses_utilization() {
        let text = "NVIDIA GeForce RTX 3080, 10240, 45\n";
        let gpus = parse_nvidia_smi_csv(text);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3080");
        assert_eq!(gpus[0].vram_bytes, Some(10240 * 1024 * 1024));
        assert_eq!(gpus[0].used_pct, Some(45));
    }

    #[test]
    fn amdgpu_busy_percent_parses() {
        assert_eq!(parse_busy_percent("12\n"), Some(12));
        assert_eq!(parse_busy_percent("100"), Some(100));
        assert_eq!(parse_busy_percent("101"), None);
        assert_eq!(parse_busy_percent(""), None);
    }

    #[test]
    fn nvidia_smi_csv_strips_units_and_quotes() {
        let text = "\"NVIDIA GeForce RTX 4090\", 24576 MiB\n";
        let gpus = parse_nvidia_smi_csv(text);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpus[0].vram_bytes, Some(24576 * 1024 * 1024));
    }

    #[test]
    fn dmidecode_memory_prefers_configured_speed() {
        let sample = "\
Handle 0x003A, DMI type 17, 40 bytes
Memory Device
	Size: 16 GB
	Type: DDR4
	Speed: 2666 MT/s
	Configured Memory Speed: 3200 MT/s

Handle 0x003B, DMI type 17, 40 bytes
Memory Device
	Size: No Module Installed
	Type: Unknown
	Speed: Unknown
";
        let (mts, kind) = parse_dmidecode_memory(sample).expect("parsed");
        assert_eq!(mts, Some(3200));
        assert_eq!(kind.as_deref(), Some("DDR4"));
    }

    #[test]
    fn smbios_type17_speed_and_ddr4() {
        let mut rec = vec![0u8; 0x22];
        rec[0] = 17;
        rec[1] = 0x22;
        rec[0x0C] = 0x00;
        rec[0x0D] = 0x40; // installed
        rec[0x12] = 0x1A; // DDR4
        rec[0x15] = 0x80;
        rec[0x16] = 0x0C; // 3200 advertised
        rec[0x20] = 0x72;
        rec[0x21] = 0x0A; // 2674 configured — take this
        rec.extend_from_slice(&[0, 0]);
        let (mts, kind) = parse_smbios_mem(&rec).expect("parsed");
        assert_eq!(mts, Some(2674));
        assert_eq!(kind.as_deref(), Some("DDR4"));
    }

    #[test]
    fn lspci_extracts_marketing_name() {
        let text = "\
00:01.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Cezanne [Radeon Vega Series / Radeon Vega Mobile Series] [1002:1638] (rev c8)
01:00.0 3D controller: NVIDIA Corporation GA104 [GeForce RTX 3070] (rev a1)
02:00.0 VGA compatible controller: ASPEED Technology, Inc. ASPEED Graphics Family (rev 41)
";
        let gpus = parse_lspci_gpus(text);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "Radeon Vega Series");
        assert_eq!(gpus[1].name, "GeForce RTX 3070");
    }

    #[test]
    fn infers_x1_comet_lake_uhd() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz").as_deref(),
            Some("Intel UHD Graphics")
        );
    }

    #[test]
    fn infers_ivy_bridge_hd_4000() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Core i5-3320M").as_deref(),
            Some("Intel HD Graphics 4000")
        );
    }

    #[test]
    fn infers_core_ultra_arc() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Core(TM) Ultra 7 155H").as_deref(),
            Some("Intel Arc Graphics")
        );
    }

    #[test]
    fn infers_tiger_lake_iris_xe() {
        assert_eq!(
            infer_igpu_from_cpu("11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz").as_deref(),
            Some("Intel Iris Xe")
        );
    }

    #[test]
    fn infers_ice_lake_g7_iris_plus() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Core i7-1065G7").as_deref(),
            Some("Intel Iris Plus")
        );
    }

    #[test]
    fn discrete_xeon_has_no_igpu() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz"),
            None
        );
    }

    #[test]
    fn parse_macos_memory_reads_speed_and_ddr4() {
        let mem = parse_macos_memory("      Speed: 3200 MHz\n      Type: DDR4\n").unwrap();
        assert_eq!(mem.0, Some(3200));
        assert_eq!(mem.1.as_deref(), Some("DDR4"));
    }

    #[test]
    fn parse_macos_memory_takes_min_speed_across_dimms() {
        let mem =
            parse_macos_memory("      Speed: 3200 MHz\n      Speed: 2666 MHz\n      Type: DDR4\n")
                .unwrap();
        assert_eq!(mem.0, Some(2666));
        assert_eq!(mem.1.as_deref(), Some("DDR4"));
    }

    #[test]
    fn parse_macos_memory_accepts_lpddr_and_none_when_empty() {
        let mem = parse_macos_memory("      Type: LPDDR5\n      Speed: 6400 MHz\n").unwrap();
        assert_eq!(mem.0, Some(6400));
        assert_eq!(mem.1.as_deref(), Some("LPDDR5"));
        assert!(parse_macos_memory("").is_none());
        assert!(parse_macos_memory("      Type: Unknown\n").is_none());
    }

    #[test]
    fn parse_macos_displays_reads_chipset_and_vram_total_gb() {
        let gpus =
            parse_macos_displays("      Chipset Model: Apple M1\n      VRAM (Total): 8 GB\n");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "Apple M1");
        assert_eq!(gpus[0].vram_bytes, Some(8 * 1_073_741_824));
    }

    #[test]
    fn parse_macos_displays_reads_vram_dynamic_max() {
        let gpus = parse_macos_displays(
            "      Chipset Model: Apple M1\n      VRAM (Dynamic, Max): 8 GB\n",
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "Apple M1");
        assert_eq!(gpus[0].vram_bytes, Some(8 * 1_073_741_824));
    }

    #[test]
    fn wmic_memory_reads_speed_and_ddr4() {
        let mem = parse_wmic_memory("Speed=3200\r\nSMBIOSMemoryType=26\r\n").unwrap();
        assert_eq!(mem.0, Some(3200));
        assert_eq!(mem.1.as_deref(), Some("DDR4"));
    }

    #[test]
    fn wmic_memory_uses_min_stick_speed() {
        let mem = parse_wmic_memory("Speed=3200\r\nSpeed=2666\r\nSMBIOSMemoryType=26\r\n").unwrap();
        assert_eq!(mem.0, Some(2666));
        assert_eq!(mem.1.as_deref(), Some("DDR4"));
    }

    #[test]
    fn wmic_memory_unknown_smbios_code_omits_type() {
        assert_eq!(
            parse_wmic_memory("Speed=3200\r\nSMBIOSMemoryType=0\r\n"),
            Some((Some(3200), None))
        );
    }

    #[test]
    fn wmic_gpus_reads_amd_radeon_adapter_ram() {
        let gpus = parse_wmic_gpus("Name=AMD Radeon(TM) Graphics\r\nAdapterRAM=536870912\r\n");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "AMD Radeon(TM) Graphics");
        assert_eq!(gpus[0].vram_bytes, Some(536870912));
    }

    #[test]
    fn wmic_gpus_adapter_ram_u32_max_omits_vram() {
        let gpus = parse_wmic_gpus("Name=AMD Radeon\r\nAdapterRAM=4294967295\r\n");
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "AMD Radeon");
        assert_eq!(gpus[0].vram_bytes, None);
    }

    #[test]
    fn gpu_json_normalize_does_not_invent_vram() {
        let cases = [
            (None, None),
            (Some(0), None),
            (Some(10_737_418_240u64), Some(10_737_418_240u64)),
        ];
        for (vram_bytes, want) in cases {
            let v = gpu_json(&Gpu {
                name: "Intel UHD Graphics".into(),
                vram_bytes,
                used_pct: None,
            });
            assert_eq!(
                v.get("vram_bytes"),
                want.map(Value::from).as_ref(),
                "vram_bytes={vram_bytes:?}"
            );
            assert!(
                v.get("used_pct").is_none(),
                "used_pct omitted when None; vram_bytes={vram_bytes:?}"
            );
        }
    }

    #[test]
    fn gpu_json_includes_positive_vram_and_used_pct() {
        let v = gpu_json(&Gpu {
            name: "NVIDIA GeForce RTX 3080".into(),
            vram_bytes: Some(10_737_418_240),
            used_pct: Some(40),
        });
        assert_eq!(v["vram_bytes"], 10_737_418_240_u64);
        assert_eq!(v["used_pct"], 40);
    }

    #[test]
    fn gpu_json_emits_used_pct_zero() {
        let v = gpu_json(&Gpu {
            name: "Intel UHD Graphics".into(),
            vram_bytes: None,
            used_pct: Some(0),
        });
        assert_eq!(v["used_pct"], 0);
    }

    #[test]
    fn gpu_json_trims_name() {
        let v = gpu_json(&Gpu {
            name: "  Intel UHD Graphics  ".into(),
            vram_bytes: None,
            used_pct: None,
        });
        assert_eq!(v["name"], "Intel UHD Graphics");
    }

    #[test]
    fn lspci_drops_empty_gpu_names_keeps_intel_igpu() {
        let text = "\
00:00.0 VGA compatible controller:
00:01.0 VGA compatible controller [0300]:
00:02.0 VGA compatible controller [0300]: Intel Corporation CometLake-U GT2 [UHD Graphics] [8086:9b41] (rev 02)
02:00.0 VGA compatible controller: ASPEED Technology, Inc. ASPEED Graphics Family (rev 41)
";
        let gpus = parse_lspci_gpus(text);
        assert_eq!(gpus.len(), 1);
        assert_eq!(tidy_gpu_name(&gpus[0].name), "Intel UHD Graphics");
        assert_eq!(gpus[0].vram_bytes, None);
    }

    #[test]
    fn wmic_drops_empty_gpu_names_keeps_intel_igpu() {
        let gpus = parse_wmic_gpus(
            "Name=\r\nAdapterRAM=1073741824\r\n\r\nName=Intel(R) UHD Graphics\r\nAdapterRAM=0\r\n\r\nName=Microsoft Basic Render Driver\r\nAdapterRAM=0\r\n",
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(tidy_gpu_name(&gpus[0].name), "Intel UHD Graphics");
        assert_eq!(gpus[0].vram_bytes, None);
    }

    #[test]
    fn macos_displays_drops_empty_gpu_names_keeps_igpu() {
        let gpus = parse_macos_displays(
            "      Chipset Model: \n      VRAM (Total): 8 GB\n      Chipset Model: Intel UHD Graphics\n      VRAM (Total): 0 MB\n",
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "Intel UHD Graphics");
        assert_eq!(gpus[0].vram_bytes, None);
    }

    #[test]
    fn heartbeat_hardware_has_cores() {
        let v = heartbeat_hardware();
        let want = std::thread::available_parallelism()
            .expect("logical CPUs")
            .get() as u32;
        assert_eq!(v.get("cpu_cores"), Some(&json!(want)));
    }

    #[test]
    fn os_line_includes_consts_os_and_arch() {
        let v = heartbeat_hardware();
        let os = v.get("os").and_then(Value::as_str).expect("os string");
        assert!(!os.is_empty());
        assert!(os.contains(std::env::consts::OS), "os={os}");
        assert!(os.contains(std::env::consts::ARCH), "os={os}");
    }

    #[test]
    fn heartbeat_hardware_always_includes_mem_snapshot_keys() {
        let v = heartbeat_hardware();
        assert!(v.get("mem_total_bytes").is_some());
        assert!(v.get("mem_used_pct").is_some());
    }

    #[test]
    fn heartbeat_hardware_disk_keys_are_paired() {
        let v = heartbeat_hardware();
        let total = v.get("disk_total_bytes");
        let free = v.get("disk_free_bytes");
        assert_eq!(
            total.is_some(),
            free.is_some(),
            "disk_total_bytes and disk_free_bytes must appear together"
        );
        assert_eq!(
            total.is_some(),
            total.and_then(Value::as_u64).is_some(),
            "disk_total_bytes is u64 when present, never null"
        );
        assert_eq!(
            free.is_some(),
            free.and_then(Value::as_u64).is_some(),
            "disk_free_bytes is u64 when present, never null"
        );
    }
}
