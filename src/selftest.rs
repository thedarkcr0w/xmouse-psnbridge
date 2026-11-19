//! `--self-test`: a quick check that this build is sound, for people who install the program
//! without a Rust toolchain. It talks to itself only: no network, no console, no sound, and
//! nothing outside a temporary folder is written.

use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{json, Value};

use crate::account;
use crate::h264::Assembler;
use crate::mapping;
use crate::proto;
use crate::store::{self, Pairing, Store};

/// Collects what the plugin writes, in place of the pipe to XMouse.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn check(condition: bool, what: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(what.to_string())
    }
}

fn message(value: Value) -> Vec<u8> {
    proto::encode_frame(proto::FRAME_CONTROL, &[value.to_string().as_bytes()]).unwrap_or_default()
}

/// Run a whole conversation in memory and return the plugin's control messages.
fn converse(input: Vec<u8>) -> Result<Vec<Value>, String> {
    let capture = Capture::default();
    let (out, writer) = crate::out::spawn(capture.clone());
    crate::bridge::serve(Cursor::new(input), &out);
    out.close();
    writer.join().map_err(|_| "the writer thread failed".to_string())?;
    let bytes = capture.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut reader = Cursor::new(bytes);
    let mut messages = Vec::new();
    while let Some((kind, payload)) = proto::read_frame(&mut reader).map_err(|e| e.to_string())? {
        if kind == proto::FRAME_CONTROL {
            messages.push(serde_json::from_slice(&payload).map_err(|e| e.to_string())?);
        }
    }
    Ok(messages)
}

fn framing() -> Result<(), String> {
    let frame = proto::encode_frame(proto::FRAME_CONTROL, &[b"{}"]).ok_or("frame not built")?;
    check(frame == [3, 0, 0, 0, 1, b'{', b'}'], "frame layout")?;
    let video = proto::video_frame(true, 1, &[&[0, 0, 1, 0x65, 1]]).ok_or("video frame not built")?;
    check(video[4..7] == [proto::FRAME_VIDEO, 0, 1], "video header")?;
    let audio = proto::audio_frame(2, 48_000, &[1]).ok_or("audio frame not built")?;
    check(audio[4..11] == [proto::FRAME_AUDIO, 0, 2, 0x80, 0xbb, 0, 0], "audio header")?;
    check(proto::read_frame(&mut Cursor::new(vec![0xff; 8])).is_err(), "oversize frame refused")
}

fn conversation(dir: &std::path::Path) -> Result<(), String> {
    let mut input = message(json!({ "type": "hello", "api": 1, "config_dir": dir.to_string_lossy(),
                                    "host": "self-test", "host_version": "0", "video_codecs": ["h264"],
                                    "audio_codecs": ["opus"] }));
    input.extend(proto::encode_frame(proto::FRAME_CONTROL, &[b"not json"]).unwrap_or_default());
    input.extend(message(json!({ "type": "pair", "console_id": "SELFTEST", "pin": "12345678" })));
    input.extend(message(json!({ "type": "start", "console_id": "SELFTEST", "mode": "pc" })));
    input.extend(message(json!({ "type": "stop" })));
    let messages = converse(input)?;
    check(messages.first().is_some_and(|m| m["type"] == "hello" && m["api"] == 1), "hello answered")?;
    check(
        messages.iter().any(|m| m["type"] == "pair_result" && m["ok"] == false && m["error"].is_string()),
        "pairing without an account ID refused with a reason",
    )?;
    check(
        messages.iter().filter(|m| m["type"] == "status" && m["phase"] == "ended").count() == 1,
        "an unpaired console ends the session exactly once",
    )
}

fn controller() -> Result<(), String> {
    for (xbox, playstation, from, to) in mapping::BUTTON_TABLE {
        check(mapping::map_buttons(xbox) == playstation, &format!("{from} -> {to}"))?;
    }
    let state = mapping::controller_state(mapping::XBOX_A, 1.0, -1.0, f32::NAN, 0.0, 0.5, 2.0);
    check(
        (state.left_x, state.left_y, state.right_x, state.l2_state, state.r2_state) == (32767, -32767, 0, 128, 255),
        "sticks and triggers",
    )
}

fn account_ids() -> Result<(), String> {
    let id = [1u8, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
    check(account::parse_account_id("ASNFZ4mrze8=") == Ok(id), "base64")?;
    check(account::parse_account_id("0123456789abcdef") == Ok(id), "hex")?;
    check(account::parse_account_id("nonsense").is_err(), "nonsense refused")
}

fn persistence(dir: &std::path::Path) -> Result<(), String> {
    let mut store = Store::default();
    store.set_account(&[9; 8]);
    store.consoles.insert(
        "SELFTEST".into(),
        Pairing {
            name: "Self test".into(),
            ps5: true,
            target: 1_000_100,
            regist_key: store::to_hex(&[0x31; 16]),
            rp_key: store::to_hex(&[0x02; 16]),
            mac: String::new(),
            last_host: String::new(),
        },
    );
    store.save(dir)?;
    let (back, problem) = Store::load(dir);
    check(problem.is_none() && back == store, "settings read back as written")
}

fn video() -> Result<(), String> {
    let mut assembler = Assembler::default();
    check(assembler.push(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]).is_none(), "parameter sets held back")?;
    let unit = assembler.push(&[0, 0, 0, 1, 0x65, 3, 0, 0]).ok_or("full picture not passed on")?;
    check(unit.keyframe && unit.prefix.len() == 12 && unit.body.len() == 6, "full picture gets the parameter sets")
}

/// Run every check; `true` if all passed. Results go to stdout, one line each.
pub fn run() -> bool {
    let dir = std::env::temp_dir().join(format!("xmouse-psnbridge-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let checks: [(&str, Box<dyn Fn() -> Result<(), String> + '_>); 6] = [
        ("protocol framing", Box::new(framing)),
        ("conversation with XMouse", Box::new(|| conversation(&dir.join("conversation")))),
        ("controller mapping", Box::new(controller)),
        ("account ID parsing", Box::new(account_ids)),
        ("settings round trip", Box::new(|| persistence(&dir.join("settings")))),
        ("video access units", Box::new(video)),
    ];
    let mut all_ok = true;
    for (name, check) in &checks {
        match check() {
            Ok(()) => println!("ok      {name}"),
            Err(e) => {
                all_ok = false;
                println!("FAILED  {name}: {e}");
            }
        }
    }
    drop(checks);
    let _ = std::fs::remove_dir_all(&dir);
    println!("{}", if all_ok { "self-test passed" } else { "self-test FAILED" });
    all_ok
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_self_test_passes() {
        assert!(super::run());
    }
}
