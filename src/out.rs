//! Everything that goes to XMouse, through one queue and one writer thread.
//!
//! Several threads have things to say (the message loop, the session thread, chiaki's network
//! threads through the callbacks), and a frame must reach the pipe whole. The queue gives
//! that, and it also keeps chiaki's network threads from ever waiting on the pipe: if XMouse
//! falls behind, video and audio are dropped here instead of stalling the connection to the
//! console.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::proto::{self, FromPlugin, StatusPhase};

/// Frames waiting for the pipe. At 60 pictures and 100 sound packets a second this is about
/// two seconds; a healthy XMouse keeps it nearly empty.
const QUEUE: usize = 384;
/// Longest text put into one message, so an odd error string can't grow a control frame.
const MAX_TEXT: usize = 1000;

enum Item {
    Frame(Vec<u8>),
    /// Write what is queued before this, then finish.
    Close,
}

#[derive(Clone)]
pub struct Out {
    tx: SyncSender<Item>,
    /// The pipe is gone (XMouse exited): nothing can be delivered any more.
    broken: Arc<AtomicBool>,
}

/// Start the writer thread on `writer` (stdout in real use, a buffer in tests).
pub fn spawn(mut writer: impl Write + Send + 'static) -> (Out, JoinHandle<()>) {
    let (tx, rx) = mpsc::sync_channel::<Item>(QUEUE);
    let broken = Arc::new(AtomicBool::new(false));
    let flag = broken.clone();
    let thread = std::thread::spawn(move || {
        while let Ok(Item::Frame(frame)) = rx.recv() {
            if flag.load(Ordering::Relaxed) {
                continue; // keep emptying the queue so nobody waits on it
            }
            if writer.write_all(&frame).and_then(|_| writer.flush()).is_err() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    });
    (Out { tx, broken }, thread)
}

impl Out {
    /// Send a control message. Waits for room in the queue: these are never dropped.
    pub fn control(&self, msg: &FromPlugin) {
        if let Some(frame) = proto::control_frame(msg) {
            let _ = self.tx.send(Item::Frame(frame));
        }
    }

    /// Offer a video or audio frame. `false` if it was dropped because XMouse isn't keeping up
    /// (or is gone).
    pub fn media(&self, frame: Vec<u8>) -> bool {
        if self.broken.load(Ordering::Relaxed) {
            return false;
        }
        match self.tx.try_send(Item::Frame(frame)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Finish: the writer thread writes what is queued and ends.
    pub fn close(&self) {
        let _ = self.tx.send(Item::Close);
    }

    pub fn status(&self, phase: StatusPhase, detail: &str) {
        self.control(&FromPlugin::Status {
            phase,
            detail: clip(detail),
            error: None,
        });
    }

    /// The final status of a session. With `error` set XMouse shows it to the user.
    pub fn ended(&self, error: Option<&str>) {
        self.control(&FromPlugin::Status {
            phase: StatusPhase::Ended,
            detail: String::new(),
            error: error.map(clip),
        });
    }

    pub fn log(&self, level: &str, text: &str) {
        self.control(&FromPlugin::Log {
            level: level.to_string(),
            text: clip(text),
        });
    }
}

/// Cut a text to a sensible length (on a character boundary).
pub fn clip(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;

    /// A writer the test can look into afterwards.
    #[derive(Clone, Default)]
    pub struct Capture(pub Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Capture {
        /// Everything written so far, split back into frames.
        pub fn frames(&self) -> Vec<(u8, Vec<u8>)> {
            let bytes = self.0.lock().unwrap().clone();
            let mut r = std::io::Cursor::new(bytes);
            let mut out = Vec::new();
            while let Ok(Some(frame)) = proto::read_frame(&mut r) {
                out.push(frame);
            }
            out
        }

        /// The control messages among them, as JSON.
        pub fn messages(&self) -> Vec<serde_json::Value> {
            self.frames()
                .into_iter()
                .filter(|(kind, _)| *kind == proto::FRAME_CONTROL)
                .map(|(_, payload)| serde_json::from_slice(&payload).expect("control frames are JSON"))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Capture;
    use super::*;
    use std::time::Duration;

    #[test]
    fn frames_arrive_whole_and_in_order() {
        let capture = Capture::default();
        let (out, thread) = spawn(capture.clone());
        out.status(StatusPhase::Starting, "one");
        assert!(out.media(proto::video_frame(true, 7, &[&[0, 0, 1, 0x65, 1]]).unwrap()));
        out.log("info", "two");
        out.ended(Some("three"));
        out.close();
        thread.join().unwrap();
        let frames = capture.frames();
        assert_eq!(frames.iter().map(|f| f.0).collect::<Vec<_>>(), [1, 2, 1, 1]);
        let messages = capture.messages();
        assert_eq!(messages[0]["detail"], "one");
        assert_eq!(messages[1]["text"], "two");
        assert_eq!(messages[2]["error"], "three");
    }

    /// A pipe nobody reads: writes never return.
    struct Stuck;
    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_secs(3600));
            Ok(0)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn media_is_dropped_instead_of_waiting_when_xmouse_does_not_read() {
        let (out, _thread) = spawn(Stuck);
        let started = std::time::Instant::now();
        let mut dropped = false;
        for _ in 0..QUEUE + 8 {
            if !out.media(vec![1, 0, 0, 0, 2]) {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "the queue is bounded");
        assert!(started.elapsed() < Duration::from_secs(2), "offering media never blocks");
    }

    /// A pipe whose other end is closed.
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_pipe_does_not_block_or_panic_anything() {
        let (out, thread) = spawn(Broken);
        for i in 0..QUEUE * 3 {
            out.log("info", &format!("line {i}")); // would block for ever if the queue stopped draining
        }
        out.close();
        thread.join().unwrap();
        assert!(!out.media(vec![1]), "media is refused once the pipe is gone");
        out.log("info", "after the writer ended: silently dropped");
    }

    #[test]
    fn long_texts_are_cut_on_a_character_boundary() {
        assert_eq!(clip(&"ä".repeat(5000)).chars().count(), MAX_TEXT);
        assert_eq!(clip("short"), "short");
    }
}
