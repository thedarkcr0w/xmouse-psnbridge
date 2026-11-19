//! The conversation with XMouse: read its messages one by one and answer each.

use std::io::Read;
use std::path::PathBuf;

use crate::consoles;
use crate::mapping;
use crate::out::Out;
use crate::proto::{self, FrameError, FromPlugin, ToPlugin};
use crate::session::{self, Command, StartRequest};

pub const PLUGIN_NAME: &str = "PSN Bridge";

/// Serve XMouse until it says `stop` or closes the pipe. `input` is stdin in real use.
///
/// Nothing XMouse sends can make this panic: frames that aren't messages are skipped, and a
/// stream that stops making sense as frames ends the conversation (as XMouse does with us).
pub fn serve(mut input: impl Read, out: &Out) {
    // Until `hello` names the folder, use one next to the program (XMouse always sends hello
    // first; this only matters for an XMouse that doesn't).
    let mut data_dir = PathBuf::from("data");
    let mut h264_ok = true;
    let mut active: Option<session::Handle> = None;

    loop {
        let (kind, payload) = match proto::read_frame(&mut input) {
            Ok(Some(frame)) => frame,
            Ok(None) => break, // XMouse closed the pipe
            Err(FrameError::Io(_)) => break,
            Err(e) => {
                // After a bad length there is no telling where the next frame starts.
                eprintln!("psnbridge: stopping: {e}");
                break;
            }
        };
        if kind != proto::FRAME_CONTROL {
            continue; // a frame type from a newer XMouse
        }
        let Some(message) = proto::parse_control(&payload) else {
            continue;
        };
        match message {
            ToPlugin::Hello { api, config_dir, video_codecs, .. } => {
                if !config_dir.trim().is_empty() {
                    data_dir = PathBuf::from(config_dir);
                }
                // An empty list means "didn't say" (an older or newer host): assume H.264.
                h264_ok = video_codecs.is_empty() || video_codecs.iter().any(|c| c.eq_ignore_ascii_case("h264"));
                if api != proto::API_VERSION {
                    eprintln!("psnbridge: XMouse speaks plugin API {api}, this plugin speaks {}", proto::API_VERSION);
                }
                // The version answered is always our own: XMouse decides whether it can
                // work with it.
                out.control(&FromPlugin::Hello {
                    name: PLUGIN_NAME.into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    api: proto::API_VERSION,
                    capabilities: vec!["pairing".into(), "wake".into(), "rumble".into()],
                });
            }
            ToPlugin::ListConsoles => {
                let (consoles, problem) = consoles::list(&data_dir);
                if let Some(p) = problem {
                    out.log("warn", &p);
                }
                out.control(&FromPlugin::Consoles { consoles });
            }
            ToPlugin::Pair { console_id, pin, extra } => {
                let result = consoles::pair(&data_dir, &console_id, &pin, &extra);
                out.control(&FromPlugin::PairResult {
                    ok: result.is_ok(),
                    error: result.err().map(|e| crate::out::clip(&e)),
                });
            }
            ToPlugin::Wake { console_id } => {
                let result = consoles::wake(&data_dir, &console_id);
                out.control(&FromPlugin::WakeResult {
                    ok: result.is_ok(),
                    error: result.err().map(|e| crate::out::clip(&e)),
                });
            }
            ToPlugin::Start { console_id, mode, max_width, max_height, fps, bitrate_kbps } => {
                // A session that has ended by itself leaves its handle behind: clear it.
                if active.as_ref().is_some_and(|h| h.finished()) {
                    if let Some(h) = active.take() {
                        h.stop();
                    }
                }
                if active.is_some() {
                    continue; // already playing: a second start changes nothing
                }
                let tv = mode.eq_ignore_ascii_case("tv");
                if !tv && !h264_ok {
                    out.ended(Some("This XMouse can't show H.264 video, which is what the console sends."));
                    continue;
                }
                let request = StartRequest { console_id, tv, max_width, max_height, fps, bitrate_kbps };
                active = Some(session::spawn(out.clone(), data_dir.clone(), request));
            }
            ToPlugin::Input { buttons, lx, ly, rx, ry, lt, rt } => {
                if let Some(handle) = &active {
                    handle.send(Command::Input(mapping::controller_state(buttons, lx, ly, rx, ry, lt, rt)));
                }
            }
            ToPlugin::RequestKeyframe => {
                if let Some(handle) = &active {
                    handle.send(Command::Keyframe);
                }
            }
            ToPlugin::Stop => break,
            ToPlugin::Unknown => {} // a message from a newer XMouse
        }
    }

    // Asked to stop, or XMouse went away: say goodbye to the console properly. The session
    // thread reports `ended` before it finishes.
    if let Some(handle) = active.take() {
        handle.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::out::testing::Capture;
    use serde_json::{json, Value};
    use std::io::Cursor;

    fn frame(message: Value) -> Vec<u8> {
        proto::encode_frame(proto::FRAME_CONTROL, &[message.to_string().as_bytes()]).unwrap()
    }

    /// Feed bytes "from XMouse" and collect every frame the plugin wrote back.
    fn converse(input: Vec<u8>) -> Capture {
        let capture = Capture::default();
        let (out, writer) = crate::out::spawn(capture.clone());
        serve(Cursor::new(input), &out);
        out.close();
        writer.join().unwrap();
        capture
    }

    fn hello(dir: &std::path::Path) -> Vec<u8> {
        frame(json!({ "type": "hello", "api": 1, "config_dir": dir.to_string_lossy(), "host": "XMouse",
                      "host_version": "1.0.2", "video_codecs": ["h264"], "audio_codecs": ["opus"] }))
    }

    /// The same opening XMouse's host performs with its example plugin: hello in, hello out
    /// with the same API number, a name and a version.
    #[test]
    fn hello_is_answered_like_the_example_plugin_answers_it() {
        let dir = crate::store::test_dir("bridge-hello");
        let mut input = hello(&dir);
        input.extend(frame(json!({ "type": "stop" })));
        let messages = converse(input).messages();
        assert_eq!(
            messages,
            [json!({ "type": "hello", "name": "PSN Bridge", "version": env!("CARGO_PKG_VERSION"),
                     "api": 1, "capabilities": ["pairing", "wake", "rumble"] })]
        );
    }

    #[test]
    fn requests_that_cannot_work_get_their_answer_with_a_reason() {
        let dir = crate::store::test_dir("bridge-refuse");
        let mut input = hello(&dir);
        // No account ID anywhere: the answer must say so (and come before any network use).
        input.extend(frame(json!({ "type": "pair", "console_id": "AABBCCDDEEFF", "pin": "12345678", "extra": {} })));
        input.extend(frame(json!({ "type": "pair", "console_id": "AABBCCDDEEFF", "pin": "1",
                                   "extra": { "account_id": "ASNFZ4mrze8=" } })));
        input.extend(frame(json!({ "type": "wake", "console_id": "AABBCCDDEEFF" })));
        input.extend(frame(json!({ "type": "wake", "console_id": "../../etc" })));
        // Not paired: the session ends at once with the reason.
        input.extend(frame(json!({ "type": "start", "console_id": "AABBCCDDEEFF", "mode": "pc",
                                   "max_width": 1920, "max_height": 1080, "fps": 60, "bitrate_kbps": 0 })));
        input.extend(frame(json!({ "type": "input", "buttons": 16, "lx": 0.5, "ly": -1.0, "rx": 0.0,
                                   "ry": 0.0, "lt": 0.0, "rt": 1.0 })));
        input.extend(frame(json!({ "type": "request_keyframe" })));
        input.extend(frame(json!({ "type": "stop" })));
        let messages = converse(input).messages();

        assert_eq!(messages[0]["type"], "hello");
        assert_eq!(messages[1]["type"], "pair_result");
        assert_eq!(messages[1]["ok"], false);
        assert!(messages[1]["error"].as_str().unwrap().contains("No PSN account ID is set"));
        assert_eq!(messages[2]["ok"], false);
        assert!(messages[2]["error"].as_str().unwrap().contains("PIN"));
        assert_eq!((&messages[3]["type"], &messages[3]["ok"]), (&json!("wake_result"), &json!(false)));
        assert!(messages[3]["error"].as_str().unwrap().contains("not paired"));
        assert_eq!(messages[4]["error"], "Unknown console.");
        let statuses: Vec<&Value> = messages.iter().filter(|m| m["type"] == "status").collect();
        assert_eq!(statuses.first().unwrap()["phase"], "starting");
        assert_eq!(statuses.last().unwrap()["phase"], "ended");
        assert_eq!(statuses.last().unwrap()["error"], "This console is not paired yet.");
        assert_eq!(statuses.iter().filter(|m| m["phase"] == "ended").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The nonsense XMouse's example plugin is built to survive, sent to this plugin.
    #[test]
    fn garbage_between_messages_is_skipped() {
        let dir = crate::store::test_dir("bridge-garbage");
        let mut input = Vec::new();
        input.extend(proto::encode_frame(proto::FRAME_CONTROL, &[b"{ this is not json"]).unwrap());
        input.extend(proto::encode_frame(proto::FRAME_VIDEO, &[&[0, 0]]).unwrap());
        input.extend(proto::encode_frame(250, &[b"a frame type nobody knows"]).unwrap());
        input.extend(frame(json!({ "type": "a_message_nobody_knows", "x": 1 })));
        input.extend(frame(json!({ "type": "input", "buttons": "all of them" })));
        input.extend(frame(json!({ "type": "input", "buttons": 16 }))); // no session: ignored
        input.extend(frame(json!({ "type": "request_keyframe" })));
        input.extend(frame(json!({ "type": "start" }))); // no console named
        input.extend(frame(json!([1, 2, 3])));
        input.extend(hello(&dir));
        // No `stop`: the input simply ends, like XMouse exiting.
        let messages = converse(input).messages();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert_eq!(messages[0]["type"], "hello");
    }

    #[test]
    fn a_broken_stream_ends_the_conversation_quietly() {
        let dir = crate::store::test_dir("bridge-broken");
        // An impossible length after a good message.
        let mut input = hello(&dir);
        input.extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
        input.extend(hello(&dir)); // never looked at
        assert_eq!(converse(input).messages().len(), 1);
        // Cut off in the middle of a frame.
        let mut input = hello(&dir);
        input.extend_from_slice(&1000u32.to_le_bytes());
        input.extend_from_slice(&[proto::FRAME_CONTROL, b'{']);
        assert_eq!(converse(input).messages().len(), 1);
        // A zero length, and nothing at all.
        assert!(converse(vec![0, 0, 0, 0]).messages().is_empty());
        assert!(converse(Vec::new()).messages().is_empty());
    }

    #[test]
    fn a_host_without_h264_is_told_why_pc_mode_cannot_start() {
        let dir = crate::store::test_dir("bridge-codec");
        let mut input = frame(json!({ "type": "hello", "api": 1, "config_dir": dir.to_string_lossy(),
                                      "video_codecs": ["av1"], "audio_codecs": ["opus"] }));
        input.extend(frame(json!({ "type": "start", "console_id": "AABB", "mode": "pc" })));
        let messages = converse(input).messages();
        assert_eq!(messages[1]["phase"], "ended");
        assert!(messages[1]["error"].as_str().unwrap().contains("H.264"));
    }
}
