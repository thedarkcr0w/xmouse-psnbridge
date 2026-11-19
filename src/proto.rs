//! XMouse's plugin wire protocol (plugin API 1), written out from the plugin's side.
//!
//! Every frame on stdin/stdout is `u32 LE length | 1 type byte | payload`, where the length
//! counts the type byte and the payload. Control frames carry one JSON object with a `"type"`
//! field; video and audio frames (plugin to host only) carry a small binary header and the
//! encoded data.
//!
//! This file deliberately shares no code with XMouse (different repository, different
//! licence): the shapes are restated here and the tests pin them to the same expectations
//! XMouse's own tests and its example plugin use.

use std::collections::BTreeMap;
use std::io::{self, Read};

use serde::{Deserialize, Serialize};

/// The plugin API version this program speaks.
pub const API_VERSION: u32 = 1;
/// Largest frame either side accepts (type byte + payload).
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Largest control (JSON) frame. Control messages are tiny; anything bigger is not one.
pub const MAX_CONTROL: usize = 256 * 1024;

pub const FRAME_CONTROL: u8 = 1;
pub const FRAME_VIDEO: u8 = 2;
pub const FRAME_AUDIO: u8 = 3;

/// Video header: codec, flags, presentation time (u64 LE, microseconds).
pub const VIDEO_HEADER: usize = 10;
/// Audio header: codec, channels, sample rate (u32 LE).
pub const AUDIO_HEADER: usize = 6;
pub const VIDEO_CODEC_H264: u8 = 0;
pub const VIDEO_FLAG_KEYFRAME: u8 = 1;
pub const AUDIO_CODEC_OPUS: u8 = 0;

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// A length of zero: there is always at least the type byte.
    Empty,
    /// A length above [`MAX_FRAME`]. After this the stream can't be trusted any more.
    TooLarge(u32),
    /// The stream ended in the middle of a frame.
    Truncated,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "read error: {e}"),
            FrameError::Empty => write!(f, "empty frame"),
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes (the limit is {MAX_FRAME})"),
            FrameError::Truncated => write!(f, "input ended in the middle of a frame"),
        }
    }
}

/// Build one frame from a type byte and the pieces of its payload. `None` if it would be
/// larger than XMouse accepts (XMouse would end the conversation on such a frame).
pub fn encode_frame(kind: u8, parts: &[&[u8]]) -> Option<Vec<u8>> {
    let len = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    if len > MAX_FRAME {
        return None;
    }
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.push(kind);
    for p in parts {
        out.extend_from_slice(p);
    }
    Some(out)
}

/// Read one frame: its type byte and payload. `Ok(None)` is a clean end of the stream (XMouse
/// closed the pipe between two frames).
pub fn read_frame(r: &mut impl Read) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
    let mut head = [0u8; 4];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let len = u32::from_le_bytes(head);
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len as usize > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut kind = [0u8; 1];
    match r.read_exact(&mut kind) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Truncated),
        Err(e) => return Err(FrameError::Io(e)),
    }
    // Let the buffer grow as bytes really arrive, so a large announced length with nothing
    // behind it doesn't cost 8 MiB.
    let want = len as usize - 1;
    let mut payload = Vec::with_capacity(want.min(64 * 1024));
    match r.take(want as u64).read_to_end(&mut payload) {
        Ok(n) if n == want => Ok(Some((kind[0], payload))),
        Ok(_) => Err(FrameError::Truncated),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Messages XMouse sends. Fields a future XMouse might leave out have defaults, and fields it
/// might add are ignored, so a small protocol addition doesn't make this plugin deaf.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPlugin {
    Hello {
        api: u32,
        /// A folder for this plugin's own files (pairing keys, account ID).
        #[serde(default)]
        config_dir: String,
        #[serde(default)]
        host: String,
        #[serde(default)]
        host_version: String,
        #[serde(default)]
        video_codecs: Vec<String>,
        #[serde(default)]
        audio_codecs: Vec<String>,
    },
    ListConsoles,
    Pair {
        console_id: String,
        #[serde(default)]
        pin: String,
        #[serde(default)]
        extra: BTreeMap<String, String>,
    },
    Wake {
        console_id: String,
    },
    Start {
        console_id: String,
        /// "tv" (picture and sound stay on the TV; XMouse throws media away) or "pc".
        #[serde(default)]
        mode: String,
        #[serde(default)]
        max_width: u32,
        #[serde(default)]
        max_height: u32,
        #[serde(default)]
        fps: u32,
        /// 0 = the plugin decides.
        #[serde(default)]
        bitrate_kbps: u32,
    },
    /// Controller state in the Xbox layout. Sticks -1..1 with up = -1, triggers 0..1.
    Input {
        #[serde(default)]
        buttons: u32,
        #[serde(default)]
        lx: f32,
        #[serde(default)]
        ly: f32,
        #[serde(default)]
        rx: f32,
        #[serde(default)]
        ry: f32,
        #[serde(default)]
        lt: f32,
        #[serde(default)]
        rt: f32,
    },
    RequestKeyframe,
    Stop,
    /// A message type from a newer XMouse: ignored.
    #[serde(other)]
    Unknown,
}

/// Parse one control payload from XMouse. `None` for anything that isn't a usable message
/// (too big, not JSON, wrong shape): the caller skips it and carries on.
pub fn parse_control(payload: &[u8]) -> Option<ToPlugin> {
    if payload.len() > MAX_CONTROL {
        return None;
    }
    serde_json::from_slice(payload).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusPhase {
    Starting,
    Waiting,
    Connecting,
    Connected,
    Ended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleState {
    On,
    Standby,
    Off,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConsoleEntry {
    pub id: String,
    pub name: String,
    pub model: String,
    pub state: ConsoleState,
    pub needs_pairing: bool,
}

/// Messages this plugin sends to XMouse.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromPlugin {
    Hello {
        name: String,
        version: String,
        api: u32,
        capabilities: Vec<String>,
    },
    Consoles {
        consoles: Vec<ConsoleEntry>,
    },
    PairResult {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    WakeResult {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// `ended` is final; with `error` set XMouse shows it to the user.
    Status {
        phase: StatusPhase,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Both 0..1.
    Rumble {
        left: f32,
        right: f32,
    },
    Log {
        level: String,
        text: String,
    },
}

/// A control message as a ready-to-write frame.
pub fn control_frame(msg: &FromPlugin) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(msg).ok()?;
    if json.len() > MAX_CONTROL {
        return None;
    }
    encode_frame(FRAME_CONTROL, &[&json])
}

/// One H.264 access unit (Annex-B) as a frame. `parts` are concatenated: it lets the caller
/// put stored parameter sets in front of a picture without copying twice.
pub fn video_frame(keyframe: bool, pts_us: u64, parts: &[&[u8]]) -> Option<Vec<u8>> {
    if parts.iter().all(|p| p.is_empty()) {
        return None; // XMouse counts a video frame without data as a bad frame
    }
    let mut head = [0u8; VIDEO_HEADER];
    head[0] = VIDEO_CODEC_H264;
    head[1] = if keyframe { VIDEO_FLAG_KEYFRAME } else { 0 };
    head[2..].copy_from_slice(&pts_us.to_le_bytes());
    let mut all: Vec<&[u8]> = Vec::with_capacity(1 + parts.len());
    all.push(&head);
    all.extend_from_slice(parts);
    encode_frame(FRAME_VIDEO, &all)
}

/// One Opus packet as a frame.
pub fn audio_frame(channels: u8, sample_rate: u32, packet: &[u8]) -> Option<Vec<u8>> {
    if packet.is_empty() {
        return None;
    }
    let mut head = [0u8; AUDIO_HEADER];
    head[0] = AUDIO_CODEC_OPUS;
    head[1] = channels;
    head[2..].copy_from_slice(&sample_rate.to_le_bytes());
    encode_frame(FRAME_AUDIO, &[&head, packet])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn frames_round_trip_and_the_length_counts_the_type_byte() {
        let mut buf = encode_frame(FRAME_CONTROL, &[b"{}"]).unwrap();
        buf.extend(encode_frame(9, &[]).unwrap());
        assert_eq!(&buf[..7], &[3, 0, 0, 0, 1, b'{', b'}']);
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap(), Some((FRAME_CONTROL, b"{}".to_vec())));
        assert_eq!(read_frame(&mut r).unwrap(), Some((9, Vec::new())));
        assert_eq!(read_frame(&mut r).unwrap(), None, "clean end between frames");
    }

    #[test]
    fn bad_lengths_are_refused_before_reading_the_payload() {
        let e = read_frame(&mut Cursor::new(vec![0, 0, 0, 0, 1])).unwrap_err();
        assert!(matches!(e, FrameError::Empty));
        let over = (MAX_FRAME as u32 + 1).to_le_bytes().to_vec();
        let e = read_frame(&mut Cursor::new(over)).unwrap_err();
        assert!(matches!(e, FrameError::TooLarge(n) if n as usize == MAX_FRAME + 1));
        let e = read_frame(&mut Cursor::new(vec![0xff; 16])).unwrap_err();
        assert!(matches!(e, FrameError::TooLarge(u32::MAX)));
        // Exactly the limit is fine.
        let mut big = (MAX_FRAME as u32).to_le_bytes().to_vec();
        big.push(FRAME_VIDEO);
        big.resize(4 + MAX_FRAME, 7);
        let (kind, payload) = read_frame(&mut Cursor::new(big)).unwrap().unwrap();
        assert_eq!((kind, payload.len()), (FRAME_VIDEO, MAX_FRAME - 1));
        // The sending side refuses to build an oversize frame too.
        assert!(encode_frame(1, &[&vec![0u8; MAX_FRAME]]).is_none());
    }

    #[test]
    fn a_stream_that_ends_mid_frame_is_truncated() {
        for bytes in [vec![5u8, 0], vec![5, 0, 0, 0], vec![5, 0, 0, 0, 1, b'{']] {
            let e = read_frame(&mut Cursor::new(bytes.clone())).unwrap_err();
            assert!(matches!(e, FrameError::Truncated), "{bytes:?} gave {e:?}");
        }
        // A huge announced length with nothing behind it fails without a huge allocation.
        let mut lie = (MAX_FRAME as u32).to_le_bytes().to_vec();
        lie.extend_from_slice(&[FRAME_VIDEO, 1, 2, 3]);
        assert!(matches!(read_frame(&mut Cursor::new(lie)).unwrap_err(), FrameError::Truncated));
    }

    /// The exact JSON XMouse's own tests document for its messages (src/plugin/proto.rs,
    /// `host_messages_have_the_documented_json_shape`).
    #[test]
    fn host_messages_parse_from_the_documented_json() {
        let p = |v: serde_json::Value| parse_control(v.to_string().as_bytes());
        assert_eq!(p(json!({ "type": "list_consoles" })), Some(ToPlugin::ListConsoles));
        assert_eq!(p(json!({ "type": "request_keyframe" })), Some(ToPlugin::RequestKeyframe));
        assert_eq!(p(json!({ "type": "stop" })), Some(ToPlugin::Stop));
        assert_eq!(
            p(json!({ "type": "hello", "api": 1, "config_dir": "C:\\data", "host": "XMouse",
                      "host_version": "1.0.2", "video_codecs": ["h264"], "audio_codecs": ["opus"] })),
            Some(ToPlugin::Hello {
                api: 1,
                config_dir: "C:\\data".into(),
                host: "XMouse".into(),
                host_version: "1.0.2".into(),
                video_codecs: vec!["h264".into()],
                audio_codecs: vec!["opus".into()],
            })
        );
        assert_eq!(
            p(json!({ "type": "start", "console_id": "c1", "mode": "pc", "max_width": 1920,
                      "max_height": 1080, "fps": 60, "bitrate_kbps": 0 })),
            Some(ToPlugin::Start {
                console_id: "c1".into(),
                mode: "pc".into(),
                max_width: 1920,
                max_height: 1080,
                fps: 60,
                bitrate_kbps: 0,
            })
        );
        assert_eq!(
            p(json!({ "type": "input", "buttons": 16, "lx": 0.5, "ly": -1.0, "rx": 0.0,
                      "ry": 0.0, "lt": 0.0, "rt": 1.0 })),
            Some(ToPlugin::Input { buttons: 16, lx: 0.5, ly: -1.0, rx: 0.0, ry: 0.0, lt: 0.0, rt: 1.0 })
        );
        assert_eq!(
            p(json!({ "type": "pair", "console_id": "c1", "pin": "12345678",
                      "extra": { "account": "abc" } })),
            Some(ToPlugin::Pair {
                console_id: "c1".into(),
                pin: "12345678".into(),
                extra: BTreeMap::from([("account".to_string(), "abc".to_string())]),
            })
        );
        assert_eq!(p(json!({ "type": "wake", "console_id": "c1" })), Some(ToPlugin::Wake { console_id: "c1".into() }));
    }

    #[test]
    fn unknown_and_malformed_host_messages_do_not_panic() {
        let p = |s: &str| parse_control(s.as_bytes());
        assert_eq!(p(r#"{"type":"from_the_future","x":1}"#), Some(ToPlugin::Unknown));
        // Extra fields are ignored, missing optional ones get defaults.
        assert_eq!(
            p(r#"{"type":"start","console_id":"c","new_field":[1,2]}"#),
            Some(ToPlugin::Start {
                console_id: "c".into(),
                mode: String::new(),
                max_width: 0,
                max_height: 0,
                fps: 0,
                bitrate_kbps: 0
            })
        );
        for bad in [
            "",
            "{",
            "not json",
            "[1,2,3]",
            "null",
            r#"{"no_type":true}"#,
            r#"{"type":5}"#,
            r#"{"type":"pair"}"#,
            r#"{"type":"input","buttons":"many"}"#,
            r#"{"type":"input","buttons":-1}"#,
            r#"{"type":"start","console_id":7}"#,
        ] {
            assert_eq!(p(bad), None, "{bad:?}");
        }
        assert_eq!(parse_control(&vec![b' '; MAX_CONTROL + 1]), None);
        assert_eq!(parse_control(&[0xff, 0xfe, 0x00]), None);
    }

    /// What XMouse's parser (and its tests, `plugin_messages_parse_with_defaults_and_unknowns`)
    /// expects from a plugin, field for field.
    #[test]
    fn plugin_messages_have_the_json_shape_xmouse_parses() {
        let v = |m: FromPlugin| serde_json::to_value(m).unwrap();
        assert_eq!(
            v(FromPlugin::Hello { name: "P".into(), version: "1".into(), api: 1, capabilities: vec!["pairing".into()] }),
            json!({ "type": "hello", "name": "P", "version": "1", "api": 1, "capabilities": ["pairing"] })
        );
        assert_eq!(
            v(FromPlugin::Consoles {
                consoles: vec![ConsoleEntry {
                    id: "a".into(),
                    name: "Living room".into(),
                    model: "PS5".into(),
                    state: ConsoleState::Standby,
                    needs_pairing: true,
                }]
            }),
            json!({ "type": "consoles", "consoles": [
                { "id": "a", "name": "Living room", "model": "PS5", "state": "standby", "needs_pairing": true } ] })
        );
        assert_eq!(v(FromPlugin::PairResult { ok: true, error: None }), json!({ "type": "pair_result", "ok": true }));
        assert_eq!(
            v(FromPlugin::PairResult { ok: false, error: Some("Wrong PIN".into()) }),
            json!({ "type": "pair_result", "ok": false, "error": "Wrong PIN" })
        );
        assert_eq!(v(FromPlugin::WakeResult { ok: true, error: None }), json!({ "type": "wake_result", "ok": true }));
        assert_eq!(
            v(FromPlugin::Status { phase: StatusPhase::Ended, detail: String::new(), error: Some("console turned off".into()) }),
            json!({ "type": "status", "phase": "ended", "error": "console turned off" })
        );
        assert_eq!(
            v(FromPlugin::Status { phase: StatusPhase::Waiting, detail: "Waking…".into(), error: None }),
            json!({ "type": "status", "phase": "waiting", "detail": "Waking…" })
        );
        for (phase, name) in [
            (StatusPhase::Starting, "starting"),
            (StatusPhase::Connecting, "connecting"),
            (StatusPhase::Connected, "connected"),
        ] {
            assert_eq!(v(FromPlugin::Status { phase, detail: String::new(), error: None })["phase"], name);
        }
        for (state, name) in [
            (ConsoleState::On, "on"),
            (ConsoleState::Standby, "standby"),
            (ConsoleState::Off, "off"),
            (ConsoleState::Unknown, "unknown"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), name);
        }
        assert_eq!(
            v(FromPlugin::Rumble { left: 1.0, right: 0.5 }),
            json!({ "type": "rumble", "left": 1.0, "right": 0.5 })
        );
        assert_eq!(
            v(FromPlugin::Log { level: "info".into(), text: "hi".into() }),
            json!({ "type": "log", "level": "info", "text": "hi" })
        );
    }

    /// Byte for byte what XMouse's example plugin writes and what XMouse's `decode` reads.
    #[test]
    fn media_frames_have_the_documented_headers() {
        let frame = video_frame(true, 0x0102_0304_0506_0708, &[&[0, 0, 0, 1, 0x67], &[0, 0, 0, 1, 0x65, 9]]).unwrap();
        let (kind, payload) = read_frame(&mut Cursor::new(frame)).unwrap().unwrap();
        assert_eq!(kind, FRAME_VIDEO);
        assert_eq!(&payload[..2], &[0, 1], "codec h264, keyframe flag");
        assert_eq!(&payload[2..VIDEO_HEADER], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&payload[VIDEO_HEADER..], &[0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x65, 9]);
        let frame = video_frame(false, 5, &[&[0, 0, 1, 0x41]]).unwrap();
        assert_eq!(frame[4..7], [FRAME_VIDEO, 0, 0], "no keyframe flag");

        let frame = audio_frame(2, 48_000, &[1, 2, 3]).unwrap();
        let (kind, payload) = read_frame(&mut Cursor::new(frame)).unwrap().unwrap();
        assert_eq!(kind, FRAME_AUDIO);
        assert_eq!(&payload[..2], &[0, 2], "codec opus, two channels");
        assert_eq!(&payload[2..AUDIO_HEADER], &48_000u32.to_le_bytes());
        assert_eq!(&payload[AUDIO_HEADER..], &[1, 2, 3]);

        // XMouse treats media frames without data as broken, so none are built.
        assert!(video_frame(true, 0, &[]).is_none());
        assert!(video_frame(true, 0, &[&[], &[]]).is_none());
        assert!(audio_frame(2, 48_000, &[]).is_none());
        // And nothing larger than XMouse's frame limit.
        assert!(video_frame(false, 0, &[&vec![0u8; MAX_FRAME]]).is_none());
    }
}
