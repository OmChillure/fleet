//! GPU / iGPU name cleanup and CPU→iGPU inference.
//!
//! Hosts often omit the iGPU (old binaries, missing WMIC). The plane still
//! knows the CPU, so we can name the usual integrated GPU for that SKU.

/// Strip trademark noise and vendor-corporation prefixes.
pub fn normalize_gpu_name(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    for tok in ["(R)", "(TM)", "(C)", "®", "™", "©"] {
        s = s.replace(tok, "");
    }
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    for prefix in [
        "Intel Corporation ",
        "NVIDIA Corporation ",
        "Advanced Micro Devices, Inc. ",
        "Advanced Micro Devices ",
        "Qualcomm Technologies, Inc. ",
        "Qualcomm Incorporated ",
        "ARM Ltd. ",
        "Apple Inc. ",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim().to_string();
        }
    }
    qualify_vendor_prefix(&s)
}

fn qualify_vendor_prefix(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if intel_family(&lower) && !lower.contains("intel") {
        return format!("Intel {name}");
    }
    if nvidia_family(&lower) && !lower.contains("nvidia") && !lower.starts_with("geforce") {
        return format!("NVIDIA {name}");
    }
    if amd_family(&lower) && !lower.contains("amd") && !lower.contains("radeon") {
        return format!("AMD {name}");
    }
    name.to_string()
}

fn intel_family(n: &str) -> bool {
    n.contains("iris")
        || n.contains("uhd graphics")
        || n.contains("hd graphics")
        || n.contains("arc graphics")
        || n.contains("arc a")
        || n.contains("arc b")
        || n.contains("xe graphics")
        || n.contains("xe-lp")
        || n.contains("battlemage")
        || n.contains("alchemist")
}

fn nvidia_family(n: &str) -> bool {
    n.contains("geforce")
        || n.contains("quadro")
        || n.contains("tesla")
        || n.contains("rtx ")
        || n.contains("gtx ")
        || n.contains("titan")
}

fn amd_family(n: &str) -> bool {
    n.contains("radeon")
        || n.contains("vega")
        || n.contains("rdna")
        || n.contains("navi")
        || n.contains("instinct")
}

/// Guess the usual iGPU from a CPU brand string when the box reported none.
pub fn infer_igpu_from_cpu(cpu: &str) -> Option<String> {
    let n = cpu.to_ascii_lowercase();
    if n.trim().is_empty() {
        return None;
    }
    if let Some(name) = infer_apple(&n) {
        return Some(name);
    }
    if n.contains("snapdragon") || n.contains("qualcomm") {
        return Some("Qualcomm Adreno".into());
    }
    if n.contains("mali") {
        return Some("ARM Mali".into());
    }
    if n.contains("immortalis") {
        return Some("ARM Immortalis".into());
    }
    if n.contains("amd") || n.contains("ryzen") || n.contains("athlon") || n.contains("epyc") {
        return infer_amd(&n);
    }
    if n.contains("intel")
        || n.contains("core i")
        || n.contains("core ultra")
        || n.contains("pentium")
        || n.contains("celeron")
        || n.contains("xeon")
    {
        return infer_intel(&n);
    }
    None
}

fn infer_apple(n: &str) -> Option<String> {
    for tag in [
        "m4 max", "m4 pro", "m4", "m3 max", "m3 pro", "m3", "m2 ultra", "m2 max", "m2 pro", "m2",
        "m1 ultra", "m1 max", "m1 pro", "m1",
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
    None
}

fn infer_amd(n: &str) -> Option<String> {
    // Ryzen AI / Strix / Hawk Point / Phoenix / Dragon / Rembrandt / Cezanne.
    if n.contains("hx 370") || n.contains("ai 9 hx 370") {
        return Some("AMD Radeon 890M".into());
    }
    if n.contains("hx 365") || n.contains("ai 9 365") || n.contains("ai 7 350") {
        return Some("AMD Radeon 880M".into());
    }
    if contains_any(
        n,
        &[
            "8945hs", "8845hs", "8840hs", "8840u", "8700g", "8600g", "7940hs", "7840hs", "7840u",
            "7640u", "7640hs",
        ],
    ) {
        return Some("AMD Radeon 780M".into());
    }
    if contains_any(n, &["7540u", "7545u", "7400f"]) {
        return Some("AMD Radeon 740M".into());
    }
    if contains_any(n, &["7735hs", "7735u", "7535hs", "7535u"]) {
        return Some("AMD Radeon 680M".into());
    }
    if contains_any(
        n,
        &["6900hs", "6800hs", "6800u", "6800h", "6600h", "6600hs"],
    ) {
        return Some("AMD Radeon 680M".into());
    }
    if contains_any(
        n,
        &[
            "5700g", "5600g", "5400g", "5300g", "4750g", "4650g", "4400g", "3400g", "3200g",
        ],
    ) {
        return Some("AMD Radeon Vega".into());
    }
    if n.contains("with radeon") || n.contains("radeon graphics") {
        return Some("AMD Radeon Graphics".into());
    }
    // Discrete-only Threadripper / EPYC / Ryzen without G / AI / HX often have no iGPU.
    if n.contains("ryzen")
        && (n.contains(" 7 ") || n.contains(" 5 ") || n.contains(" 9 ") || n.contains(" 3 "))
        && (n.contains('u') || n.contains('h') || n.contains("hx") || n.contains("ai"))
    {
        return Some("AMD Radeon Graphics".into());
    }
    None
}

fn infer_intel(n: &str) -> Option<String> {
    if let Some(ultra) = parse_ultra_sku(n) {
        return Some(if ultra.0 >= 200 && ultra.1 == 'v' {
            "Intel Arc Graphics".into()
        } else {
            "Intel Arc Graphics".into()
        });
    }
    if n.contains("xeon") && !n.contains("e3-") && !n.contains("e-") {
        // Most Xeon SKUs are compute-only; don't invent an iGPU.
        return None;
    }
    let Some((family, sku, suffix)) = parse_core_i(n) else {
        if n.contains("pentium") || n.contains("celeron") {
            return Some("Intel UHD Graphics".into());
        }
        if n.contains("intel") {
            return Some("Intel Graphics".into());
        }
        return None;
    };
    let g7 = sku.contains("g7") || sku.contains("g4") || suffix == 'g';
    Some(match family {
        2 => "Intel HD Graphics 3000".into(),
        3 => "Intel HD Graphics 4000".into(),
        4 | 5 => "Intel HD Graphics".into(),
        6 | 7 => "Intel HD Graphics".into(),
        8 | 9 => "Intel UHD Graphics".into(),
        10 if g7 => "Intel Iris Plus".into(),
        10 => "Intel UHD Graphics".into(),
        11..=14 if g7 || matches!(suffix, 'u' | 'p' | 'h' | 'g') => "Intel Iris Xe".into(),
        11..=14 => "Intel UHD Graphics".into(),
        _ if family >= 15 => "Intel Arc Graphics".into(),
        _ => "Intel Graphics".into(),
    })
}

fn parse_ultra_sku(n: &str) -> Option<(u16, char)> {
    // "core ultra 7 155h" / "ultra 5 226v"
    let idx = n.find("ultra")?;
    let rest = n[idx + 5..].trim_start();
    let mut parts = rest.split_whitespace();
    let _tier = parts.next()?; // 5/7/9
    let sku = parts.next().unwrap_or("");
    let digits: String = sku.chars().take_while(|c| c.is_ascii_digit()).collect();
    let num: u16 = digits.parse().ok()?;
    let suffix = sku
        .chars()
        .rev()
        .find(|c| c.is_ascii_alphabetic())
        .unwrap_or('u')
        .to_ascii_lowercase();
    Some((num, suffix))
}

fn parse_core_i(n: &str) -> Option<(u16, String, char)> {
    // i7-10610u / i7 10610u / core i5-1335u
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
            if j > start {
                let num = &n[start..j];
                if num.len() >= 4 {
                    let family = intel_family_from_digits(num)?;
                    let mut end = j;
                    while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
                        end += 1;
                    }
                    let sku = n[start..end].to_string();
                    let suffix = n[j..end]
                        .chars()
                        .next()
                        .filter(|c| c.is_ascii_alphabetic())
                        .unwrap_or(' ')
                        .to_ascii_lowercase();
                    return Some((family, sku, suffix));
                }
            }
        }
        i += 1;
    }
    None
}

fn intel_family_from_digits(num: &str) -> Option<u16> {
    if num.len() >= 5 {
        return num[..2].parse().ok();
    }
    if num.len() == 4 {
        let two: u16 = num[..2].parse().ok()?;
        if (10..=16).contains(&two) {
            return Some(two);
        }
        return num[..1].parse().ok();
    }
    None
}

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x1_comet_lake_is_uhd() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz").as_deref(),
            Some("Intel UHD Graphics")
        );
    }

    #[test]
    fn ivy_bridge_is_hd_4000() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Core i5-3320M").as_deref(),
            Some("Intel HD Graphics 4000")
        );
    }

    #[test]
    fn coffee_lake_u_is_uhd() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Core i7-8650U").as_deref(),
            Some("Intel UHD Graphics")
        );
    }

    #[test]
    fn tiger_lake_g7_is_iris_xe() {
        assert_eq!(
            infer_igpu_from_cpu("11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz").as_deref(),
            Some("Intel Iris Xe")
        );
    }

    #[test]
    fn thirteenth_gen_u_is_iris_xe() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Core(TM) i7-1355U").as_deref(),
            Some("Intel Iris Xe")
        );
    }

    #[test]
    fn core_ultra_is_arc() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Core(TM) Ultra 7 155H").as_deref(),
            Some("Intel Arc Graphics")
        );
    }

    #[test]
    fn ice_lake_g7_is_iris_plus() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Core i7-1065G7").as_deref(),
            Some("Intel Iris Plus")
        );
    }

    #[test]
    fn desktop_alder_lake_is_uhd_not_iris_xe() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Core i7-12700K").as_deref(),
            Some("Intel UHD Graphics")
        );
    }

    #[test]
    fn pentium_or_celeron_is_uhd() {
        assert_eq!(
            infer_igpu_from_cpu("Intel Pentium Gold 7505").as_deref(),
            Some("Intel UHD Graphics")
        );
        assert_eq!(
            infer_igpu_from_cpu("Intel Celeron N4500").as_deref(),
            Some("Intel UHD Graphics")
        );
    }

    #[test]
    fn ryzen_5700g_is_vega_not_generic_radeon() {
        assert_eq!(
            infer_igpu_from_cpu("AMD Ryzen 7 5700G with Radeon Graphics").as_deref(),
            Some("AMD Radeon Vega")
        );
    }

    #[test]
    fn ryzen_7840u_is_780m() {
        assert_eq!(
            infer_igpu_from_cpu("AMD Ryzen 7 7840U").as_deref(),
            Some("AMD Radeon 780M")
        );
    }

    #[test]
    fn ryzen_ai_hx_370_is_890m() {
        assert_eq!(
            infer_igpu_from_cpu("AMD Ryzen AI 9 HX 370").as_deref(),
            Some("AMD Radeon 890M")
        );
    }

    #[test]
    fn apple_m3_pro_is_m3_pro_gpu() {
        assert_eq!(
            infer_igpu_from_cpu("Apple M3 Pro").as_deref(),
            Some("Apple M3 Pro GPU")
        );
    }

    #[test]
    fn snapdragon_is_adreno() {
        assert_eq!(
            infer_igpu_from_cpu("Snapdragon X Elite").as_deref(),
            Some("Qualcomm Adreno")
        );
    }

    #[test]
    fn empty_or_unknown_cpu_infers_no_igpu() {
        assert_eq!(infer_igpu_from_cpu(""), None);
        assert_eq!(infer_igpu_from_cpu("   "), None);
        assert_eq!(infer_igpu_from_cpu("Some Random CPU"), None);
    }

    #[test]
    fn arm_mali_and_immortalis_from_cpu() {
        assert_eq!(infer_igpu_from_cpu("Mali-G78").as_deref(), Some("ARM Mali"));
        assert_eq!(
            infer_igpu_from_cpu("Immortalis-G720").as_deref(),
            Some("ARM Immortalis")
        );
    }

    #[test]
    fn discrete_amd_desktop_has_no_igpu() {
        assert_eq!(infer_igpu_from_cpu("AMD EPYC 7763"), None);
        assert_eq!(infer_igpu_from_cpu("AMD Ryzen 9 7950X"), None);
    }

    #[test]
    fn normalize_strips_trademarks() {
        for (raw, want) in [
            ("Intel(R) UHD Graphics", "Intel UHD Graphics"),
            ("AMD Radeon(TM) Graphics", "AMD Radeon Graphics"),
        ] {
            assert_eq!(normalize_gpu_name(raw), want, "{raw}");
        }
    }

    #[test]
    fn normalize_qualifies_intel_family_without_vendor() {
        for (raw, want) in [
            ("Iris Xe Graphics", "Intel Iris Xe Graphics"),
            ("UHD Graphics", "Intel UHD Graphics"),
        ] {
            assert_eq!(normalize_gpu_name(raw), want, "{raw}");
        }
    }

    #[test]
    fn normalize_trademark_only_is_empty() {
        for raw in ["(R)", "(TM)", "®"] {
            assert_eq!(normalize_gpu_name(raw), "", "{raw}");
        }
    }

    #[test]
    fn normalize_strips_corporation_prefix() {
        for (raw, want) in [
            ("NVIDIA Corporation GeForce RTX 4090", "GeForce RTX 4090"),
            ("Intel Corporation UHD Graphics", "Intel UHD Graphics"),
        ] {
            assert_eq!(normalize_gpu_name(raw), want, "{raw}");
        }
    }

    #[test]
    fn normalize_qualifies_nvidia_rtx_without_vendor() {
        let raw = "RTX 4090";
        assert_eq!(normalize_gpu_name(raw), "NVIDIA RTX 4090", "{raw}");
    }

    #[test]
    fn discrete_xeon_has_no_igpu() {
        assert_eq!(
            infer_igpu_from_cpu("Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz"),
            None
        );
    }
}
