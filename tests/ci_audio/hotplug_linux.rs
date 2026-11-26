//! Linux evidence for native default-sink following, explicit-device binding,
//! and recovery-budget exhaustion after daemon loss (issues #94 and #102).
//!
//! `RSAC_HOTPLUG_LINUX=1` enables the three sink tests. Outside `CI=true`,
//! they also require `RSAC_HOTPLUG_ALLOW_DEFAULT_CHANGE=1`: `wpctl set-default`
//! pins a configured default sink. Cleanup restores the original sink by
//! name, but leaves it pinned even if WirePlumber originally chose it
//! automatically.
//!
//! `RSAC_HOTPLUG_LINUX_DAEMON_STOP=1` enables the destructive test. Outside
//! `CI=true`, it also requires `RSAC_HOTPLUG_ALLOW_DAEMON_KILL=1`. It stops
//! every PipeWire daemon of the current user, not just the fixture's daemon,
//! and does not restart them. Run it alone and last in a disposable session.

#![cfg(all(
    target_os = "linux",
    feature = "feat_linux",
    feature = "hotplug-recovery"
))]

use std::cell::RefCell;
use std::fmt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use rsac::{
    get_device_enumerator, AudioCaptureBuilder, AudioError, AutoRecoveryConfig, CaptureTarget,
    DeviceId, RecoveryEvent,
};
use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};

use crate::helpers::init_test_logging;
use crate::hotplug_common::{
    enabled, evidence_line, fmt_at, is_exhausted, machine_lock, variant_name, Checkpoint, Machine,
    Session, Streak, Verdict, CAPTURE_CHANNELS, CAPTURE_RATE, POLL_INTERVAL, TONE_AMPLITUDE,
    TONE_A_HZ, TONE_B_HZ,
};

const SWITCH_VAR: &str = "RSAC_HOTPLUG_LINUX";
const DAEMON_STOP_VAR: &str = "RSAC_HOTPLUG_LINUX_DAEMON_STOP";
const ALLOW_DEFAULT_CHANGE_VAR: &str = "RSAC_HOTPLUG_ALLOW_DEFAULT_CHANGE";
const ALLOW_DAEMON_KILL_VAR: &str = "RSAC_HOTPLUG_ALLOW_DAEMON_KILL";
const A: &str = "rsac_hp_a";
const B: &str = "rsac_hp_b";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const READBACK_INTERVAL: Duration = Duration::from_millis(100);
const PHASE_TIMEOUT: Duration = Duration::from_secs(15);
const EXHAUST_TIMEOUT: Duration = Duration::from_secs(60);
const RETRY_BUDGET: u32 = 3;
const FIRST_BACKOFF: Duration = Duration::from_millis(200);
const RESTORE_ATTEMPTS: usize = 3;

// Capture to files, not unread pipes: neither pw-dump nor a long-lived player
// may block on a full output pipe. A separate read handle leaves the child's
// write offset alone, so the diagnostic hook can inspect a running player.
struct ChildCommand {
    child: Child,
    line: String,
    stdout: NamedTempFile,
    stderr: NamedTempFile,
}

impl ChildCommand {
    fn spawn(program: &str, args: &[&str]) -> Result<Self, String> {
        let mut command = Command::new(program);
        command.args(args);
        let line = format!("{command:?}");
        let not_started = |e| format!("{line}: not started: {e}\nstdout: <empty>\nstderr: <empty>");
        let stdout = NamedTempFile::new().map_err(not_started)?;
        let stderr = NamedTempFile::new().map_err(not_started)?;
        let child = command
            .stdin(Stdio::null())
            .stdout(stdout.as_file().try_clone().map_err(not_started)?)
            .stderr(stderr.as_file().try_clone().map_err(not_started)?)
            .spawn()
            .map_err(not_started)?;
        Ok(Self {
            child,
            line,
            stdout,
            stderr,
        })
    }

    fn output(&self, status: impl fmt::Display) -> CommandOutput {
        fn read(log: &NamedTempFile) -> String {
            match std::fs::read(log.path()) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) => format!("<could not read log: {e}>"),
            }
        }
        CommandOutput {
            label: format!("{}: {status}", self.line),
            stdout: read(&self.stdout),
            stderr: read(&self.stderr),
        }
    }

    fn poll(&mut self) -> Result<Option<ExitStatus>, String> {
        self.child.try_wait().map_err(|e| {
            self.output(format_args!("exit status unavailable: {e}"))
                .to_string()
        })
    }

    fn wait_until(&mut self, deadline: Instant) -> Result<ExitStatus, String> {
        loop {
            if let Some(status) = self.poll()? {
                return Ok(status);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.output("timed out (still running)").to_string());
            }
            thread::sleep(POLL_INTERVAL.min(remaining));
        }
    }

    fn terminate(&mut self) -> Result<(), String> {
        if self.poll()?.is_some() {
            return Ok(());
        }
        if let Err(e) = self.child.kill() {
            return match self.poll()? {
                Some(_) => Ok(()),
                None => Err(self.output(format_args!("failed to kill: {e}")).to_string()),
            };
        }
        self.wait_until(Instant::now() + CONTROL_TIMEOUT)
            .map(|_| ())
    }
}

impl Drop for ChildCommand {
    fn drop(&mut self) {
        if let Err(e) = self.terminate() {
            eprintln!("HOTPLUG-RESTORE-FAILED process cleanup: {e}");
        }
    }
}

struct CommandOutput {
    label: String,
    stdout: String,
    stderr: String,
}

impl fmt::Display for CommandOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\nstdout: {}\nstderr: {}",
            self.label, self.stdout, self.stderr
        )
    }
}

fn command_until(
    program: &str,
    args: &[&str],
    deadline: Instant,
) -> Result<(ExitStatus, CommandOutput), String> {
    if Instant::now() >= deadline {
        return Err(format!(
            "{program} {args:?}: deadline expired (not started)\nstdout: <empty>\nstderr: <empty>"
        ));
    }
    let mut child = ChildCommand::spawn(program, args)?;
    let status = child.wait_until(deadline)?;
    let output = child.output(status);
    if Instant::now() >= deadline {
        return Err(format!(
            "observed command completion after its deadline\n{output}"
        ));
    }
    Ok((status, output))
}

fn run_until(program: &str, args: &[&str], deadline: Instant) -> Result<CommandOutput, String> {
    let (status, output) = command_until(program, args, deadline)?;
    if status.success() {
        Ok(output)
    } else {
        Err(output.to_string())
    }
}

fn run(program: &str, args: &[&str]) -> Result<CommandOutput, String> {
    run_until(program, args, Instant::now() + CONTROL_TIMEOUT)
}

#[derive(Debug)]
struct SinkNode {
    name: String,
    global_id: u32,
    device_id: String,
}

fn sink_nodes(deadline: Instant) -> Result<Vec<SinkNode>, String> {
    let output = run_until("pw-dump", &["--no-colors"], deadline)?;
    let objects: Vec<Value> = serde_json::from_str(&output.stdout)
        .map_err(|e| format!("expected a pw-dump JSON array: {e}\n{output}"))?;
    let mut sinks = Vec::new();
    for obj in &objects {
        let Some(props) = obj.pointer("/info/props") else {
            continue;
        };
        if obj["type"].as_str() != Some("PipeWire:Interface:Node")
            || props["media.class"].as_str() != Some("Audio/Sink")
        {
            continue;
        }
        let name = props["node.name"]
            .as_str()
            .ok_or_else(|| format!("expected a node.name on Audio/Sink: {obj}\n{output}"))?;
        let global_id = obj["id"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
            .ok_or_else(|| format!("expected a u32 global id for {name}: {obj}\n{output}"))?;
        // rsac enumerates the serial, whereas wpctl selects by global id.
        let serial = &props["object.serial"];
        let device_id = serial
            .as_str()
            .map(str::to_owned)
            .or_else(|| serial.as_u64().map(|id| id.to_string()))
            .unwrap_or_else(|| global_id.to_string());
        sinks.push(SinkNode {
            name: name.to_owned(),
            global_id,
            device_id,
        });
    }
    Ok(sinks)
}

fn sink_node(name: &str) -> Result<SinkNode, String> {
    let mut nodes = sink_nodes(Instant::now() + CONTROL_TIMEOUT)?;
    let index = nodes
        .iter()
        .position(|node| node.name == name)
        .ok_or_else(|| format!("expected Audio/Sink node {name}, saw {nodes:?}"))?;
    Ok(nodes.swap_remove(index))
}

fn set_default(name: &str, mut idle: impl FnMut(Duration)) -> Result<(), String> {
    let id = sink_node(name)?.global_id.to_string();
    let switch = run("wpctl", &["set-default", &id])?;
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    let mut last = "<not read yet>".to_owned();
    while Instant::now() < deadline {
        let readback = run_until("pactl", &["get-default-sink"], deadline).map_err(|e| {
            format!("default sink did not become {name} (still {last})\n{switch}\n{e}")
        })?;
        let seen = readback.stdout.trim();
        if seen == name {
            if Instant::now() >= deadline {
                return Err(format!(
                    "default sink became {name} after the deadline\n{readback}"
                ));
            }
            return Ok(());
        }
        last = seen.to_owned();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            idle(READBACK_INTERVAL.min(remaining));
        }
    }
    Err(format!(
        "default sink did not become {name} (still {last})\n{switch}"
    ))
}

fn restore_with_retry(mut action: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    let mut failures = Vec::new();
    for attempt in 1..=RESTORE_ATTEMPTS {
        match action() {
            Ok(()) => return Ok(()),
            Err(e) => failures.push(format!("attempt {attempt}: {e}")),
        }
        if attempt < RESTORE_ATTEMPTS {
            thread::sleep(READBACK_INTERVAL);
        }
    }
    Err(failures.join("\n"))
}

fn restore_default(name: &str) -> Result<(), String> {
    restore_with_retry(|| {
        let current = run("pactl", &["get-default-sink"])?;
        if current.stdout.trim() == name {
            Ok(())
        } else {
            set_default(name, thread::sleep)
        }
    })
}

fn short_list_has_name(list: &str, name: &str) -> bool {
    list.lines()
        .any(|row| row.split_whitespace().nth(1) == Some(name))
}

fn wait_for_missing_sink(name: &str) -> Result<(), String> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    loop {
        let sinks = run_until("pactl", &["list", "short", "sinks"], deadline)
            .map_err(|e| format!("expected {name} to vanish after unload: {e}"))?;
        if !short_list_has_name(&sinks.stdout, name) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining <= READBACK_INTERVAL {
            return Err(format!("expected {name} to vanish after unload\n{sinks}"));
        }
        thread::sleep(READBACK_INTERVAL);
    }
}

fn require_permission(test: &str, allow_var: &str, action: &str) {
    assert!(
        std::env::var("CI").as_deref() == Ok("true")
            || std::env::var(allow_var).as_deref() == Ok("1"),
        "{test}: refusing to {action}; requires CI=true or {allow_var}=1"
    );
}

fn require_default_change_permission(test: &str) {
    require_permission(
        test,
        ALLOW_DEFAULT_CHANGE_VAR,
        "change the configured default sink (cleanup leaves the original sink pinned)",
    );
}

fn wait_for_daemon_exit(uid: &str) -> Result<(), String> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    loop {
        let (status, output) = command_until("pgrep", &["-x", "-u", uid, "pipewire"], deadline)
            .map_err(|e| format!("expected no pipewire process for UID {uid} after pkill: {e}"))?;
        match status.code() {
            Some(1) => return Ok(()), // procps: no matching process
            Some(0) => {}
            _ => return Err(format!("expected pgrep status 1 (no matches)\n{output}")),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining <= READBACK_INTERVAL {
            return Err(format!(
                "expected no pipewire process for UID {uid} after pkill\n{output}"
            ));
        }
        thread::sleep(READBACK_INTERVAL);
    }
}

fn wait_for_sinks_and_monitors() -> Result<(), String> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    let expected = format!("expected {A}, {B} and both .monitor sources");
    let mut last = String::from("<not read yet>");
    while Instant::now() < deadline {
        let sinks = run_until("pactl", &["list", "short", "sinks"], deadline)
            .map_err(|e| format!("{expected}; last seen:\n{last}\n{e}"))?;
        let sources = run_until("pactl", &["list", "short", "sources"], deadline).map_err(|e| {
            format!("{expected}; last sinks:\n{sinks}\nlast complete poll:\n{last}\n{e}")
        })?;
        if [A, B].iter().all(|name| {
            short_list_has_name(&sinks.stdout, name)
                && short_list_has_name(&sources.stdout, &format!("{name}.monitor"))
        }) {
            return Ok(());
        }
        last = format!("{sinks}\n{sources}");
        thread::sleep(READBACK_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
    Err(format!("{expected}, last seen:\n{last}"))
}

fn write_tone(path: &Path, hz: f64) -> hound::Result<()> {
    let spec = hound::WavSpec {
        channels: CAPTURE_CHANNELS,
        sample_rate: CAPTURE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(path, spec)?;
    let step = std::f64::consts::TAU * hz / f64::from(CAPTURE_RATE);
    let scale = f64::from(TONE_AMPLITUDE) * f64::from(i16::MAX);
    for frame in 0..90 * CAPTURE_RATE {
        let sample = ((f64::from(frame) * step).sin() * scale).round() as i16;
        for _ in 0..CAPTURE_CHANNELS {
            wav.write_sample(sample)?;
        }
    }
    wav.finalize()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Module {
    Absent,
    Loading,
    Present(u32),
}

struct Sink {
    name: &'static str,
    module: Module,
    player: Option<Rc<RefCell<ChildCommand>>>,
}

impl Sink {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            module: Module::Absent,
            player: None,
        }
    }

    fn load(&mut self, description: &str) -> Result<(), String> {
        self.module = Module::Loading;
        let output = run(
            "pactl",
            &[
                "load-module",
                "module-null-sink",
                &format!("sink_name={}", self.name),
                &format!("sink_properties=device.description={description}"),
                "rate=48000",
                "channels=2",
                "format=float32le",
            ],
        )?;
        let id = output.stdout.trim().parse::<u32>().map_err(|e| {
            format!(
                "expected the module id for {}, got invalid output: {e}\n{output}",
                self.name
            )
        })?;
        self.module = Module::Present(id);
        Ok(())
    }

    fn unload(&mut self) -> Result<(), String> {
        let id = match self.module {
            Module::Absent => return Ok(()),
            Module::Present(id) => id.to_string(),
            Module::Loading => {
                // The server can commit a load even if its client is killed
                // before reporting the id. Recover only our exact module.
                let output = run("pactl", &["list", "short", "modules"])?;
                let arg = format!("sink_name={}", self.name);
                let Some(id) = output.stdout.lines().find_map(|row| {
                    let mut fields = row.split_whitespace();
                    let id = fields.next()?;
                    (fields.next() == Some("module-null-sink") && fields.any(|f| f == arg))
                        .then_some(id)
                }) else {
                    self.module = Module::Absent;
                    return Ok(());
                };
                id.to_owned()
            }
        };
        if let Err(error) = run("pactl", &["unload-module", &id]) {
            // The server may have committed an unload whose reply we missed.
            // Confirm the module really is gone before treating this as success.
            let modules = run("pactl", &["list", "short", "modules"])
                .map_err(|e| format!("{error}\ncould not verify module {id}: {e}"))?;
            if modules
                .stdout
                .lines()
                .any(|row| row.split_whitespace().next() == Some(id.as_str()))
            {
                return Err(format!("{error}\nmodule {id} still present:\n{modules}"));
            }
        }
        self.module = Module::Absent;
        Ok(())
    }

    fn play(&mut self, path: &Path) -> Result<(), String> {
        let path = path
            .to_str()
            .ok_or_else(|| format!("pw-play needs a UTF-8 WAV path, got {path:?}"))?;
        let child = ChildCommand::spawn(
            "pw-play",
            &[
                &format!("--target={}", self.name),
                "-P",
                r#"{"node.dont-reconnect":true}"#,
                path,
            ],
        )?;
        self.player = Some(Rc::new(RefCell::new(child)));
        Ok(())
    }
}

struct Fixture {
    test: &'static str,
    original: String,
    a: Sink,
    b: Sink,
    tones: TempDir,
    daemon_stopped: bool,
    cleaned: bool,
}

impl Fixture {
    fn new(test: &'static str, play_b: bool) -> Self {
        init_test_logging();
        let output = run("pactl", &["get-default-sink"])
            .unwrap_or_else(|e| panic!("{test}: expected an original default sink: {e}"));
        let original = output.stdout.trim().to_owned();
        assert!(
            !original.is_empty() && original != "(null)",
            "{test}: expected a named original default sink\n{output}"
        );
        let tones = tempfile::tempdir().unwrap_or_else(|e| {
            panic!("{test}: expected a temporary WAV directory, default={original}: {e}")
        });
        let mut fixture = Self {
            test,
            original,
            a: Sink::new(A),
            b: Sink::new(B),
            tones,
            daemon_stopped: false,
            cleaned: false,
        };
        if let Err(e) = fixture.prepare(play_b) {
            fixture.fail(&e);
        }
        fixture
    }

    fn prepare(&mut self, play_b: bool) -> Result<(), String> {
        let before = run("pactl", &["list", "short", "sinks"])?;
        if [A, B]
            .iter()
            .any(|name| short_list_has_name(&before.stdout, name))
        {
            return Err(format!(
                "expected no existing {A} or {B} fixture sink\n{before}"
            ));
        }
        if !short_list_has_name(&before.stdout, &self.original) {
            return Err(format!(
                "original default sink {:?} is not present; cannot restore it\n{before}",
                self.original
            ));
        }
        sink_node(&self.original)?;
        let a_wav = self.tones.path().join("440hz.wav");
        let b_wav = self.tones.path().join("997hz.wav");
        for (path, hz) in [(&a_wav, TONE_A_HZ), (&b_wav, TONE_B_HZ)] {
            write_tone(path, hz)
                .map_err(|e| format!("expected a 90 s PCM16 tone at {path:?}: {e}"))?;
        }
        self.a.load("RSAC_HP_A")?;
        self.b.load("RSAC_HP_B")?;
        wait_for_sinks_and_monitors()?;
        set_default(A, thread::sleep)?;
        self.a.play(&a_wav)?;
        if play_b {
            self.b.play(&b_wav)?;
        }
        self.check_players()
    }

    fn context(&self) -> String {
        format!(
            "test={} a={A} b={B} original_default={}",
            self.test, self.original
        )
    }

    fn machine(&self) -> Box<dyn Machine> {
        Box::new(LinuxMachine {
            players: [&self.a, &self.b]
                .into_iter()
                .filter_map(|sink| sink.player.as_ref().map(Rc::clone))
                .collect(),
        })
    }

    fn state(&self) -> String {
        format!(
            "{} modules: A={:?} B={:?}",
            self.context(),
            self.a.module,
            self.b.module
        )
    }

    fn fail(&self, message: &str) -> ! {
        let machine = self.machine();
        panic!(
            "HOTPLUG FAILURE: {message}\n{}\n{}\nplayers:\n{}",
            self.state(),
            machine.defaults(),
            machine.players().join("\n")
        );
    }

    fn check_players(&self) -> Result<(), String> {
        for sink in [&self.a, &self.b] {
            if sink.module == Module::Absent {
                // A may exit after its sink is deliberately removed in T3.
                continue;
            }
            if let Some(player) = &sink.player {
                let mut child = player.borrow_mut();
                if let Some(status) = child.poll()? {
                    return Err(format!(
                        "expected player for {} to stay alive, exited early\n{}",
                        sink.name,
                        child.output(status)
                    ));
                }
            }
        }
        Ok(())
    }

    fn restore(&mut self) -> Vec<String> {
        if self.cleaned {
            return Vec::new();
        }
        self.cleaned = true;
        let mut errors = Vec::new();
        for sink in [&mut self.a, &mut self.b] {
            if let Some(player) = sink.player.take() {
                let stopped = player.borrow_mut().terminate();
                if let Err(e) = stopped {
                    errors.push(format!("stop player for {}: {e}", sink.name));
                }
            }
        }
        if !self.daemon_stopped && [self.a.module, self.b.module] != [Module::Absent; 2] {
            // Attempt both unloads even if the default read-back or one of the
            // unloads fails; don't leave a second fixture node behind.
            if let Err(e) = restore_default(&self.original) {
                errors.push(format!("restore default sink to {}: {e}", self.original));
            }
            for sink in [&mut self.a, &mut self.b] {
                if let Err(e) = restore_with_retry(|| {
                    sink.unload()?;
                    wait_for_missing_sink(sink.name)
                }) {
                    errors.push(format!("unload {:?} for {}: {e}", sink.module, sink.name));
                }
            }
            if let Err(e) = restore_default(&self.original) {
                errors.push(format!("verify default after unloading fixture sinks: {e}"));
            }
        }
        for error in &errors {
            eprintln!("HOTPLUG-RESTORE-FAILED {error}");
        }
        errors
    }

    fn finish(mut self, mut s: Session, fields: &str) {
        s.stop();
        if !self.daemon_stopped {
            self.check_players().unwrap_or_else(|e| s.fail(&e));
        }
        drop(s);
        if !self.restore().is_empty() {
            self.fail("expected the fixture to restore cleanly; see HOTPLUG-RESTORE-FAILED above");
        }
        let evidence = evidence_line(self.test, fields);
        drop(self);
        println!("{evidence}");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

struct LinuxMachine {
    players: Vec<Rc<RefCell<ChildCommand>>>,
}

impl Machine for LinuxMachine {
    fn defaults(&self) -> String {
        let mut lines = Vec::new();
        for (program, args) in [
            ("pactl", &["get-default-sink"][..]),
            ("pactl", &["list", "short", "sinks"][..]),
            ("wpctl", &["status"][..]),
            ("pw-link", &["-l"][..]),
        ] {
            lines.push(match run(program, args) {
                Ok(output) => output.to_string(),
                Err(e) => e,
            });
        }
        lines.join("\n")
    }

    fn players(&self) -> Vec<String> {
        self.players
            .iter()
            .map(|player| {
                let Ok(mut child) = player.try_borrow_mut() else {
                    return "player status unavailable: command is being inspected".to_owned();
                };
                match child.poll() {
                    Ok(None) => child.output("running").to_string(),
                    Ok(Some(status)) => child.output(status).to_string(),
                    Err(e) => e,
                }
            })
            .collect()
    }
}

// ── Capture phases ───────────────────────────────────────────────────────

fn native_only(s: &Session) -> Result<(), String> {
    if s.generations.keys().any(|&generation| generation != 0)
        || s.events()
            .iter()
            .any(|event| matches!(event.value, RecoveryEvent::Recovered { .. }))
        || s.capture
            .recovery_snapshot()
            .is_some_and(|snap| snap.generation != 0 || snap.successful_recoveries != 0)
    {
        return Err(
            "observed a supervisor reopen; ADR-0019 requires native follow in generation 0"
                .to_owned(),
        );
    }
    Ok(())
}

fn expect_tone(
    s: &mut Session,
    fixture: &Fixture,
    wanted: Verdict,
    since: Checkpoint,
    deadline: Duration,
) -> Duration {
    let what = format!("capture sees {wanted} in generation 0");
    let mut streak = Streak::new(wanted, Some(0));
    let mut next = since.windows;
    let mut seen = None;
    s.wait_for_observation(&what, deadline, |s, _| {
        native_only(s)?;
        fixture.check_players()?;
        for window in &s.windows[next..] {
            if streak.observe(&window.value) {
                seen = Some(window.at);
                return Ok(seen);
            }
        }
        next = s.windows.len();
        Ok(None)
    });
    let seen = seen.expect("wait_for_observation returns only when the streak completes");
    s.mark(format!("{what}: seen at {}", fmt_at(seen)));
    seen
}

fn start_on_a(s: &mut Session, fixture: &Fixture) {
    s.start();
    let since = s.fresh_checkpoint();
    expect_tone(s, fixture, Verdict::A, since, since.at + PHASE_TIMEOUT);
}

fn switch_default(s: &mut Session, fixture: &Fixture, name: &str) -> Checkpoint {
    let result = set_default(name, |pause| s.pump_for(pause));
    if let Err(e) = result {
        s.fail(&format!("{e}\n{}", fixture.state()));
    }
    let verified = s.fresh_checkpoint();
    s.mark(format!("default switched to {name} and read back"));
    verified
}

fn follow_b_then_a(s: &mut Session, fixture: &Fixture) -> [u128; 2] {
    let mut follow_ms = [0; 2];
    for ((name, tone), ms) in [(B, Verdict::B), (A, Verdict::A)]
        .into_iter()
        .zip(&mut follow_ms)
    {
        let switching = s.elapsed();
        let since = switch_default(s, fixture, name);
        let seen = expect_tone(s, fixture, tone, since, switching + PHASE_TIMEOUT);
        *ms = s
            .elapsed_between("default switch to target tone", switching, seen)
            .as_millis();
    }
    follow_ms
}

fn event_name(event: &RecoveryEvent) -> &'static str {
    match event {
        RecoveryEvent::DefaultChanged { .. } => "DefaultChanged",
        RecoveryEvent::DeviceInvalidated { .. } => "DeviceInvalidated",
        RecoveryEvent::RetryScheduled { .. } => "RetryScheduled",
        RecoveryEvent::Recovered { .. } => "Recovered",
        RecoveryEvent::Exhausted { .. } => "Exhausted",
        _ => "Unknown",
    }
}

fn follow_evidence(s: &Session, [to_b, to_a]: [u128; 2]) -> String {
    let events = s
        .events()
        .iter()
        .map(|event| event_name(&event.value))
        .collect::<Vec<_>>()
        .join(",");
    let events = if events.is_empty() { "none" } else { &events };
    format!("a={A} b={B} follow_ms={to_b},{to_a} events={events}")
}

#[test]
fn hotplug_linux_default_switch_follows_natively() {
    const TEST: &str = "hotplug_linux_default_switch_follows_natively";
    let _lock = machine_lock();
    if !enabled(TEST, SWITCH_VAR) {
        return;
    }
    require_default_change_permission(TEST);
    let fixture = Fixture::new(TEST, true);
    let mut s = Session::open(
        TEST,
        Some(AutoRecoveryConfig::new()),
        fixture.machine(),
        fixture.state(),
    );
    s.forbid_exhausted = true;
    start_on_a(&mut s, &fixture);
    let latencies = follow_b_then_a(&mut s, &fixture);
    s.pump();
    if let Err(e) = native_only(&s) {
        s.fail(&e);
    }
    s.expect_running_snapshot(0, 0);
    let route = follow_evidence(&s, latencies);
    let fields = format!(
        "{route} generation=0 min_dominance_db={:.1}",
        s.min_dominance_db()
    );
    fixture.finish(s, &fields);
}

#[test]
fn hotplug_linux_default_switch_without_recovery_also_follows() {
    const TEST: &str = "hotplug_linux_default_switch_without_recovery_also_follows";
    let _lock = machine_lock();
    if !enabled(TEST, SWITCH_VAR) {
        return;
    }
    require_default_change_permission(TEST);
    let fixture = Fixture::new(TEST, true);
    let mut s = Session::open(TEST, None, fixture.machine(), fixture.state());
    s.forbid_exhausted = true;
    start_on_a(&mut s, &fixture);
    let latencies = follow_b_then_a(&mut s, &fixture);
    let fields = follow_evidence(&s, latencies);
    fixture.finish(s, &fields);
}

fn enumerated_sink(name: &str) -> Result<DeviceId, String> {
    let node = sink_node(name)?;
    let enumerator = get_device_enumerator()
        .map_err(|e| format!("expected a device enumerator to look up {node:?}, got {e:?}"))?;
    let devices = enumerator
        .enumerate_devices()
        .map_err(|e| format!("expected public enumeration to list {node:?}, got {e:?}"))?;
    let mut seen = Vec::new();
    for device in devices {
        let id = device.id();
        if id.0 == node.device_id {
            return Ok(id);
        }
        seen.push((id, device.name()));
    }
    Err(format!(
        "expected public enumeration to list {node:?}, saw {seen:?}"
    ))
}

fn no_b_since(s: &Session, from: usize) -> Result<(), String> {
    if let Some(window) = s.windows[from..]
        .iter()
        .find(|w| w.value.verdict == Verdict::B)
    {
        return Err(format!(
            "expected no B-dominant audio on the explicit A device, saw B at {} ({:.1} dB)",
            fmt_at(window.at),
            window.value.dominance_b_db()
        ));
    }
    Ok(())
}

fn terminal_error(s: &Session, since: Duration) -> (Duration, String) {
    // Linux's Error/Unconnected callback marks the bridge terminal, and the
    // supervisor's Exhausted phase likewise makes reads return StreamEnded.
    match &s.terminal {
        Some(t)
            if !t.value.is_recoverable() && matches!(t.value, AudioError::StreamEnded { .. }) =>
        {
            s.elapsed_between("verified outage to terminal read", since, t.at);
            (t.at, variant_name(&t.value))
        }
        other => s.fail(&format!(
            "expected a non-recoverable StreamEnded after the verified outage, saw {other:?}"
        )),
    }
}

fn open_device_session(fixture: &Fixture, name: &str) -> Session {
    let id = enumerated_sink(name).unwrap_or_else(|e| fixture.fail(&e));
    let context = format!(
        "{} explicit_device={name} explicit_device_id={id}",
        fixture.state()
    );
    let capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(id))
        .sample_rate(CAPTURE_RATE)
        .channels(CAPTURE_CHANNELS)
        .build()
        .unwrap_or_else(|e| {
            fixture.fail(&format!("expected an explicit capture on {name}: {e:?}"))
        });
    Session::new(capture, None, fixture.machine(), context)
}

#[test]
fn hotplug_linux_explicit_device_does_not_follow_and_ends_when_its_node_vanishes() {
    const TEST: &str =
        "hotplug_linux_explicit_device_does_not_follow_and_ends_when_its_node_vanishes";
    let _lock = machine_lock();
    if !enabled(TEST, SWITCH_VAR) {
        return;
    }
    require_default_change_permission(TEST);
    let mut fixture = Fixture::new(TEST, true);
    let mut s = open_device_session(&fixture, A);
    start_on_a(&mut s, &fixture);

    // Verify B delivers real audio during the same interval, independently
    // of SystemDefault routing so this control does not depend on T1/T2.
    let mut reference = open_device_session(&fixture, B);
    reference.start();
    let switching = s.fresh_checkpoint();
    let switched = switch_default(&mut s, &fixture, B);
    let mut next_b = reference.fresh_checkpoint().windows;
    let mut b_streak = Streak::new(Verdict::B, Some(0));
    let a_windows = s.expect_stays_on_a(
        "the first 15 fresh windows after B became default must all be A-dominant",
        switched,
        |_| {
            reference.pump();
            for window in &reference.windows[next_b..] {
                b_streak.observe(&window.value);
            }
            next_b = reference.windows.len();
            fixture.check_players()
        },
    );
    no_b_since(&s, switching.windows).unwrap_or_else(|e| s.fail(&e));
    if !b_streak.is_seen() {
        s.fail(&format!(
            "expected to hear B on the reference capture during the A-only phase\n{}",
            reference.report()
        ));
    }
    fixture.check_players().unwrap_or_else(|e| s.fail(&e));
    let default = run("pactl", &["get-default-sink"]).unwrap_or_else(|e| s.fail(&e));
    if default.stdout.trim() != B {
        s.fail(&format!(
            "expected {B} to still be default after the A-only phase\n{default}"
        ));
    }
    reference.stop();
    drop(reference);
    s.mark("verified B audible while the explicit A capture stayed on A");

    let unload = s.fresh_checkpoint();
    no_b_since(&s, switching.windows).unwrap_or_else(|e| s.fail(&e));
    s.expect_terminal = true;
    fixture
        .a
        .unload()
        .and_then(|()| wait_for_missing_sink(A))
        .unwrap_or_else(|e| s.fail(&format!("expected to unload {A}: {e}\n{}", fixture.state())));
    // A draining checkpoint would fail if StreamEnded is already readable.
    // The pre-unload fresh cut broke the partial window; no reads happened
    // during the unload/read-back. Only poll after the outage is verified.
    let verified = s.checkpoint();
    s.mark(format!("verified {A} absent in pactl list short sinks"));
    s.wait_for_observation(
        "explicit A capture ends without B after A is unloaded",
        unload.at + PHASE_TIMEOUT,
        |s, first| {
            no_b_since(s, first)?;
            fixture.check_players()?;
            Ok(s.terminal.as_ref().map(|t| t.at))
        },
    );
    let (terminal_at, error) = terminal_error(&s, verified.at);
    let ended = s.elapsed_between("unload to terminal read", unload.at, terminal_at);
    let fields = format!(
        "a={A} b={B} a_windows={a_windows} b_windows=0 ended_ms={} error={error}",
        ended.as_millis()
    );
    fixture.finish(s, &fields);
}

#[test]
fn hotplug_linux_daemon_stop_spends_the_retry_budget() {
    const TEST: &str = "hotplug_linux_daemon_stop_spends_the_retry_budget";
    let _lock = machine_lock();
    if !enabled(TEST, DAEMON_STOP_VAR) {
        return;
    }
    require_permission(
        TEST,
        ALLOW_DAEMON_KILL_VAR,
        "stop every PipeWire daemon of the current user",
    );
    // SAFETY: geteuid only reads the calling process's effective UID.
    let uid = unsafe { libc::geteuid() }.to_string();
    let mut fixture = Fixture::new(TEST, false);
    let config = AutoRecoveryConfig::new()
        .with_max_attempts(RETRY_BUDGET)
        .with_initial_backoff(FIRST_BACKOFF)
        .with_max_backoff(Duration::from_secs(1))
        .with_device_wait_timeout(Duration::from_secs(120));
    let mut s = Session::open(TEST, Some(config), fixture.machine(), fixture.state());
    start_on_a(&mut s, &fixture);

    s.fresh_checkpoint();
    s.expect_running_snapshot(0, 0);
    s.expect_terminal = true;
    let killed = s.checkpoint();
    if let Err(e) = run("pkill", &["-x", "-u", &uid, "pipewire"]) {
        s.fail(&format!(
            "expected pkill to signal the PipeWire daemons for UID {uid}: {e}"
        ));
    }
    // The runner starts PipeWire manually and will discard the stopped audio
    // session. On any exit after a successful kill, still reap the player.
    fixture.daemon_stopped = true;
    wait_for_daemon_exit(&uid).unwrap_or_else(|e| s.fail(&e));
    let verified = s.checkpoint();
    s.mark(format!(
        "verified no pipewire process remains for UID {uid}"
    ));

    // Gate the reads on verified daemon loss, but keep the event fence at
    // the kill action so verification cannot hide a scheduled retry.
    let spent = format!("auto-recovery exhausted after {RETRY_BUDGET} failed attempt(s): ");
    let mut exhaustion = None;
    s.wait_for_observation(
        "daemon loss exhausts retries and returns a terminal read within 60 s",
        killed.at + EXHAUST_TIMEOUT,
        |s, _| {
            let events = &s.events()[killed.events..];
            let Some(index) = events.iter().position(is_exhausted) else {
                return Ok(None);
            };
            let event = &events[index];
            let retries = events[..index]
                .iter()
                .filter(|event| matches!(event.value, RecoveryEvent::RetryScheduled { .. }))
                .count();
            if retries == 0
                || event.at < killed.at + FIRST_BACKOFF
                || !matches!(
                    &event.value,
                    RecoveryEvent::Exhausted { attempts: RETRY_BUDGET, reason, .. }
                        if reason.starts_with(&spent)
                )
            {
                return Err(format!(
                    "expected RetryScheduled then budget exhaustion no earlier than {}; \
                     saw {retries} retries and {event:?}",
                    fmt_at(killed.at + FIRST_BACKOFF)
                ));
            }
            let Some(terminal) = &s.terminal else {
                return Ok(None);
            };
            exhaustion = Some((event.at, retries));
            Ok(Some(event.at.max(terminal.at)))
        },
    );
    let (exhausted_at, retries) = exhaustion.expect("observed retry-budget exhaustion");
    let (_, error) = terminal_error(&s, verified.at);
    let exhausted = s.elapsed_between(
        "daemon kill to retry-budget exhaustion",
        killed.at,
        exhausted_at,
    );
    let fields = format!(
        "exhausted_ms={} retries={retries} error={error} restore=skipped-daemon-stopped",
        exhausted.as_millis()
    );
    fixture.finish(s, &fields);
}
