//! The memory and CPU sampler: per process from `/proc`, per container from the cgroup.
//!
//! The spike's memory question is what a session costs **with the streamer in it**. So every
//! sample reads, for each process in the container's PID namespace:
//!
//! - from `/proc/<pid>/smaps_rollup`: RSS, PSS, and the anonymous and shared-memory parts of
//!   the PSS. PSS splits shared pages between the processes that map them, so the PSS of all
//!   processes adds up to what they cost together, where RSS would count a shared library once
//!   per process;
//! - from `/proc/<pid>/stat`: user and system CPU time, in clock ticks (`USER_HZ`, 100 on
//!   Linux for every architecture this runs on).
//!
//! and, for the whole container, from cgroup v2 (`/sys/fs/cgroup`): `memory.current`,
//! `memory.peak` where the kernel has it, `anon`, `file` and `shmem` of `memory.stat`, and
//! `usage_usec` of `cpu.stat`. The field meanings are the kernel's
//! (<https://docs.kernel.org/admin-guide/cgroup-v2.html> and
//! <https://docs.kernel.org/filesystems/proc.html>, checked 2026-09-23).
//!
//! Processes are grouped by what they are: the X server, the streamer, the application (its
//! executable under `--app-prefix`), this tool, and everything else. The summary reports each
//! group per scenario label. The tool's own processes are in the cgroup too; their group is
//! reported so it can be subtracted.
//!
//! The cgroup figure a session is compared by is **anon + shmem**: the memory its processes
//! own. `memory.current` and `memory.peak` also count the page cache, which the release build
//! just before a run fills, so they are logged but not taken as the session's cost.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::control::Control;
use crate::json::{JsonObject, array};
use crate::stats::{Summary, mib, u64_to_f64};

/// Clock ticks per second of `/proc/<pid>/stat` times: `USER_HZ`, fixed at 100 by the Linux ABI
/// on x86 and arm.
pub const USER_HZ: u64 = 100;

/// What one process costs, from one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcSample {
    /// Its pid.
    pub pid: u32,
    /// Its group (see [`classify`]).
    pub group: &'static str,
    /// The basename of its executable, from `argv[0]`.
    pub name: String,
    /// Resident set, bytes.
    pub rss: u64,
    /// Proportional set, bytes.
    pub pss: u64,
    /// The anonymous part of the PSS, bytes.
    pub pss_anon: u64,
    /// The shared-memory part of the PSS, bytes.
    pub pss_shmem: u64,
    /// User plus system CPU time so far, clock ticks.
    pub cpu_ticks: u64,
}

/// The container's cgroup, from one sample.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupSample {
    /// `memory.current`, bytes.
    pub current: Option<u64>,
    /// `memory.peak`, bytes, where the kernel provides it.
    pub peak: Option<u64>,
    /// `anon` of `memory.stat`, bytes.
    pub anon: Option<u64>,
    /// `file` of `memory.stat`, bytes.
    pub file: Option<u64>,
    /// `shmem` of `memory.stat`, bytes.
    pub shmem: Option<u64>,
    /// `usage_usec` of `cpu.stat`.
    pub cpu_usec: Option<u64>,
}

/// Parses `smaps_rollup`: (rss, pss, pss_anon, pss_shmem) in bytes. Missing fields are 0.
pub fn parse_smaps_rollup(text: &str) -> (u64, u64, u64, u64) {
    let mut out = (0, 0, 0, 0);
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(kib) = value.parse::<u64>() else {
            continue;
        };
        let bytes = kib * 1024;
        match key {
            "Rss:" => out.0 = bytes,
            "Pss:" => out.1 = bytes,
            "Pss_Anon:" => out.2 = bytes,
            "Pss_Shmem:" => out.3 = bytes,
            _ => {}
        }
    }
    out
}

/// Parses `/proc/<pid>/stat` for utime + stime, in clock ticks. The command name is in
/// parentheses and may itself hold spaces or parentheses, so fields are counted from the last
/// `)`: utime and stime are fields 14 and 15 of the whole line.
pub fn parse_stat_cpu_ticks(text: &str) -> Option<u64> {
    let after = &text[text.rfind(')')? + 1..];
    let mut fields = after.split_whitespace();
    // After the name: state is field 3, so utime (14) is the 12th field from here.
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// Reads one `key value` line out of a flat-keyed cgroup file.
pub fn flat_key(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next()? == key)
            .then(|| parts.next()?.parse().ok())
            .flatten()
    })
}

/// The group of a process, from the basename of its `argv[0]`, the full `argv[0]`, and the
/// application's install prefix.
pub fn classify(name: &str, argv0: &str, app_prefix: &str) -> &'static str {
    if !app_prefix.is_empty() && argv0.starts_with(app_prefix) {
        "app"
    } else if name == "Xvfb" || name == "Xorg" {
        "x-server"
    } else if name == "appricot-streamer" {
        "streamer"
    } else if name == "appricot-spike" {
        "spike-tools"
    } else {
        "other"
    }
}

fn read_proc(pid: u32, app_prefix: &str) -> Option<ProcSample> {
    let base = PathBuf::from(format!("/proc/{pid}"));
    let cmdline = fs::read(base.join("cmdline")).ok()?;
    let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
    let argv0 = String::from_utf8_lossy(argv0).into_owned();
    // Kernel threads have an empty command line; they are not the container's.
    if argv0.is_empty() {
        return None;
    }
    let name = argv0.rsplit('/').next().unwrap_or(&argv0).to_owned();
    let (rss, pss, pss_anon, pss_shmem) =
        parse_smaps_rollup(&fs::read_to_string(base.join("smaps_rollup")).unwrap_or_default());
    let cpu_ticks = fs::read_to_string(base.join("stat"))
        .ok()
        .and_then(|s| parse_stat_cpu_ticks(&s))
        .unwrap_or(0);
    Some(ProcSample {
        pid,
        group: classify(&name, &argv0, app_prefix),
        name,
        rss,
        pss,
        pss_anon,
        pss_shmem,
        cpu_ticks,
    })
}

/// Samples every process of the PID namespace.
pub fn sample_procs(app_prefix: &str) -> Vec<ProcSample> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out: Vec<ProcSample> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| read_proc(pid, app_prefix))
        .collect();
    out.sort_by_key(|p| p.pid);
    out
}

/// Samples the cgroup mounted at `root` (normally `/sys/fs/cgroup`).
pub fn sample_cgroup(root: &Path) -> CgroupSample {
    let read = |name: &str| fs::read_to_string(root.join(name)).ok();
    let single = |name: &str| read(name).and_then(|s| s.trim().parse().ok());
    let stat = read("memory.stat").unwrap_or_default();
    let cpu = read("cpu.stat").unwrap_or_default();
    CgroupSample {
        current: single("memory.current"),
        peak: single("memory.peak"),
        anon: flat_key(&stat, "anon"),
        file: flat_key(&stat, "file"),
        shmem: flat_key(&stat, "shmem"),
        cpu_usec: flat_key(&cpu, "usage_usec"),
    }
}

/// How the sampler runs.
#[derive(Debug, Clone)]
pub struct MemConfig {
    /// The run directory: `mem.jsonl` and `mem.md` go here.
    pub out_dir: PathBuf,
    /// Between two samples.
    pub every: Duration,
    /// The application's install prefix (`/pilot/`), for [`classify`].
    pub app_prefix: String,
    /// Stop after this long, when set; otherwise at the `stop` control file.
    pub duration: Option<Duration>,
    /// The cgroup root.
    pub cgroup_root: PathBuf,
}

/// Per group and label: the samples of summed PSS, summed RSS and CPU percent.
#[derive(Debug, Default)]
struct GroupSeries {
    pss: Vec<f64>,
    rss: Vec<f64>,
    cpu_pct: Vec<f64>,
}

/// Runs the sampler until stopped, then writes `mem.md`. Returns the summary text.
pub fn run(cfg: &MemConfig, control: &Control) -> io::Result<String> {
    let mut log = io::BufWriter::new(fs::File::create(cfg.out_dir.join("mem.jsonl"))?);
    let started = Instant::now();
    let mut series: BTreeMap<(String, &'static str), GroupSeries> = BTreeMap::new();
    let mut cgroup_series: BTreeMap<String, Vec<(f64, f64, f64)>> = BTreeMap::new();
    let mut labels: Vec<String> = Vec::new();
    let mut last_ticks: BTreeMap<u32, (u64, Instant)> = BTreeMap::new();
    let mut last_cgroup_cpu: Option<(u64, Instant)> = None;
    let mut cgroup_cpu_pct: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut owned_max: Option<u64> = None;

    loop {
        let label = control.mark();
        if !labels.contains(&label) {
            labels.push(label.clone());
        }
        let now = Instant::now();
        let procs = sample_procs(&cfg.app_prefix);
        let cgroup = sample_cgroup(&cfg.cgroup_root);
        if let (Some(anon), Some(shmem)) = (cgroup.anon, cgroup.shmem) {
            owned_max = owned_max.max(Some(anon + shmem));
        }

        // Per-group sums for this sample, and CPU percent from the tick deltas.
        let mut sums: BTreeMap<&'static str, (u64, u64, f64)> = BTreeMap::new();
        for p in &procs {
            let entry = sums.entry(p.group).or_default();
            entry.0 += p.pss;
            entry.1 += p.rss;
            if let Some((ticks, at)) = last_ticks.get(&p.pid) {
                let dt = now.duration_since(*at).as_secs_f64();
                if dt > 0.0 && p.cpu_ticks >= *ticks {
                    let cpu_s = u64_to_f64(p.cpu_ticks - ticks) / u64_to_f64(USER_HZ);
                    entry.2 += 100.0 * cpu_s / dt;
                }
            }
        }
        last_ticks = procs.iter().map(|p| (p.pid, (p.cpu_ticks, now))).collect();
        for (group, (pss, rss, cpu)) in &sums {
            let s = series.entry((label.clone(), group)).or_default();
            s.pss.push(u64_to_f64(*pss));
            s.rss.push(u64_to_f64(*rss));
            s.cpu_pct.push(*cpu);
        }
        if let (Some(usec), Some((prev, at))) = (cgroup.cpu_usec, last_cgroup_cpu) {
            let dt = now.duration_since(at).as_secs_f64();
            if dt > 0.0 && usec >= prev {
                cgroup_cpu_pct
                    .entry(label.clone())
                    .or_default()
                    .push(100.0 * u64_to_f64(usec - prev) / 1e6 / dt);
            }
        }
        last_cgroup_cpu = cgroup.cpu_usec.map(|u| (u, now));
        cgroup_series.entry(label.clone()).or_default().push((
            u64_to_f64(cgroup.current.unwrap_or(0)),
            u64_to_f64(cgroup.anon.unwrap_or(0)),
            u64_to_f64(cgroup.shmem.unwrap_or(0)),
        ));

        writeln!(log, "{}", sample_line(&label, started, &procs, &cgroup))?;
        log.flush()?;

        if control.stop_requested() || cfg.duration.is_some_and(|d| started.elapsed() >= d) {
            break;
        }
        sleep_unless_stopped(cfg.every, control);
    }

    let summary = summarize(&labels, &series, &cgroup_series, &cgroup_cpu_pct, owned_max);
    fs::write(cfg.out_dir.join("mem.md"), &summary)?;
    Ok(summary)
}

fn sleep_unless_stopped(total: Duration, control: &Control) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if control.stop_requested() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50).min(total));
    }
}

/// Wall-clock microseconds since the Unix epoch: comparable across the run's processes.
pub fn wall_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

fn sample_line(
    label: &str,
    started: Instant,
    procs: &[ProcSample],
    cgroup: &CgroupSample,
) -> String {
    let procs_json = array(procs.iter().map(|p| {
        JsonObject::new()
            .uint("pid", u64::from(p.pid))
            .str("group", p.group)
            .str("name", &p.name)
            .uint("rss", p.rss)
            .uint("pss", p.pss)
            .uint("pss_anon", p.pss_anon)
            .uint("pss_shmem", p.pss_shmem)
            .uint("cpu_ticks", p.cpu_ticks)
            .finish()
    }));
    let cgroup_json = JsonObject::new()
        .opt_uint("current", cgroup.current)
        .opt_uint("peak", cgroup.peak)
        .opt_uint("anon", cgroup.anon)
        .opt_uint("file", cgroup.file)
        .opt_uint("shmem", cgroup.shmem)
        .opt_uint("cpu_usec", cgroup.cpu_usec)
        .finish();
    JsonObject::new()
        .uint("t_us", wall_us())
        .uint(
            "t_ms",
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        )
        .str("label", label)
        .raw("cgroup", &cgroup_json)
        .raw("procs", &procs_json)
        .finish()
}

fn summarize(
    labels: &[String],
    series: &BTreeMap<(String, &'static str), GroupSeries>,
    cgroup: &BTreeMap<String, Vec<(f64, f64, f64)>>,
    cgroup_cpu: &BTreeMap<String, Vec<f64>>,
    owned_max: Option<u64>,
) -> String {
    let mut out = String::from(
        "## Memory and CPU\n\n\
         Per scenario and process group: summed PSS and RSS (MiB, mean and max over the \
         samples) and CPU (percent of one core, mean and max).\n\n\
         | Scenario | Group | PSS mean | PSS max | RSS mean | RSS max | CPU % mean | CPU % max |\n\
         |---|---|---:|---:|---:|---:|---:|---:|\n",
    );
    for label in labels {
        for ((l, group), s) in series {
            if l != label {
                continue;
            }
            let pss = Summary::of(&s.pss);
            let rss = Summary::of(&s.rss);
            let cpu = Summary::of(&s.cpu_pct);
            let mean_max = |x: Option<Summary>, scale: fn(f64) -> f64| {
                x.map_or((0.0, 0.0), |x| (scale(x.mean()), scale(x.max)))
            };
            let to_mib = |v: f64| v / (1024.0 * 1024.0);
            let (pss_mean, pss_max) = mean_max(pss, to_mib);
            let (rss_mean, rss_max) = mean_max(rss, to_mib);
            let (cpu_mean, cpu_max) = mean_max(cpu, |v| v);
            let _ = writeln!(
                out,
                "| {label} | {group} | {pss_mean:.1} | {pss_max:.1} | {rss_mean:.1} | {rss_max:.1} | {cpu_mean:.1} | {cpu_max:.1} |"
            );
        }
    }
    out.push_str(
        "\n### The container's cgroup\n\n\
         MiB. anon + shmem is what the session's processes own; memory.current adds the page \
         cache, which the build before the run fills.\n\n\
         | Scenario | anon + shmem mean | max | anon max | shmem max | memory.current max | CPU % mean |\n\
         |---|---:|---:|---:|---:|---:|---:|\n",
    );
    for label in labels {
        let Some(samples) = cgroup.get(label) else {
            continue;
        };
        let owned: Vec<f64> = samples.iter().map(|s| s.1 + s.2).collect();
        let current_max = samples.iter().map(|s| s.0).fold(0.0, f64::max);
        let anon_max = samples.iter().map(|s| s.1).fold(0.0, f64::max);
        let shmem_max = samples.iter().map(|s| s.2).fold(0.0, f64::max);
        let own = Summary::of(&owned);
        let cpu = cgroup_cpu.get(label).and_then(|v| Summary::of(v));
        let to_mib = |v: f64| v / (1024.0 * 1024.0);
        let _ = writeln!(
            out,
            "| {label} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
            own.map_or(0.0, |c| to_mib(c.mean())),
            own.map_or(0.0, |c| to_mib(c.max)),
            to_mib(anon_max),
            to_mib(shmem_max),
            to_mib(current_max),
            cpu.map_or(0.0, |c| c.mean()),
        );
    }
    if let Some(owned) = owned_max {
        let _ = writeln!(
            out,
            "\nHighest anon + shmem sampled over the run: {:.1} MiB.",
            mib(owned)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{classify, flat_key, parse_smaps_rollup, parse_stat_cpu_ticks};

    #[test]
    fn smaps_rollup_fields_are_read_in_bytes() {
        let text = "55d2c0e3a000-7ffd4e1f6000 ---p 00000000 00:00 0    [rollup]\n\
                    Rss:              204800 kB\n\
                    Pss:              150016 kB\n\
                    Pss_Dirty:         90000 kB\n\
                    Pss_Anon:          88000 kB\n\
                    Pss_File:          52000 kB\n\
                    Pss_Shmem:         10016 kB\n";
        assert_eq!(
            parse_smaps_rollup(text),
            (204_800 * 1024, 150_016 * 1024, 88_000 * 1024, 10_016 * 1024)
        );
        assert_eq!(parse_smaps_rollup(""), (0, 0, 0, 0));
    }

    #[test]
    fn stat_times_are_counted_from_the_last_parenthesis() {
        // pid (comm) state ppid pgrp session tty tpgid flags minflt cminflt majflt cmajflt
        // utime stime ...
        let text = "4242 (Web (Content) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 250 50 0 0 20 0";
        assert_eq!(parse_stat_cpu_ticks(text), Some(300));
        assert_eq!(parse_stat_cpu_ticks("garbage"), None);
    }

    #[test]
    fn cgroup_flat_keys_are_found_by_name() {
        let stat = "anon 1048576\nfile 2097152\nshmem 4096\nanon_thp 0\n";
        assert_eq!(flat_key(stat, "anon"), Some(1_048_576));
        assert_eq!(flat_key(stat, "shmem"), Some(4096));
        assert_eq!(flat_key(stat, "sock"), None);
    }

    #[test]
    fn processes_are_grouped_by_what_they_are() {
        assert_eq!(classify("app", "/pilot/app", "/pilot/"), "app");
        assert_eq!(classify("Xvfb", "Xvfb", "/pilot/"), "x-server");
        assert_eq!(
            classify(
                "appricot-streamer",
                "/target/release/appricot-streamer",
                "/pilot/"
            ),
            "streamer"
        );
        assert_eq!(
            classify("appricot-spike", "appricot-spike", "/pilot/"),
            "spike-tools"
        );
        assert_eq!(classify("bash", "/bin/bash", "/pilot/"), "other");
        assert_eq!(classify("bash", "/pilot/x", ""), "other");
    }
}
