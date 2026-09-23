//! appricot-spike: the M1 spike's measuring tools (docs/spike/README.md).
//!
//! ```text
//! appricot-spike observe  --out DIR [--for SECS]
//! appricot-spike record   --out DIR --url URL [--codecs raw|qoi] [--tiles] [--script FILE]
//!                         [--snap-every SECS] [--no-resize-ask] [--for SECS]
//! appricot-spike mem      --out DIR [--every MS] [--app-prefix PATH] [--for SECS]
//! appricot-spike bench    DIR [--reps N]
//! appricot-spike fake-app [--out DIR] [--for SECS]
//! ```
//!
//! `record` reads the stream token from `APPRICOT_STREAM_TOKEN`, never from the command line:
//! a secret on argv is readable by every process on the machine. Each tool stops at the run's
//! `control/stop` file (or after `--for`) and writes its Markdown summary into DIR.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use appricot_spike::control::Control;
use appricot_spike::{bench, fake_app, mem, observe, record, script, tiles};

fn main() -> ExitCode {
    let mut args: VecDeque<String> = std::env::args().skip(1).collect();
    let Some(command) = args.pop_front() else {
        return usage();
    };
    let result = match command.as_str() {
        "observe" => cmd_observe(args),
        "record" => cmd_record(args),
        "mem" => cmd_mem(args),
        "bench" => cmd_bench(args),
        "fake-app" => cmd_fake_app(args),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            say(&format!("appricot-spike {command}: {e}"));
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

fn usage() -> ExitCode {
    say(
        "usage: appricot-spike observe|record|mem|bench|fake-app ... \
         (see crates/appricot-spike/src/main.rs)",
    );
    ExitCode::from(2)
}

type CmdResult = Result<(), Box<dyn std::error::Error>>;

/// The flags of one command: `--name value` pairs and bare `--name` switches.
struct Flags {
    args: VecDeque<String>,
}

impl Flags {
    fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let Some(at) = self.args.iter().position(|a| a == name) else {
            return Ok(None);
        };
        self.args.remove(at);
        self.args
            .remove(at)
            .map(Some)
            .ok_or_else(|| format!("{name} needs a value"))
    }

    fn switch(&mut self, name: &str) -> bool {
        let Some(at) = self.args.iter().position(|a| a == name) else {
            return false;
        };
        self.args.remove(at);
        true
    }

    fn secs(&mut self, name: &str) -> Result<Option<Duration>, String> {
        self.value(name)?
            .map(|v| {
                v.parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .map(Duration::from_secs_f64)
                    .ok_or_else(|| format!("{name} needs a positive number of seconds"))
            })
            .transpose()
    }

    fn out_dir(&mut self) -> Result<PathBuf, String> {
        let dir = PathBuf::from(self.value("--out")?.ok_or("--out DIR is required")?);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(dir)
    }

    fn done(self) -> Result<(), String> {
        match self.args.front() {
            None => Ok(()),
            Some(extra) => Err(format!("unexpected argument `{extra}`")),
        }
    }
}

fn cmd_observe(args: VecDeque<String>) -> CmdResult {
    let mut flags = Flags { args };
    let out_dir = flags.out_dir()?;
    let duration = flags.secs("--for")?;
    flags.done()?;
    let control = Control::open(&out_dir)?;
    let windows = observe::run(
        &observe::ObserveConfig {
            out_dir,
            display: None,
            duration,
        },
        &control,
    )?;
    say(&format!(
        "observe: {} windows in the inventory",
        windows.len()
    ));
    Ok(())
}

fn cmd_record(args: VecDeque<String>) -> CmdResult {
    let mut flags = Flags { args };
    let out_dir = flags.out_dir()?;
    let url = flags.value("--url")?.ok_or("--url is required")?;
    let codecs = match flags.value("--codecs")?.as_deref() {
        None | Some("qoi") => vec![appricot_proto::limits::codec::QOI],
        Some("raw") => Vec::new(),
        Some(other) => return Err(format!("unknown codec set `{other}`").into()),
    };
    let save_tiles = flags.switch("--tiles");
    let honour_resize_ask = !flags.switch("--no-resize-ask");
    let script = match flags.value("--script")? {
        Some(path) => Some(script::parse(&std::fs::read_to_string(&path)?)?),
        None => None,
    };
    let snapshot_every = flags.secs("--snap-every")?;
    let duration = flags.secs("--for")?;
    flags.done()?;
    let token = std::env::var("APPRICOT_STREAM_TOKEN")
        .map_err(|_| "APPRICOT_STREAM_TOKEN is not set")?
        .into_bytes();
    let control = Control::open(&out_dir)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let summary = runtime.block_on(record::run(
        record::RecordConfig {
            out_dir,
            url,
            token,
            codecs,
            save_tiles,
            script,
            snapshot_every,
            duration,
            honour_resize_ask,
        },
        control,
    ))?;
    say(&format!(
        "record: {} toplevels, {} popups, {} frames, {} tiles, {} snapshots",
        summary.toplevels,
        summary.popups,
        summary.frames,
        summary.tiles,
        summary.snapshots.len()
    ));
    Ok(())
}

fn cmd_mem(args: VecDeque<String>) -> CmdResult {
    let mut flags = Flags { args };
    let out_dir = flags.out_dir()?;
    let every = match flags.value("--every")? {
        Some(ms) => Duration::from_millis(ms.parse().map_err(|_| "--every needs milliseconds")?),
        None => Duration::from_secs(1),
    };
    let app_prefix = flags
        .value("--app-prefix")?
        .unwrap_or_else(|| "/pilot/".into());
    let duration = flags.secs("--for")?;
    flags.done()?;
    let control = Control::open(&out_dir)?;
    mem::run(
        &mem::MemConfig {
            out_dir,
            every,
            app_prefix,
            duration,
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
        },
        &control,
    )?;
    Ok(())
}

fn cmd_bench(mut args: VecDeque<String>) -> CmdResult {
    let dir = PathBuf::from(args.pop_front().ok_or("bench needs the run directory")?);
    let mut flags = Flags { args };
    let reps = match flags.value("--reps")? {
        Some(n) => n.parse().map_err(|_| "--reps needs a number")?,
        None => bench::REPS,
    };
    flags.done()?;
    let records = tiles::read_all(&dir.join("tiles.bin"))?;
    let rows = bench::run(&records, reps)?;
    let table = format!("## Codec bench\n\n{}", bench::markdown(&rows));
    std::fs::write(dir.join("bench.md"), &table)?;
    let _ = std::io::stdout().lock().write_all(table.as_bytes());
    Ok(())
}

fn cmd_fake_app(args: VecDeque<String>) -> CmdResult {
    let mut flags = Flags { args };
    let control = match flags.value("--out")? {
        Some(dir) => Some(Control::open(&PathBuf::from(dir))?),
        None => None,
    };
    let duration = flags.secs("--for")?;
    flags.done()?;
    fake_app::run(
        &fake_app::FakeAppConfig {
            display: None,
            duration,
            print_keys: true,
        },
        control.as_ref(),
        |_| {},
    )?;
    Ok(())
}
