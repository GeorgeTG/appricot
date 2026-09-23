//! The spike's tools against a real X server and the real streamer, end to end.
//!
//! The pilot application is not in this repository, so these prove the harness on the stand-in
//! ([`appricot_spike::fake_app`]), which does on a timeline what the spike measures: a scrolling
//! table, an override-redirect popup, a transient dialog. What they pin:
//!
//! 1. the observer records each of those windows with the properties the inventory needs, and
//!    damage for them;
//! 2. the recorder, as a host would, opens a session on the in-process streamer around the real
//!    `X11Backend`, sees the surfaces, types through XTEST into the window it focused (the app
//!    reports the keysyms it received), writes snapshots that are PNGs, and records tiles the
//!    bench re-encodes.
//!
//! Like the other X11 tests, these need the container's X server and panic rather than skip
//! without one. The streamer's backend is the display's one window manager, and the observer
//! sees every window on the display, so the tests take one lock for their whole run.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use appricot_spike::control::Control;
use appricot_spike::fake_app::{self, FakeAppConfig, FakeWindows, KeyReport};
use appricot_spike::observe::{self, ObserveConfig, WindowRecord};
use appricot_spike::record::{self, RecordConfig};
use appricot_spike::{bench, script, tiles};
use appricot_streamer::backend::BackendHandle;
use appricot_streamer::server::{self, ServerState};
use appricot_x11::X11Backend;

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";

/// The token the in-process streamer is started with.
const TOKEN: &[u8] = b"spike-test-token";

/// The longest any one wait here may take.
const WAIT: Duration = Duration::from_secs(20);

/// One test at a time on the display. A `tokio` mutex, because the recorder test holds the
/// guard across its awaits.
static DISPLAY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A fresh, empty run directory under the system's temporary directory.
fn run_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("appricot-spike-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the run directory is created");
    dir
}

fn assert_display() {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(!display.is_empty(), "DISPLAY is not set; {RUN_HINT}");
}

/// Starts the stand-in application on a thread; it stops at `control`'s stop file.
fn spawn_fake_app(
    control: Control,
    keys: Arc<Mutex<Vec<KeyReport>>>,
) -> std::thread::JoinHandle<FakeWindows> {
    std::thread::spawn(move || {
        let cfg = FakeAppConfig {
            display: None,
            duration: Some(Duration::from_secs(60)),
            print_keys: false,
        };
        fake_app::run(&cfg, Some(&control), |key| {
            keys.lock().expect("the key log is not poisoned").push(key);
        })
        .unwrap_or_else(|e| panic!("the fake app runs: {e}; {RUN_HINT}"))
    })
}

fn find(windows: &[WindowRecord], id: u32) -> &WindowRecord {
    windows
        .iter()
        .find(|w| w.id == id)
        .unwrap_or_else(|| panic!("window {id:#x} is in the inventory: {windows:#?}"))
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} reads: {e}", path.display()))
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_observer_records_every_kind_of_window_and_its_damage() {
    let _display = DISPLAY.blocking_lock();
    assert_display();
    let dir = run_dir("observe");
    let control = Control::open(&dir).expect("the control directory is created");

    let observer = {
        let control = control.clone();
        let cfg = ObserveConfig {
            out_dir: dir.clone(),
            display: None,
            duration: Some(WAIT),
        };
        std::thread::spawn(move || observe::run(&cfg, &control))
    };
    // The observer selects its events before the application maps anything; the inventory's
    // first line says it is listening.
    let started = Instant::now();
    while !std::fs::read_to_string(dir.join("inventory.jsonl"))
        .is_ok_and(|s| s.contains("\"ev\":\"start\""))
    {
        assert!(started.elapsed() < WAIT, "the observer starts");
        std::thread::sleep(Duration::from_millis(20));
    }

    let app_control = Control::open(&dir.join("app")).expect("the app's control directory");
    let app = spawn_fake_app(app_control.clone(), Arc::default());
    control.set_mark("scroll").expect("the mark is set");
    // The dialog maps at 2.5 s; leave the table a few more scrolls after it.
    std::thread::sleep(Duration::from_millis(3500));
    app_control.request_stop().expect("the app is stopped");
    let made = app.join().expect("the fake app does not panic");
    control.request_stop().expect("the observer is stopped");
    let windows = observer
        .join()
        .expect("the observer does not panic")
        .expect("the observer runs");

    let toplevel = find(&windows, made.toplevel);
    assert_eq!(toplevel.class, "fake-app.FakeApp");
    assert_eq!(toplevel.title, "Fake app: table");
    assert_eq!(toplevel.types, ["_NET_WM_WINDOW_TYPE_NORMAL"]);
    assert!(!toplevel.override_redirect);
    assert_eq!(toplevel.maps, 1);

    let popup = find(&windows, made.popup);
    assert!(popup.override_redirect, "the popup is override-redirect");
    assert_eq!(popup.maps, 1);

    let dialog = find(&windows, made.dialog);
    assert_eq!(dialog.transient_for, Some(made.toplevel));
    assert_eq!(dialog.types, ["_NET_WM_WINDOW_TYPE_DIALOG"]);
    assert_eq!(dialog.motif, Some([2, 0, 0, 0, 0]));

    let damage = lines(&dir.join("damage.jsonl"));
    let on_toplevel = format!("\"window\":{}", made.toplevel);
    assert!(
        damage
            .iter()
            .any(|l| l.contains(&on_toplevel) && l.contains("\"label\":\"scroll\"")),
        "the table's repaints are logged as damage under the label in force"
    );
    let summary = std::fs::read_to_string(dir.join("inventory.md")).expect("inventory.md");
    assert!(summary.contains("fake-app.FakeApp"), "{summary}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Connects the real backend. A previous backend may hold the display for a moment after its
/// connection closed, so a takeover conflict is retried.
async fn x11_backend() -> X11Backend {
    let deadline = Instant::now() + WAIT;
    loop {
        match X11Backend::connect(None) {
            Ok(backend) => return backend,
            Err(e) if e.is_takeover_conflict() && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("the backend connects to the X server: {e}; {RUN_HINT}"),
        }
    }
}

/// Serves the real streamer around a fresh `X11Backend` on an ephemeral loopback port.
async fn serve() -> (Arc<ServerState<X11Backend>>, std::net::SocketAddr) {
    let state = ServerState::new(TOKEN.to_vec(), BackendHandle::spawn(x11_backend().await));
    state.set_ready();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("an ephemeral loopback port is free");
    let addr = listener.local_addr().expect("the listener has an address");
    let router = server::router::<X11Backend>().with_state(Arc::clone(&state));
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("the server serves");
    });
    (state, addr)
}

/// The recorder's script. Toplevel 2 is the dialog, which maps at 2.5 s: waiting for it, not
/// for time, keeps the script deterministic. The popup (1 s to 2 s) is mapped long enough to be
/// announced on its own.
const SCRIPT: &str = "wait-for toplevel 1 15000
focus 1
wait 300
mark typing
type ab
wait 300
snap typed
wait-for toplevel 2 15000
mark dialog
wait 500
stop
";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_recorder_drives_the_real_streamer_and_records_what_it_sends() {
    let _display = DISPLAY.lock().await;
    assert_display();
    let dir = run_dir("record");
    let control = Control::open(&dir).expect("the control directory is created");
    let (state, addr) = serve().await;

    let keys: Arc<Mutex<Vec<KeyReport>>> = Arc::default();
    let app_control = Control::open(&dir.join("app")).expect("the app's control directory");
    let app = spawn_fake_app(app_control.clone(), Arc::clone(&keys));

    let summary = tokio::time::timeout(
        WAIT + WAIT,
        record::run(
            RecordConfig {
                out_dir: dir.clone(),
                url: format!("ws://{addr}/session"),
                token: TOKEN.to_vec(),
                codecs: vec![appricot_proto::limits::codec::QOI],
                save_tiles: true,
                script: Some(script::parse(SCRIPT).expect("the script parses")),
                snapshot_every: None,
                duration: Some(WAIT + WAIT),
                honour_resize_ask: true,
            },
            control,
        ),
    )
    .await
    .expect("the recording ends by its script")
    .expect("the recording runs");

    app_control.request_stop().expect("the app is stopped");
    tokio::task::spawn_blocking(move || app.join())
        .await
        .expect("the join runs")
        .expect("the fake app does not panic");
    state.shutdown();
    tokio::time::timeout(WAIT, state.until_over())
        .await
        .expect("the session ends when the server stops");

    assert!(summary.bye.is_none(), "the server did not end the session");
    assert!(
        summary.toplevels >= 2,
        "the table and the dialog: {summary:?}"
    );
    assert!(summary.popups >= 1, "the popup: {summary:?}");
    assert!(summary.frames > 0 && summary.tiles > 0, "{summary:?}");

    // What reached the app: `a` then `b`, typed into the toplevel the script focused.
    let pressed: Vec<u32> = keys
        .lock()
        .expect("the key log is not poisoned")
        .iter()
        .filter(|k| k.pressed)
        .map(|k| k.keysym)
        .collect();
    assert_eq!(pressed, [0x61, 0x62], "the keysyms the app received");

    check_snapshots(&summary.snapshots);
    check_logs(&dir);

    let recorded = tiles::read_all(&dir.join("tiles.bin")).expect("tiles.bin reads");
    assert_eq!(
        u64::try_from(recorded.len()).expect("a count"),
        summary.tiles,
        "every tile received is recorded"
    );
    let rows = bench::run(&recorded, 1).expect("the bench runs");
    assert!(
        rows.iter().any(|r| r.label == "all"),
        "the bench has a total row"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The script's snapshot and the final one were taken, and each is a PNG.
fn check_snapshots(snapshots: &[PathBuf]) {
    let names: Vec<String> = snapshots
        .iter()
        .filter_map(|p| p.file_name()?.to_str().map(str::to_owned))
        .collect();
    assert!(
        names.iter().any(|n| n.starts_with("typed-s"))
            && names.iter().any(|n| n.starts_with("final-s")),
        "the snapshots: {names:?}"
    );
    for path in snapshots {
        let png = std::fs::read(path).expect("the snapshot reads");
        assert_eq!(
            &png[..8],
            b"\x89PNG\r\n\x1a\n",
            "{} is a PNG",
            path.display()
        );
    }
}

/// The wire log names the popup and the table's title, and the summary has a row per label.
fn check_logs(dir: &Path) {
    let wire = std::fs::read_to_string(dir.join("wire.md")).expect("wire.md");
    for label in ["| start |", "| typing |", "| dialog |"] {
        assert!(wire.contains(label), "wire.md has a row {label}:\n{wire}");
    }
    let log = lines(&dir.join("wire.jsonl"));
    assert!(
        log.iter()
            .any(|l| l.contains("\"ev\":\"surface_new\"") && l.contains("\"role\":\"popup\"")),
        "a popup surface is logged"
    );
    assert!(
        log.iter()
            .any(|l| l.contains("\"title\":\"Fake app: table\"")),
        "the table's title is logged"
    );
}
