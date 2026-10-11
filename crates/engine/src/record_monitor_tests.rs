//! Tests of [`crate::record_monitor`]: the microphone level before a recording, with the
//! synthetic input (a click every 250 ms on the channel a mono recording takes).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::record::SyntheticFactory;
use crate::voiceover::{AudioInput, InputFormat, SyntheticInput};
use serde_json::{Value, json};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("filmcraft-recmon-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn session() -> Session {
    let mut s = Session::default();
    s.record.factory = Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), ..Default::default() }));
    s
}

/// `monitor` of `record.status`.
fn monitors(s: &mut Session) -> Value {
    s.execute("record.status", json!({})).unwrap()["monitor"].clone()
}

fn devices(m: &Value) -> Vec<String> {
    m.as_array().unwrap().iter().map(|x| x["device"].as_str().unwrap().to_string()).collect()
}

/// Whether monitor `i` shows both silence and a click within a few seconds.
fn level_moves(s: &mut Session, i: usize) -> bool {
    let (mut low, mut high) = (false, false);
    let t0 = Instant::now();
    while !(low && high) && t0.elapsed() < Duration::from_secs(5) {
        if let Some(l) = monitors(s)[i]["level"].as_f64() {
            low |= l < 0.1;
            high |= l > 0.4;
        }
        // a monitor reads its microphone every 30 ms
        std::thread::sleep(Duration::from_millis(12));
    }
    low && high
}

#[test]
fn the_level_of_a_microphone_is_reported_before_a_recording() {
    let mut s = session();
    assert_eq!(monitors(&mut s), json!([]), "nothing is monitored until asked");
    let r = s.execute("record.monitor", json!({"mics": ["default"]})).unwrap();
    assert_eq!(r["wanted"], json!(["default"]), "{r}");
    assert_eq!(devices(&r["monitor"]), ["default"], "{r}");
    assert!(r["monitor"][0]["level"].is_number() && r["monitor"][0].get("error").is_none(), "{r}");
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["recording"].as_bool(), devices(&st["monitor"])), (Some(false), vec!["default".to_string()]), "{st}");
    assert!(level_moves(&mut s, 0), "the level follows the microphone: silence and its clicks");
    assert!(crate::record_monitor::level(&s).is_some());
    // "" is the default input too; a second microphone gets a monitor of its own
    let r = s.execute("record.monitor", json!({"mics": ["", SyntheticInput::DEVICE2]})).unwrap();
    assert_eq!(devices(&r["monitor"]), ["default", SyntheticInput::DEVICE2], "{r}");
    assert!(level_moves(&mut s, 0) && level_moves(&mut s, 1));
    // the same microphone twice is one monitor; without `mics` it only reports
    let r = s.execute("record.monitor", json!({"mics": [SyntheticInput::DEVICE2, "default", "", SyntheticInput::DEVICE2]})).unwrap();
    assert_eq!(devices(&r["monitor"]), [SyntheticInput::DEVICE2, "default"], "in the order asked for: {r}");
    let r = s.execute("record.monitor", json!({})).unwrap();
    assert_eq!((devices(&r["monitor"]).len(), r["wanted"].as_array().unwrap().len()), (2, 2), "{r}");
    // [] and null stop them: the microphones are closed
    let r = s.execute("record.monitor", json!({"mics": []})).unwrap();
    assert_eq!((r["monitor"].clone(), r["wanted"].clone()), (json!([]), json!([])));
    assert!(crate::record_monitor::level(&s).is_none());
    s.execute("record.monitor", json!({"mics": [SyntheticInput::DEVICE]})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), [SyntheticInput::DEVICE]);
    let r = s.execute("record.monitor", json!({"mics": null})).unwrap();
    assert_eq!(r["monitor"], json!([]));
    assert!(s.voiceover.input.is_some(), "the session's own input was never taken");
}

#[test]
fn record_start_takes_the_microphones_and_stop_gives_the_monitors_back() {
    let dir = tmp("handover");
    let mut s = session();
    let d = dir.to_string_lossy().into_owned();
    s.execute("record.monitor", json!({"mics": ["default", SyntheticInput::DEVICE2]})).unwrap();
    assert_eq!(monitors(&mut s).as_array().unwrap().len(), 2);
    // recording: the monitors are gone (the recording reads the microphones), the list is kept
    s.execute("record.start", json!({"mics": [{}, {"device": SyntheticInput::DEVICE2}], "dir": d})).unwrap();
    let st = s.execute("record.status", json!({})).unwrap();
    assert_eq!((st["recording"].as_bool(), st["monitor"].clone()), (Some(true), json!([])), "{st}");
    assert_eq!(s.record.monitor_wanted, ["default", SyntheticInput::DEVICE2]);
    assert!(st["sources"][0]["level"].is_number(), "the recording has the level now: {st}");
    // asking again while recording changes the list only
    let r = s.execute("record.monitor", json!({"mics": ["default"]})).unwrap();
    assert_eq!((r["wanted"].clone(), r["monitor"].clone()), (json!(["default"]), json!([])), "{r}");
    std::thread::sleep(Duration::from_millis(300));
    // Stop: they are back without being asked for again
    let v = s.execute("record.stop", json!({})).unwrap();
    assert_eq!(v["errors"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    assert!(level_moves(&mut s, 0), "and it reads the microphone again");
    // Cancel gives them back too
    s.execute("record.start", json!({"mic": {}, "dir": d})).unwrap();
    assert_eq!(monitors(&mut s), json!([]));
    s.execute("record.cancel", json!({})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // a countdown has not opened the microphones yet: the level stays; cancelling it keeps it
    s.execute("record.start", json!({"mic": {}, "dir": d, "countdown": 5})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    assert!(level_moves(&mut s, 0));
    s.execute("record.cancel", json!({})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // a start that is refused before anything opens leaves them running
    assert!(s.execute("record.start", json!({"mic": {"device": "Nope"}, "dir": d})).is_err());
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // a start that fails while its sources start gives them back
    s.record.factory =
        Some(Arc::new(SyntheticFactory { display_size: (320, 180), camera_size: (320, 180), camera_delay_ms: u64::MAX / 2, ..Default::default() }));
    s.record.start_timeout = Some(Duration::from_millis(300));
    assert!(s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": d})).is_err());
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // the same without waiting: gone while the sources start, back when the start has failed
    s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": d, "wait": false})).unwrap();
    assert_eq!(s.execute("record.monitor", json!({})).unwrap()["monitor"], json!([]));
    let t0 = Instant::now();
    while s.record.starting.is_some() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
        s.execute("record.status", json!({})).unwrap();
    }
    assert_eq!(s.record.last_event.as_ref().unwrap()["event"], "startFailed");
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // and a start cancelled while its sources start
    s.record.start_timeout = None;
    s.execute("record.start", json!({"camera": {"device": SyntheticFactory::CAMERA}, "mic": {}, "dir": d, "wait": false})).unwrap();
    s.execute("record.cancel", json!({})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    assert!(s.voiceover.input.is_some());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_devices_too_many_and_hostile_params_are_refused() {
    let mut s = session();
    s.execute("record.monitor", json!({"mics": ["default"]})).unwrap();
    let e = s.execute("record.monitor", json!({"mics": ["default", "No Such Mic"]})).unwrap_err().to_string();
    assert!(e.contains("no microphone `No Such Mic`"), "{e}");
    let e = s.execute("record.monitor", json!({"mics": ["", "", "", "", ""]})).unwrap_err().to_string();
    assert!(e.contains("at most 4"), "{e}");
    for bad in [json!(7), json!("default"), json!({"device": "default"}), json!(true), json!(1e300)] {
        let e = s.execute("record.monitor", json!({"mics": bad.clone()})).unwrap_err().to_string();
        assert!(e.contains("`mics` must be a list of device names"), "{bad}: {e}");
    }
    for bad in [json!([7]), json!([null]), json!([["default"]]), json!(["default", {"device": "default"}])] {
        let e = s.execute("record.monitor", json!({"mics": bad.clone()})).unwrap_err().to_string();
        assert!(e.contains("each of `mics` must be a device name"), "{bad}: {e}");
    }
    // a refused call changes nothing
    assert_eq!(s.record.monitor_wanted, ["default"]);
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // parameters that are no object only report
    for p in [json!(null), json!([1, 2]), json!("mics"), json!(42)] {
        let r = s.execute("record.monitor", p.clone()).unwrap_or_else(|e| panic!("{p}: {e}"));
        assert_eq!(devices(&r["monitor"]), ["default"], "{p}");
    }
    // a very long or odd name is just an unknown device
    for name in ["x".repeat(100_000), "\u{0}".to_string(), "../default".to_string()] {
        assert!(s.execute("record.monitor", json!({"mics": [name]})).is_err());
    }
    let spec = crate::commands::find("record.monitor").unwrap();
    assert!(!spec.label.is_empty() && spec.params.contains("mics") && (spec.enabled)(&s).is_ok());
}

#[test]
fn a_voice_over_stops_the_monitors_and_they_come_back() {
    let dir = tmp("voiceover");
    let mut s = session();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("record.monitor", json!({"mics": ["default"]})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    s.execute("audio.voiceover.start", json!({})).unwrap();
    assert_eq!(monitors(&mut s), json!([]), "the voice-over has the microphone");
    s.execute("audio.voiceover.stop", json!({"discard": true})).unwrap();
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    // a saved take gives them back as well
    s.execute("audio.voiceover.start", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let v = s.execute("audio.voiceover.stop", json!({"dir": dir.to_string_lossy()})).unwrap();
    assert_eq!(v["recording"], false, "{v}");
    assert_eq!(devices(&monitors(&mut s)), ["default"]);
    std::fs::remove_dir_all(&dir).ok();
}

/// An input that cannot be opened twice.
struct Single;

impl AudioInput for Single {
    fn devices(&self) -> Vec<String> {
        vec!["Only".into()]
    }
    fn channels(&self, _: &str) -> u16 {
        1
    }
    fn start(&mut self, _: &str, sample_rate: u32) -> std::result::Result<InputFormat, String> {
        Ok(InputFormat { sample_rate, channels: 1 })
    }
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>> {
        vec![vec![0.0; frames]]
    }
    fn discard(&mut self) {}
    fn stop(&mut self) {}
}

/// An input whose copies fail: `start` refuses, or `read` panics.
struct Broken {
    panics: bool,
}

impl AudioInput for Broken {
    fn devices(&self) -> Vec<String> {
        vec!["Broken".into()]
    }
    fn channels(&self, _: &str) -> u16 {
        1
    }
    fn start(&mut self, device: &str, sample_rate: u32) -> std::result::Result<InputFormat, String> {
        if self.panics { Ok(InputFormat { sample_rate, channels: 1 }) } else { Err(format!("cannot open `{device}`: allow the microphone in System Settings")) }
    }
    fn read(&mut self, _: usize) -> Vec<Vec<f32>> {
        panic!("the driver crashed")
    }
    fn discard(&mut self) {}
    fn stop(&mut self) {}
    fn spawn(&self) -> Option<Box<dyn AudioInput>> {
        Some(Box::new(Broken { panics: self.panics }))
    }
}

#[test]
fn an_input_that_cannot_be_opened_twice_has_no_monitor() {
    let mut s = session();
    s.voiceover.input = Some(Box::new(Single));
    let r = s.execute("record.monitor", json!({"mics": ["default", "Only"]})).unwrap();
    assert_eq!((r["wanted"].clone(), r["monitor"].clone()), (json!(["default", "Only"]), json!([])), "simply no monitor: {r}");
    assert!(crate::record_monitor::level(&s).is_none());
    assert!(s.voiceover.input.is_some(), "the session's input is never used for it");
}

#[test]
fn a_microphone_that_cannot_be_read_reports_why_and_never_takes_the_session_down() {
    let wait_error = |s: &mut Session| -> String {
        let t0 = Instant::now();
        loop {
            let m = monitors(s);
            if let Some(e) = m[0]["error"].as_str() {
                assert_eq!(m[0]["level"], 0.0, "{m}");
                return e.to_string();
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "no error: {m}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let mut s = session();
    s.voiceover.input = Some(Box::new(Broken { panics: false }));
    s.execute("record.monitor", json!({"mics": ["Broken"]})).unwrap();
    let e = wait_error(&mut s);
    assert!(e.contains("cannot open `Broken`") && e.contains("System Settings"), "{e}");
    // a device whose driver panics: the monitor thread reports it, the session goes on
    s.voiceover.input = Some(Box::new(Broken { panics: true }));
    s.execute("record.monitor", json!({"mics": []})).unwrap();
    s.execute("record.monitor", json!({"mics": ["default"]})).unwrap();
    let e = wait_error(&mut s);
    assert!(e.contains("crashed"), "{e}");
    assert_eq!(s.execute("record.status", json!({})).unwrap()["recording"], false);
    s.execute("record.monitor", json!({"mics": []})).unwrap();
    assert_eq!(monitors(&mut s), json!([]));
}
