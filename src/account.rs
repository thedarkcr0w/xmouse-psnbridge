//! The PSN account ID: eight bytes that identify the PlayStation Network account signed in on
//! the console. The console only accepts a registration (pairing) that names it.
//!
//! The usual tools (chiaki's `psn-account-id.py`, chiaki-ng's login window) print it as
//! base64, e.g. `AbCdEfGhIjk=`. Those eight bytes are the account's number in little-endian
//! order, so three spellings are accepted:
//! - base64 of the eight bytes (12 characters with the `=`, or 11 without);
//! - hex of the eight bytes in the same order (16 hex digits, optionally `0x` in front);
//! - `dec:` followed by the account number in decimal.

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;

pub const ACCOUNT_ID_SIZE: usize = 8;

pub type AccountId = [u8; ACCOUNT_ID_SIZE];

/// Read an account ID in any of the accepted spellings. The error is a sentence for the user.
pub fn parse_account_id(text: &str) -> Result<AccountId, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The PSN account ID is empty.".into());
    }
    if text.len() > 64 {
        return Err("That is too long to be a PSN account ID.".into());
    }
    if let Some(number) = text.strip_prefix("dec:") {
        let n: u64 = number
            .trim()
            .parse()
            .map_err(|_| "After \"dec:\" the PSN account ID must be a whole number.".to_string())?;
        return Ok(n.to_le_bytes());
    }
    let hex = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    // 16 hex digits can never be base64 of eight bytes (that is 11 or 12 characters), so the
    // two spellings can't be confused.
    if hex.len() == ACCOUNT_ID_SIZE * 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut id = [0u8; ACCOUNT_ID_SIZE];
        for (i, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| "The PSN account ID is not valid hex.".to_string())?;
        }
        return Ok(id);
    }
    let decoded = STANDARD
        .decode(text)
        .or_else(|_| STANDARD_NO_PAD.decode(text))
        .map_err(|_| {
            "The PSN account ID must be base64 (like AbCdEfGhIjk=) or 16 hex digits.".to_string()
        })?;
    <AccountId>::try_from(decoded.as_slice()).map_err(|_| {
        format!(
            "The PSN account ID must be 8 bytes, but this decodes to {}.",
            decoded.len()
        )
    })
}

/// The spelling used in the settings file and in messages.
pub fn to_base64(id: &AccountId) -> String {
    STANDARD.encode(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BYTES: AccountId = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];

    #[test]
    fn base64_with_and_without_padding() {
        assert_eq!(to_base64(&BYTES), "ASNFZ4mrze8=");
        assert_eq!(parse_account_id("ASNFZ4mrze8=").unwrap(), BYTES);
        assert_eq!(parse_account_id("ASNFZ4mrze8").unwrap(), BYTES);
        assert_eq!(parse_account_id("  ASNFZ4mrze8=\r\n").unwrap(), BYTES, "pasted with whitespace");
    }

    #[test]
    fn hex_is_the_same_eight_bytes_in_the_same_order() {
        assert_eq!(parse_account_id("0123456789abcdef").unwrap(), BYTES);
        assert_eq!(parse_account_id("0123456789ABCDEF").unwrap(), BYTES);
        assert_eq!(parse_account_id("0x0123456789abcdef").unwrap(), BYTES);
        // All digits and 16 long: still hex, never mistaken for a decimal number.
        assert_eq!(
            parse_account_id("1234567812345678").unwrap(),
            [0x12, 0x34, 0x56, 0x78, 0x12, 0x34, 0x56, 0x78]
        );
    }

    #[test]
    fn decimal_is_little_endian_like_the_upstream_tools() {
        // psn-account-id.py: base64(int(user_id).to_bytes(8, "little"))
        let n: u64 = 1234567890123456789;
        let id = parse_account_id(&format!("dec:{n}")).unwrap();
        assert_eq!(id, n.to_le_bytes());
        assert_eq!(parse_account_id(&to_base64(&id)).unwrap(), id);
    }

    #[test]
    fn every_id_round_trips_through_base64() {
        for id in [[0u8; 8], [0xff; 8], BYTES, [0xfb, 0xff, 0xfe, 0x3e, 0x3f, 0, 1, 2]] {
            assert_eq!(parse_account_id(&to_base64(&id)).unwrap(), id);
        }
    }

    #[test]
    fn wrong_input_is_an_error_with_a_reason_and_never_a_panic() {
        for bad in [
            "",
            "   ",
            "hello",
            "ASNFZ4mrze8=ASNFZ4mrze8=",
            "ASNF",                 // 3 bytes
            "0123456789abcde",      // 15 hex digits
            "0123456789abcdef01",   // 9 bytes of hex
            "0x",
            "dec:",
            "dec:-5",
            "dec:99999999999999999999999",
            "dec:12ab",
            "ää=ö",
            "\0\0\0",
            &"A".repeat(5000),
        ] {
            let e = parse_account_id(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad:?}");
        }
        assert!(parse_account_id("ASNF").unwrap_err().contains("8 bytes"));
    }
}
