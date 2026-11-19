//! One Remote Play session: find the console (waking it if needed), connect with chiaki, and
//! carry media one way and controller input the other until someone ends it.
//!
//! Each session has its own thread that owns chiaki's `Session`. The message loop talks to
//! it through a channel, and so do chiaki's callbacks when the console ends the session. That
//! thread is the only place that reports the final `ended` status, so XMouse gets it exactly
//! once whatever the reason was.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chiaki_core::controller::ControllerState;
use chiaki_core::discovery::DiscoveryHostState;
use chiaki_core::session::{
    quit_reason_is_error, quit_reason_string, ConnectInfo, Session, SessionCallbacks, SessionEvent,
    VideoFrame,
};
use chiaki_core::takion::DisableAudioVideo;
use chiaki_core::video::{ConnectVideoProfile, VideoFpsPreset, VideoResolutionPreset};

use crate::consoles;
use crate::h264::Assembler;
use crate::out::Out;
use crate::proto::{self, FromPlugin, StatusPhase};
use crate::store::{Pairing, Store};

/// How long to wait for a console that is waking up before giving up. A PS5 on a hard disk
/// extension can take most of a minute.
const WAKE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long to look for a console that doesn't answer at all.
const FIND_TIMEOUT: Duration = Duration::from_secs(12);
/// The wake-up message is repeated this often while waiting (a single UDP packet can be lost).
const WAKE_REPEAT: Duration = Duration::from_secs(5);

/// What XMouse asked for in `start`.
#[derive(Clone, Debug)]
pub struct StartRequest {
    pub console_id: String,
    /// "tv" mode: the game stays on the TV and XMouse throws media away, so none is requested
    /// from the console or passed on.
    pub tv: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

/// What the session thread is told.
pub enum Command {
    Input(ControllerState),
    Keyframe,
    /// XMouse wants the session over (or went away).
    Stop,
    /// chiaki reports the session over; `Some` is an error to show.
    Quit(Option<String>),
}

pub struct Handle {
    tx: Sender<Command>,
    thread: JoinHandle<()>,
}

impl Handle {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command); // the thread having ended already is fine
    }

    pub fn finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// End the session and wait until the console has been told and the threads are gone.
    pub fn stop(self) {
        let _ = self.tx.send(Command::Stop);
        let _ = self.thread.join();
    }
}

pub fn spawn(out: Out, data_dir: PathBuf, request: StartRequest) -> Handle {
    let (tx, rx) = mpsc::channel();
    let callbacks_tx = tx.clone();
    let thread = std::thread::spawn(move || {
        let error = run(&out, &data_dir, &request, callbacks_tx, &rx).err();
        if let Some(e) = &error {
            eprintln!("psnbridge: session ended: {e}");
        }
        out.ended(error.as_deref());
    });
    Handle { tx, thread }
}

/// The stream settings to ask the console for. The console only offers four sizes; the
/// largest that fits what XMouse can show is taken (720p if XMouse didn't say). 1080p exists
/// on PS5 and PS4 Pro only; chiaki falls back by itself where the console refuses a size.
pub fn pick_profile(max_width: u32, max_height: u32, fps: u32, bitrate_kbps: u32) -> ConnectVideoProfile {
    let fits = |w: u32, h: u32| (max_width == 0 || w <= max_width) && (max_height == 0 || h <= max_height);
    let resolution = if max_width == 0 && max_height == 0 {
        VideoResolutionPreset::Res720p
    } else if fits(1920, 1080) {
        VideoResolutionPreset::Res1080p
    } else if fits(1280, 720) {
        VideoResolutionPreset::Res720p
    } else if fits(960, 540) {
        VideoResolutionPreset::Res540p
    } else {
        VideoResolutionPreset::Res360p
    };
    // The console streams at 30 or 60 pictures a second. A limit below 60 gets 30; no limit
    // given (0) means "your choice", which is 60.
    let fps = if fps == 0 || fps >= 60 { VideoFpsPreset::Fps60 } else { VideoFpsPreset::Fps30 };
    // The preset brings chiaki's usual bitrate for that size, and H.264 (the only video codec
    // XMouse decodes).
    let mut profile = ConnectVideoProfile::from_preset(resolution, fps);
    if bitrate_kbps != 0 {
        profile.bitrate = bitrate_kbps.clamp(1000, 50_000);
    }
    profile
}

/// chiaki's callbacks: called on its network threads, so they only hand things on.
struct Bridge {
    out: Out,
    tv: bool,
    started: Instant,
    assembler: Mutex<Assembler>,
    /// What the console said about its audio stream (the packets themselves don't repeat it).
    audio_channels: AtomicU8,
    audio_rate: AtomicU32,
    commands: Mutex<Sender<Command>>,
}

impl Bridge {
    fn tell(&self, command: Command) {
        let _ = self.commands.lock().unwrap_or_else(PoisonError::into_inner).send(command);
    }
}

impl SessionCallbacks for Bridge {
    /// One video sample. Returning `false` tells chiaki the picture was lost on our side, and
    /// it asks the console for a fresh full picture.
    fn video_frame(&self, frame: VideoFrame<'_>) -> bool {
        if self.tv {
            return true;
        }
        let mut assembler = self.assembler.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(unit) = assembler.push(frame.data) else {
            return true; // parameter sets: remembered for the next full picture
        };
        let pts = self.started.elapsed().as_micros() as u64;
        match proto::video_frame(unit.keyframe, pts, &[unit.prefix, unit.body]) {
            Some(bytes) => self.out.media(bytes),
            None => false, // larger than XMouse accepts: can't be delivered
        }
    }

    /// One Opus packet (the callback's name is historical: chiaki hands over the encoded
    /// packets and leaves decoding to the application, here XMouse). Empty means "a packet
    /// was lost"; XMouse has no message for that, so nothing is sent.
    fn audio_pcm(&self, packet: &[u8]) {
        if self.tv || packet.is_empty() {
            return;
        }
        let channels = self.audio_channels.load(Ordering::Relaxed);
        let rate = self.audio_rate.load(Ordering::Relaxed);
        if let Some(bytes) = proto::audio_frame(channels, rate, packet) {
            self.out.media(bytes);
        }
    }

    fn event(&self, event: SessionEvent) {
        match event {
            SessionEvent::Connected => self.out.status(StatusPhase::Connected, ""),
            SessionEvent::AudioStreamInfo(header) => {
                if header.channels != 0 && header.rate != 0 {
                    self.audio_channels.store(header.channels, Ordering::Relaxed);
                    self.audio_rate.store(header.rate, Ordering::Relaxed);
                }
            }
            SessionEvent::Rumble { left, right, .. } => self.out.control(&FromPlugin::Rumble {
                left: f32::from(left) / 255.0,
                right: f32::from(right) / 255.0,
            }),
            SessionEvent::LoginPinRequest(_) => {
                // The console account has a login passcode. XMouse's plugin protocol has no
                // way to ask the user for it, so the session can't go on.
                self.tell(Command::Quit(Some(
                    "The console asks for the login passcode of its user account, which this plugin \
                     can't enter. Remove the passcode on the console (Settings > Users and Accounts > \
                     Login Settings) to use Remote Play from XMouse."
                        .into(),
                )));
            }
            SessionEvent::Quit { reason, reason_str } => {
                let error = quit_reason_is_error(reason).then(|| {
                    let text = quit_reason_string(reason);
                    if reason_str.trim().is_empty() {
                        text.to_string()
                    } else {
                        format!("{text} ({})", reason_str.trim())
                    }
                });
                self.tell(Command::Quit(error));
            }
            // Keyboard, DualSense lights and triggers, motion reset: nothing in XMouse to
            // pass them to.
            _ => {}
        }
    }
}

/// Wait for `wait`, or less if XMouse asks to stop. `true` = stop was asked (or XMouse is
/// gone). Input and keyframe requests that arrive before there is a session mean nothing.
fn stop_requested(rx: &Receiver<Command>, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        match rx.try_recv() {
            Ok(Command::Stop) | Ok(Command::Quit(_)) | Err(TryRecvError::Disconnected) => return true,
            Ok(_) => continue,
            Err(TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20).min(deadline - now));
    }
}

/// Find the console's address, waking it if it is in rest mode. `Ok(None)`: XMouse asked to
/// stop while we were waiting.
fn locate(
    out: &Out,
    data_dir: &std::path::Path,
    store: &mut Store,
    console_id: &str,
    pairing: &Pairing,
    rx: &Receiver<Command>,
) -> Result<Option<String>, String> {
    let began = Instant::now();
    let mut last_wake: Option<Instant> = None;
    let mut waking = false;
    loop {
        if stop_requested(rx, Duration::ZERO) {
            return Ok(None);
        }
        let found = consoles::search(Duration::from_millis(1500), &consoles::known_hosts(store), &|list| {
            list.iter().any(|f| f.id == console_id)
        })?;
        if consoles::remember_hosts(store, &found) {
            let _ = store.save(data_dir);
        }
        let console = found.into_iter().find(|f| f.id == console_id);
        match &console {
            Some(c) if c.state == DiscoveryHostState::Ready => return Ok(Some(c.addr.clone())),
            Some(c) => {
                // In rest mode: wake it and keep asking until it reports ready.
                if !waking {
                    waking = true;
                    out.status(StatusPhase::Waiting, "Waking the console…");
                }
                if last_wake.map_or(true, |t| t.elapsed() >= WAKE_REPEAT) {
                    last_wake = Some(Instant::now());
                    if let Err(e) = consoles::send_wake(pairing, &c.addr) {
                        out.log("warn", &e);
                    }
                }
                if began.elapsed() > WAKE_TIMEOUT {
                    return Err("The console did not wake up. Check that \"Enable turning on from network\" \
                                is switched on in its power saving settings."
                        .into());
                }
            }
            None => {
                // No answer. A console that is fully off can't be reached at all; one that is
                // on another network connection might still take a directed wake-up.
                if last_wake.is_none() && !pairing.last_host.is_empty() {
                    last_wake = Some(Instant::now());
                    let _ = consoles::send_wake(pairing, &pairing.last_host);
                }
                if began.elapsed() > if waking { WAKE_TIMEOUT } else { FIND_TIMEOUT } {
                    return Err("The console was not found. It must be switched on or in rest mode, and on \
                                the same network as this PC."
                        .into());
                }
            }
        }
        if stop_requested(rx, Duration::from_millis(500)) {
            return Ok(None);
        }
    }
}

/// The whole life of one session. `Err` is the sentence XMouse shows; `Ok` is a normal end.
fn run(
    out: &Out,
    data_dir: &std::path::Path,
    request: &StartRequest,
    callbacks_tx: Sender<Command>,
    rx: &Receiver<Command>,
) -> Result<(), String> {
    out.status(StatusPhase::Starting, "Looking for the console…");
    let (mut store, problem) = Store::load(data_dir);
    if let Some(p) = problem {
        out.log("warn", &p);
    }
    let pairing = store
        .consoles
        .get(&request.console_id)
        .cloned()
        .ok_or("This console is not paired yet.")?;
    let (Some(regist_key), Some(rp_key)) = (pairing.regist_key_bytes(), pairing.rp_key_bytes()) else {
        return Err("The stored pairing is damaged. Pair the console again.".into());
    };

    let Some(addr) = locate(out, data_dir, &mut store, &request.console_id, &pairing, rx)? else {
        return Ok(());
    };

    out.status(StatusPhase::Connecting, "Connecting…");
    let profile = pick_profile(request.max_width, request.max_height, request.fps, request.bitrate_kbps);
    out.log(
        "info",
        &format!(
            "connecting to {} at {addr}: {}x{} at {} fps, {} kbit/s, {}",
            if pairing.ps5 { "PS5" } else { "PS4" },
            profile.width,
            profile.height,
            profile.max_fps,
            profile.bitrate,
            if request.tv { "TV mode (no media)" } else { "PC mode" },
        ),
    );
    let bridge = Arc::new(Bridge {
        out: out.clone(),
        tv: request.tv,
        started: Instant::now(),
        assembler: Mutex::new(Assembler::default()),
        // What the console sends unless it says otherwise: 48 kHz stereo.
        audio_channels: AtomicU8::new(2),
        audio_rate: AtomicU32::new(48_000),
        commands: Mutex::new(callbacks_tx),
    });
    let info = ConnectInfo {
        ps5: pairing.ps5,
        host: addr,
        regist_key,
        morning: rp_key,
        video_profile: profile,
        video_profile_auto_downgrade: true,
        enable_keyboard: false,
        // With DualSense features on, the console sends haptics as an audio stream instead of
        // the two rumble strengths XMouse understands.
        enable_dualsense: false,
        audio_video_disabled: if request.tv {
            DisableAudioVideo::AudioVideoDisabled
        } else {
            DisableAudioVideo::NoneDisabled
        },
        auto_regist: false,
        // Internet play through Sony's servers is a different connection path; this plugin is
        // for the home network only.
        holepunch_session: None,
        rudp_sock: None,
        psn_account_id: store.account().unwrap_or_default(),
        packet_loss_max: 0.0,
        enable_idr_on_fec_failure: true,
        av_reorder_timeout_us: 0,
    };
    let mut session = Session::new(info, bridge).map_err(|e| format!("Could not set up the connection ({e})."))?;
    session.start().map_err(|e| format!("Could not start the connection ({e})."))?;

    // From here chiaki works on its own threads; this one passes commands on until the end.
    let result = loop {
        match rx.recv() {
            Ok(Command::Input(state)) => {
                let _ = session.send_controller_state(&state);
            }
            Ok(Command::Keyframe) => {
                let _ = session.request_idr(); // fails only before the stream is up
            }
            Ok(Command::Quit(error)) => break error.map_or(Ok(()), Err),
            Ok(Command::Stop) | Err(_) => break Ok(()),
        }
    };
    session.stop();
    session.fini();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::out::testing::Capture;
    use chiaki_core::Codec;

    #[test]
    fn the_stream_size_is_the_largest_preset_that_fits() {
        let size = |w, h| {
            let p = pick_profile(w, h, 60, 0);
            (p.width, p.height)
        };
        assert_eq!(size(1920, 1080), (1920, 1080));
        assert_eq!(size(3840, 2160), (1920, 1080), "the console has nothing larger");
        assert_eq!(size(1919, 1080), (1280, 720));
        assert_eq!(size(1280, 720), (1280, 720));
        assert_eq!(size(1280, 719), (960, 540));
        assert_eq!(size(960, 540), (960, 540));
        assert_eq!(size(640, 360), (640, 360));
        assert_eq!(size(64, 64), (640, 360), "the console has nothing smaller");
        assert_eq!(size(0, 0), (1280, 720), "XMouse didn't say: a size every console offers");
        assert_eq!(size(0, 1080), (1920, 1080), "one side given");
        assert_eq!(size(u32::MAX, u32::MAX), (1920, 1080));
    }

    #[test]
    fn frame_rate_bitrate_and_codec() {
        assert_eq!(pick_profile(1280, 720, 60, 0).max_fps, 60);
        assert_eq!(pick_profile(1280, 720, 240, 0).max_fps, 60);
        assert_eq!(pick_profile(1280, 720, 59, 0).max_fps, 30);
        assert_eq!(pick_profile(1280, 720, 30, 0).max_fps, 30);
        assert_eq!(pick_profile(1280, 720, 1, 0).max_fps, 30);
        assert_eq!(pick_profile(1280, 720, 0, 0).max_fps, 60);
        // 0 = the plugin decides: chiaki's usual bitrate for the size.
        assert_eq!(pick_profile(1920, 1080, 60, 0).bitrate, 15_000);
        assert_eq!(pick_profile(1280, 720, 60, 0).bitrate, 10_000);
        assert_eq!(pick_profile(1280, 720, 60, 8000).bitrate, 8000);
        assert_eq!(pick_profile(1280, 720, 60, 1).bitrate, 1000);
        assert_eq!(pick_profile(1280, 720, 60, u32::MAX).bitrate, 50_000);
        // XMouse decodes H.264 only.
        assert_eq!(pick_profile(1920, 1080, 60, 0).codec, Codec::H264);
    }

    fn bridge(tv: bool) -> (Bridge, Capture, JoinHandle<()>, Receiver<Command>) {
        let capture = Capture::default();
        let (out, writer) = crate::out::spawn(capture.clone());
        let (tx, rx) = mpsc::channel();
        let bridge = Bridge {
            out,
            tv,
            started: Instant::now(),
            assembler: Mutex::new(Assembler::default()),
            audio_channels: AtomicU8::new(2),
            audio_rate: AtomicU32::new(48_000),
            commands: Mutex::new(tx),
        };
        (bridge, capture, writer, rx)
    }

    fn sample(data: &[u8]) -> VideoFrame<'_> {
        VideoFrame { data, frames_lost: 0, frame_recovered: false }
    }

    /// What chiaki delivers, pushed through the callbacks by hand: the frames XMouse gets
    /// must be exactly the ones its example plugin would send for the same content.
    #[test]
    fn chiaki_samples_become_xmouse_media_frames() {
        let (b, capture, writer, _rx) = bridge(false);
        let header = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 0];
        let idr = [0, 0, 0, 1, 0x65, 0x88, 0, 0, 0];
        let p = [0, 0, 0, 1, 0x41, 0x9a];
        assert!(b.video_frame(sample(&header)));
        assert!(b.video_frame(sample(&idr)));
        assert!(b.video_frame(sample(&p)));
        b.event(SessionEvent::AudioStreamInfo(chiaki_core::audio::AudioHeader::set(2, 16, 48_000, 480)));
        b.audio_pcm(&[0xfc, 1, 2]);
        b.audio_pcm(&[]); // a lost packet: nothing is sent
        b.out.close();
        writer.join().unwrap();

        let frames = capture.frames();
        assert_eq!(frames.len(), 3, "{frames:?}");
        let (kind, payload) = &frames[0];
        assert_eq!(*kind, proto::FRAME_VIDEO);
        assert_eq!(&payload[..2], &[0, 1], "H.264, keyframe");
        assert_eq!(
            &payload[proto::VIDEO_HEADER..],
            &[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88],
            "parameter sets in front of the full picture, padding gone"
        );
        let (kind, payload) = &frames[1];
        assert_eq!((*kind, &payload[..2]), (proto::FRAME_VIDEO, &[0u8, 0][..]));
        assert_eq!(&payload[proto::VIDEO_HEADER..], &p);
        let (kind, payload) = &frames[2];
        assert_eq!(*kind, proto::FRAME_AUDIO);
        assert_eq!(&payload[..proto::AUDIO_HEADER], &[0, 2, 0x80, 0xbb, 0, 0], "Opus, stereo, 48000");
        assert_eq!(&payload[proto::AUDIO_HEADER..], &[0xfc, 1, 2]);
        // Timestamps never go backwards.
        let pts = |f: &(u8, Vec<u8>)| u64::from_le_bytes(f.1[2..10].try_into().unwrap());
        assert!(pts(&frames[1]) >= pts(&frames[0]));
    }

    #[test]
    fn tv_mode_sends_no_media_but_still_reports_events() {
        let (b, capture, writer, _rx) = bridge(true);
        assert!(b.video_frame(sample(&[0, 0, 0, 1, 0x65, 0x88])));
        b.audio_pcm(&[1, 2, 3]);
        b.event(SessionEvent::Connected);
        b.event(SessionEvent::Rumble { unknown: 0, left: 255, right: 0 });
        b.out.close();
        writer.join().unwrap();
        assert!(capture.frames().iter().all(|(kind, _)| *kind == proto::FRAME_CONTROL));
        let messages = capture.messages();
        assert_eq!(messages[0], serde_json::json!({ "type": "status", "phase": "connected" }));
        assert_eq!(messages[1], serde_json::json!({ "type": "rumble", "left": 1.0, "right": 0.0 }));
    }

    #[test]
    fn the_end_of_a_session_reaches_the_session_thread_with_its_reason() {
        use chiaki_core::session::QuitReason;
        let (b, _capture, _writer, rx) = bridge(false);
        b.event(SessionEvent::Quit { reason: QuitReason::Stopped, reason_str: String::new() });
        assert!(matches!(rx.try_recv(), Ok(Command::Quit(None))), "our own stop is not an error");
        b.event(SessionEvent::Quit { reason: QuitReason::SessionRequestRpInUse, reason_str: String::new() });
        match rx.try_recv() {
            Ok(Command::Quit(Some(e))) => assert!(e.contains("already in use"), "{e}"),
            _ => panic!("expected an error"),
        }
        b.event(SessionEvent::Quit { reason: QuitReason::CtrlUnknown, reason_str: " detail ".into() });
        match rx.try_recv() {
            Ok(Command::Quit(Some(e))) => assert!(e.ends_with("(detail)"), "{e}"),
            _ => panic!("expected an error"),
        }
        b.event(SessionEvent::LoginPinRequest(false));
        assert!(matches!(rx.try_recv(), Ok(Command::Quit(Some(e))) if e.contains("passcode")));
        // Events with no counterpart in XMouse are ignored.
        b.event(SessionEvent::MotionReset);
        b.event(SessionEvent::KeyboardRemoteClose);
        assert!(rx.try_recv().is_err());
        // The session thread having ended must not make a late callback panic.
        drop(rx);
        b.event(SessionEvent::Quit { reason: QuitReason::Stopped, reason_str: String::new() });
    }

    #[test]
    fn waiting_can_be_interrupted_by_stop() {
        let (tx, rx) = mpsc::channel();
        assert!(!stop_requested(&rx, Duration::from_millis(30)));
        tx.send(Command::Keyframe).unwrap();
        tx.send(Command::Input(ControllerState::default())).unwrap();
        assert!(!stop_requested(&rx, Duration::ZERO), "input before the session exists is dropped");
        tx.send(Command::Stop).unwrap();
        let started = Instant::now();
        assert!(stop_requested(&rx, Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(tx);
        assert!(stop_requested(&rx, Duration::from_secs(30)), "XMouse gone counts as stop");
    }

    #[test]
    fn starting_an_unpaired_console_ends_with_a_clear_error_and_no_network() {
        let dir = crate::store::test_dir("session-unpaired");
        let capture = Capture::default();
        let (out, writer) = crate::out::spawn(capture.clone());
        let handle = spawn(
            out.clone(),
            dir.clone(),
            StartRequest { console_id: "AABB".into(), tv: false, max_width: 1920, max_height: 1080, fps: 60, bitrate_kbps: 0 },
        );
        handle.stop();
        out.close();
        writer.join().unwrap();
        let messages = capture.messages();
        assert_eq!(messages[0]["phase"], "starting");
        let last = messages.last().unwrap();
        assert_eq!(last["phase"], "ended");
        assert_eq!(last["error"], "This console is not paired yet.");
        assert_eq!(messages.iter().filter(|m| m["phase"] == "ended").count(), 1, "ended exactly once");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
