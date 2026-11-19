//! What the bridge remembers between runs: the PSN account ID and one pairing record per
//! console. It lives in one JSON file in the folder XMouse hands over in `hello.config_dir`.
//!
//! The pairing keys are secrets (whoever has them can start Remote Play on that console from
//! the same network), so the file stays in the user's own profile and is never logged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::account::{self, AccountId};

const FILE_NAME: &str = "psnbridge.json";
/// The real file is a few hundred bytes per console; anything bigger is not ours.
const MAX_FILE: u64 = 1024 * 1024;
/// The registration key and the session key are both 16 bytes.
pub const KEY_SIZE: usize = 16;

/// One paired console.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pairing {
    /// The console's own name, for lists while it is switched off.
    #[serde(default)]
    pub name: String,
    pub ps5: bool,
    /// The protocol generation the console spoke when it was paired (chiaki's "target" number).
    #[serde(default)]
    pub target: u32,
    /// Hex of the 16-byte registration key ("RegistKey"): proves to the console who we are.
    pub regist_key: String,
    /// Hex of the 16-byte key the session encryption starts from ("RP-Key" / "morning").
    pub rp_key: String,
    /// Hex of the console's network card address, as the console reported it.
    #[serde(default)]
    pub mac: String,
    /// Where the console was last seen, tried when a broadcast search doesn't reach it.
    #[serde(default)]
    pub last_host: String,
}

impl Pairing {
    pub fn regist_key_bytes(&self) -> Option<[u8; KEY_SIZE]> {
        from_hex(&self.regist_key)
    }

    pub fn rp_key_bytes(&self) -> Option<[u8; KEY_SIZE]> {
        from_hex(&self.rp_key)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    /// Base64 of the eight account ID bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Pairing records by console ID (the ID the console announces on the network).
    #[serde(default)]
    pub consoles: BTreeMap<String, Pairing>,
}

impl Store {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(FILE_NAME)
    }

    /// Read the file in `dir`. A missing file is an empty store. A file that can't be used is
    /// also an empty store, with the reason in the second value so it can be logged: refusing
    /// to start over a damaged settings file would leave the user with no way forward.
    pub fn load(dir: &Path) -> (Store, Option<String>) {
        let file = Self::path(dir);
        let size = match std::fs::metadata(&file) {
            Ok(m) => m.len(),
            Err(_) => return (Store::default(), None),
        };
        if size > MAX_FILE {
            return (Store::default(), Some(format!("{FILE_NAME} is too large ({size} bytes); ignored")));
        }
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => return (Store::default(), Some(format!("can't read {FILE_NAME}: {e}"))),
        };
        match serde_json::from_str::<Store>(&text) {
            Ok(mut store) => {
                // Records that can't work (edited by hand, cut off) are dropped here, so the
                // rest of the program can rely on what it finds.
                store
                    .consoles
                    .retain(|id, p| valid_console_id(id) && p.regist_key_bytes().is_some() && p.rp_key_bytes().is_some());
                (store, None)
            }
            Err(e) => (Store::default(), Some(format!("{FILE_NAME} is not valid: {e}"))),
        }
    }

    /// Write the file. It is written next to the real one and then moved over it, so a crash
    /// in the middle never leaves half a file where the pairing keys were.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
        let text = serde_json::to_string_pretty(self).map_err(|e| format!("Can't write settings: {e}"))?;
        let file = Self::path(dir);
        let tmp = dir.join(format!("{FILE_NAME}.tmp"));
        std::fs::write(&tmp, text).map_err(|e| format!("Can't write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &file).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Can't write {}: {e}", file.display())
        })
    }

    pub fn account(&self) -> Option<AccountId> {
        account::parse_account_id(self.account_id.as_deref()?).ok()
    }

    pub fn set_account(&mut self, id: &AccountId) {
        self.account_id = Some(account::to_base64(id));
    }
}

/// Console IDs come from the network and from XMouse, and end up as keys in the settings
/// file and in log lines: only plain letters and digits, and not endless.
pub fn valid_console_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exactly `N` bytes of hex, or nothing.
pub fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    // Checked digit by digit: the number parser alone would also take a "+" sign.
    if text.len() != N * 2 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xmouse-psnbridge-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairing() -> Pairing {
        Pairing {
            name: "Living room PS5".into(),
            ps5: true,
            target: 1_000_100,
            regist_key: to_hex(b"4f6a2b1c\0\0\0\0\0\0\0\0"),
            rp_key: "00112233445566778899aabbccddeeff".into(),
            mac: "aabbccddeeff".into(),
            last_host: "192.168.1.50".into(),
        }
    }

    #[test]
    fn everything_survives_a_save_and_a_load() {
        let dir = test_dir("roundtrip");
        // The folder doesn't exist yet: XMouse may hand over one it could not create.
        let nested = dir.join("data");
        assert_eq!(Store::load(&nested), (Store::default(), None));

        let mut store = Store::default();
        store.set_account(&[1, 2, 3, 4, 5, 6, 7, 8]);
        store.consoles.insert("AABBCCDDEEFF".into(), pairing());
        store.consoles.insert("001122334455".into(), Pairing { ps5: false, target: 1000, ..pairing() });
        store.save(&nested).unwrap();

        let (back, problem) = Store::load(&nested);
        assert_eq!(problem, None);
        assert_eq!(back, store);
        assert_eq!(back.account(), Some([1, 2, 3, 4, 5, 6, 7, 8]));
        let p = &back.consoles["AABBCCDDEEFF"];
        assert_eq!(p.regist_key_bytes().unwrap(), *b"4f6a2b1c\0\0\0\0\0\0\0\0");
        assert_eq!(p.rp_key_bytes().unwrap()[15], 0xff);
        assert!(!nested.join("psnbridge.json.tmp").exists(), "no temporary file left behind");

        // Saving again replaces the file (rename over an existing file).
        let mut second = back.clone();
        second.consoles.remove("001122334455");
        second.save(&nested).unwrap();
        assert_eq!(Store::load(&nested).0, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_is_an_empty_store_with_a_reason() {
        let dir = test_dir("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        for bad in ["{", "[1, 2]", "\"text\"", "{\"consoles\": 5}", "\u{0}\u{1}"] {
            std::fs::write(Store::path(&dir), bad).unwrap();
            let (store, problem) = Store::load(&dir);
            assert_eq!(store, Store::default(), "{bad:?}");
            assert!(problem.is_some(), "{bad:?}");
        }
        // Not text at all.
        std::fs::write(Store::path(&dir), [0xff, 0xfe, 0xfd]).unwrap();
        assert!(Store::load(&dir).1.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unusable_records_are_dropped_and_good_ones_kept() {
        let dir = test_dir("records");
        std::fs::create_dir_all(&dir).unwrap();
        let text = serde_json::json!({
            "account_id": "not base64 at all!",
            "future_setting": true,
            "consoles": {
                "GOOD": pairing(),
                "SHORTKEY": { "ps5": true, "regist_key": "abcd", "rp_key": "00112233445566778899aabbccddeeff" },
                "NOTHEX": { "ps5": true, "regist_key": "zz112233445566778899aabbccddeeff", "rp_key": "00112233445566778899aabbccddeeff" },
                "bad id!": pairing(),
            }
        });
        std::fs::write(Store::path(&dir), text.to_string()).unwrap();
        let (store, problem) = Store::load(&dir);
        assert_eq!(problem, None);
        assert_eq!(store.consoles.keys().collect::<Vec<_>>(), ["GOOD"]);
        assert_eq!(store.account(), None, "an unreadable account ID counts as not set");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hex_and_ids_are_checked() {
        assert_eq!(from_hex::<2>("0aFF"), Some([0x0a, 0xff]));
        assert_eq!(from_hex::<2>("0aF"), None);
        assert_eq!(from_hex::<2>("0aFFF"), None);
        assert_eq!(from_hex::<2>("+1+2"), None);
        assert_eq!(from_hex::<2>("ääää"), None, "no slicing inside a character");
        assert_eq!(to_hex(&[0, 0x0a, 0xff]), "000aff");
        assert!(valid_console_id("AABBCCDDEEFF"));
        assert!(valid_console_id("a-b_c"));
        for bad in ["", "a b", "a/b", "..", "a\nb", "ä", &"a".repeat(65)] {
            assert!(!valid_console_id(bad), "{bad:?}");
        }
    }
}
