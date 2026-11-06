use serde_json::Value;
use std::collections::VecDeque;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// None on the first sample of a stream, which has no previous CPU reading.
    pub cpu_percent: Option<f64>,
    pub memory_used: u64,
    pub memory_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub block_read: u64,
    pub block_write: u64,
}

impl Sample {
    /// Same formulas as `docker stats`: CPU over the system delta times online CPUs,
    /// memory without the inactive page cache.
    pub fn from_stats(stats: &Value) -> Self {
        let number = |value: &Value| value.as_u64().unwrap_or(0);
        let cpu = &stats["cpu_stats"];
        let precpu = &stats["precpu_stats"];
        let cpu_delta = number(&cpu["cpu_usage"]["total_usage"]) as f64 - number(&precpu["cpu_usage"]["total_usage"]) as f64;
        let system_delta = number(&cpu["system_cpu_usage"]) as f64 - number(&precpu["system_cpu_usage"]) as f64;
        let online = cpu["online_cpus"].as_u64().or_else(|| cpu["cpu_usage"]["percpu_usage"].as_array().map(|cpus| cpus.len() as u64)).unwrap_or(1) as f64;
        let cpu_percent = (precpu["system_cpu_usage"].is_u64() && system_delta > 0.0).then(|| cpu_delta.max(0.0) / system_delta * online * 100.0);

        let memory = &stats["memory_stats"];
        let inactive = memory["stats"]["inactive_file"].as_u64().or(memory["stats"]["total_inactive_file"].as_u64()).unwrap_or(0);
        let mut sample = Sample { cpu_percent, memory_used: number(&memory["usage"]).saturating_sub(inactive), memory_limit: number(&memory["limit"]), ..Sample::default() };
        for network in stats["networks"].as_object().into_iter().flat_map(|networks| networks.values()) {
            sample.net_rx += number(&network["rx_bytes"]);
            sample.net_tx += number(&network["tx_bytes"]);
        }
        for entry in stats["blkio_stats"]["io_service_bytes_recursive"].as_array().into_iter().flatten() {
            match entry["op"].as_str().map(str::to_lowercase).as_deref() {
                Some("read") => sample.block_read += number(&entry["value"]),
                Some("write") => sample.block_write += number(&entry["value"]),
                _ => {}
            }
        }
        sample
    }
}

/// Last minute of samples, one per second.
const HISTORY: usize = 60;

pub fn push(history: &mut VecDeque<Sample>, sample: Sample) {
    history.push_back(sample);
    if history.len() > HISTORY {
        history.pop_front();
    }
}

pub fn format_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

/// `45s`, `12m`, `3h`, `20d` or `2y` since the Unix time `seconds`.
pub fn age(seconds: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let elapsed = (now - seconds).max(0);
    match elapsed {
        0..60 => format!("{elapsed}s"),
        60..3600 => format!("{}m", elapsed / 60),
        3600..86400 => format!("{}h", elapsed / 3600),
        86400..31536000 => format!("{}d", elapsed / 86400),
        _ => format!("{}y", elapsed / 31536000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn computes_cpu_and_memory_like_docker_stats() {
        let stats = json!({
            "cpu_stats": {"cpu_usage": {"total_usage": 300}, "system_cpu_usage": 2000, "online_cpus": 4},
            "precpu_stats": {"cpu_usage": {"total_usage": 100}, "system_cpu_usage": 1000},
            "memory_stats": {"usage": 5000, "limit": 10000, "stats": {"inactive_file": 1000}},
            "networks": {"eth0": {"rx_bytes": 10, "tx_bytes": 20}, "eth1": {"rx_bytes": 1, "tx_bytes": 2}},
            "blkio_stats": {"io_service_bytes_recursive": [{"op": "Read", "value": 7}, {"op": "write", "value": 9}]}
        });
        let sample = Sample::from_stats(&stats);
        assert_eq!(sample.cpu_percent, Some(80.0));
        assert_eq!((sample.memory_used, sample.memory_limit), (4000, 10000));
        assert_eq!((sample.net_rx, sample.net_tx, sample.block_read, sample.block_write), (11, 22, 7, 9));
    }

    #[test]
    fn counts_percpu_usage_when_online_cpus_is_missing() {
        let stats = json!({
            "cpu_stats": {"cpu_usage": {"total_usage": 300, "percpu_usage": [1, 2, 3, 4]}, "system_cpu_usage": 2000},
            "precpu_stats": {"cpu_usage": {"total_usage": 100}, "system_cpu_usage": 1000}
        });
        assert_eq!(Sample::from_stats(&stats).cpu_percent, Some(80.0));
    }

    #[test]
    fn first_sample_has_no_cpu() {
        let stats = json!({"cpu_stats": {"cpu_usage": {"total_usage": 300}, "system_cpu_usage": 2000}, "precpu_stats": {"cpu_usage": {}}});
        assert_eq!(Sample::from_stats(&stats).cpu_percent, None);
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
