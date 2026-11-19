//! Finding, pairing and waking consoles: the glue between XMouse's questions and chiaki's
//! discovery and registration code.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chiaki_core::discovery::{
    self, Discovery, DiscoveryCmd, DiscoveryHost, DiscoveryHostState, DiscoveryPacket,
    DiscoveryThread, DISCOVERY_PORT_PS4, DISCOVERY_PORT_PS5, DISCOVERY_PROTOCOL_VERSION_PS4,
    DISCOVERY_PROTOCOL_VERSION_PS5,
};
use chiaki_core::regist::{Regist, RegistEvent, RegistInfo};
use chiaki_core::Target;

use crate::account;
use crate::proto::{ConsoleEntry, ConsoleState};
use crate::store::{self, Pairing, Store};

/// How long a plain "which consoles are there" search listens. Consoles answer within
/// milliseconds; the rest is margin for Wi-Fi.
const LIST_SEARCH: Duration = Duration::from_millis(2000);
/// How often the search question is repeated while listening (UDP packets do get lost).
const SEARCH_REPEAT: Duration = Duration::from_millis(500);
/// Registration is two short exchanges; chiaki gives each one three seconds.
const REGIST_TIMEOUT: Duration = Duration::from_secs(20);
/// Never keep more consoles than XMouse would take anyway.
const MAX_FOUND: usize = 32;

/// A console that answered the search.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: String,
    pub name: String,
    /// "PS5" or "PS4", as the console calls itself.
    pub model: String,
    pub ps5: bool,
    pub state: DiscoveryHostState,
    /// Its IP address.
    pub addr: String,
    /// Which generation of the protocol its system software speaks.
    pub target: Target,
}

/// Turn chiaki's discovery answer into ours. `None` for answers without a usable ID: the ID
/// becomes a key in the settings file and goes to XMouse, so it must be plain.
pub fn found_from_host(host: &DiscoveryHost) -> Option<Found> {
    let id = host.host_id.as_deref()?.trim();
    if !store::valid_console_id(id) {
        return None;
    }
    let ps5 = host.is_ps5();
    let model = match host.host_type.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => t.chars().filter(|c| !c.is_control()).take(40).collect(),
        _ => if ps5 { "PS5" } else { "PS4" }.to_string(),
    };
    let name = match host.host_name.as_deref().map(str::trim) {
        Some(n) if !n.is_empty() => n.chars().filter(|c| !c.is_control()).take(80).collect(),
        _ => model.clone(),
    };
    Some(Found {
        id: id.to_string(),
        name,
        model,
        ps5,
        state: host.state,
        addr: host.host_addr.clone(),
        target: host.system_version_target(),
    })
}

/// Ask the local network which consoles are there and listen for `wait`.
///
/// The question goes out as a broadcast (reaches everything on the network this PC's main
/// connection is on) and directly to each address in `also_ask` (where paired consoles were
/// last seen, which helps on PCs with several network connections). `enough` can end the
/// search early, e.g. as soon as the one wanted console has answered.
pub fn search(
    wait: Duration,
    also_ask: &[String],
    enough: &dyn Fn(&[Found]) -> bool,
) -> Result<Vec<Found>, String> {
    chiaki_core::error::lib_init().map_err(|e| format!("The network is not available ({e})."))?;
    let discovery = Arc::new(
        Discovery::new(false).map_err(|e| format!("Could not open a socket to search for consoles ({e})."))?,
    );
    let answers: Arc<Mutex<Vec<DiscoveryHost>>> = Arc::default();
    let sink = answers.clone();
    let listener = DiscoveryThread::start(
        discovery.clone(),
        Arc::new(move |host| {
            let mut list = sink.lock().unwrap_or_else(PoisonError::into_inner);
            if list.len() < MAX_FOUND * 4 {
                list.push(host);
            }
        }),
    )
    .map_err(|e| format!("Could not start searching for consoles ({e})."))?;

    let mut targets: Vec<IpAddr> = vec![IpAddr::V4(Ipv4Addr::BROADCAST)];
    for host in also_ask {
        // Only plain IPv4 addresses are stored; anything else is skipped rather than resolved.
        if let Ok(ip @ IpAddr::V4(_)) = host.parse::<IpAddr>() {
            if !targets.contains(&ip) {
                targets.push(ip);
            }
        }
    }

    let collect = || -> Vec<Found> {
        let list = answers.lock().unwrap_or_else(PoisonError::into_inner);
        // A console answers every repeat of the question: keep its latest answer only.
        let mut by_id: BTreeMap<String, Found> = BTreeMap::new();
        for found in list.iter().filter_map(found_from_host) {
            by_id.insert(found.id.clone(), found);
        }
        by_id.into_values().take(MAX_FOUND).collect()
    };

    let deadline = Instant::now() + wait;
    let mut sent_any = false;
    loop {
        for ip in &targets {
            // PS4 and PS5 listen on different ports and expect different version strings.
            for (port, version) in [
                (DISCOVERY_PORT_PS4, DISCOVERY_PROTOCOL_VERSION_PS4),
                (DISCOVERY_PORT_PS5, DISCOVERY_PROTOCOL_VERSION_PS5),
            ] {
                let packet = DiscoveryPacket {
                    cmd: DiscoveryCmd::Srch,
                    protocol_version: Some(version.to_string()),
                    user_credential: 0,
                };
                sent_any |= discovery.send(&packet, SocketAddr::new(*ip, port)).is_ok();
            }
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(SEARCH_REPEAT.min(deadline - now));
        if enough(&collect()) {
            break;
        }
    }
    let _ = listener.stop();
    let found = collect();
    if found.is_empty() && !sent_any {
        return Err("Could not send the search for consoles. Is this PC connected to a network?".into());
    }
    Ok(found)
}

/// The addresses where paired consoles were last seen.
pub fn known_hosts(store: &Store) -> Vec<String> {
    store
        .consoles
        .values()
        .map(|p| p.last_host.clone())
        .filter(|h| !h.is_empty())
        .collect()
}

fn console_state(state: DiscoveryHostState) -> ConsoleState {
    match state {
        DiscoveryHostState::Ready => ConsoleState::On,
        DiscoveryHostState::Standby => ConsoleState::Standby,
        DiscoveryHostState::Unknown => ConsoleState::Unknown,
    }
}

/// Put what the search found together with what is paired. Paired consoles that didn't
/// answer are still listed (state unknown: switched off, or on another network), so they
/// don't vanish from XMouse's list.
pub fn merge_list(found: &[Found], store: &Store) -> Vec<ConsoleEntry> {
    let mut list: Vec<ConsoleEntry> = found
        .iter()
        .map(|f| ConsoleEntry {
            id: f.id.clone(),
            name: f.name.clone(),
            model: f.model.clone(),
            state: console_state(f.state),
            needs_pairing: !store.consoles.contains_key(&f.id),
        })
        .collect();
    for (id, pairing) in &store.consoles {
        if found.iter().any(|f| &f.id == id) {
            continue;
        }
        let model = if pairing.ps5 { "PS5" } else { "PS4" };
        list.push(ConsoleEntry {
            id: id.clone(),
            name: if pairing.name.is_empty() { model.to_string() } else { pairing.name.clone() },
            model: model.to_string(),
            state: ConsoleState::Unknown,
            needs_pairing: false,
        });
    }
    list.truncate(MAX_FOUND);
    list
}

/// `list_consoles`: search, remember where paired consoles are now, and build the list.
pub fn list(data_dir: &Path) -> (Vec<ConsoleEntry>, Option<String>) {
    let (mut store, _) = Store::load(data_dir);
    match search(LIST_SEARCH, &known_hosts(&store), &|_| false) {
        Ok(found) => {
            if remember_hosts(&mut store, &found) {
                let _ = store.save(data_dir); // only a convenience: failing to save is not an error here
            }
            (merge_list(&found, &store), None)
        }
        // Still show what is paired; the reason goes to the log.
        Err(e) => (merge_list(&[], &store), Some(e)),
    }
}

/// Note the current address (and name) of paired consoles. Returns whether anything changed.
pub fn remember_hosts(store: &mut Store, found: &[Found]) -> bool {
    let mut changed = false;
    for f in found {
        if let Some(p) = store.consoles.get_mut(&f.id) {
            if p.last_host != f.addr || p.name != f.name {
                p.last_host = f.addr.clone();
                p.name = f.name.clone();
                changed = true;
            }
        }
    }
    changed
}

/// The PIN the console shows under Settings, Remote Play, Link Device: eight digits. People
/// type it the way it is displayed, in two groups, so spaces and dashes are allowed.
pub fn parse_pin(text: &str) -> Result<u32, String> {
    let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-' | '\t')).collect();
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("The PIN is the 8 digits the console shows under Settings > Remote Play > Link Device.".into());
    }
    digits.parse().map_err(|_| "The PIN is not a number.".to_string())
}

/// Which account ID to pair with: the one in the request if there is one (it is then also
/// remembered), otherwise the stored one. A clear sentence if neither exists.
pub fn account_for_pairing(
    store: &mut Store,
    extra: &BTreeMap<String, String>,
) -> Result<(account::AccountId, bool), String> {
    match extra.get("account_id").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(text) => {
            let id = account::parse_account_id(text)?;
            let changed = store.account() != Some(id);
            store.set_account(&id);
            Ok((id, changed))
        }
        None => store.account().map(|id| (id, false)).ok_or_else(|| {
            "No PSN account ID is set. Pairing needs the ID of the PlayStation account that is \
             signed in on the console. Set it once with: xmouse-psnbridge --set-account <id> \
             (see the PSN Bridge README for how to find it)."
                .to_string()
        }),
    }
}

/// `pair`: register this PC with the console, using the PIN on its screen.
pub fn pair(
    data_dir: &Path,
    console_id: &str,
    pin: &str,
    extra: &BTreeMap<String, String>,
) -> Result<(), String> {
    if !store::valid_console_id(console_id) {
        return Err("Unknown console.".into());
    }
    let (mut store, _) = Store::load(data_dir);
    // Checked before anything touches the network, so a missing ID is reported at once.
    let (account_id, account_changed) = account_for_pairing(&mut store, extra)?;
    if account_changed {
        store.save(data_dir)?;
    }
    let pin = parse_pin(pin)?;

    let found = search(Duration::from_secs(4), &known_hosts(&store), &|list| {
        list.iter().any(|f| f.id == console_id)
    })?;
    let console = found.into_iter().find(|f| f.id == console_id).ok_or_else(|| {
        "The console did not answer. It must be switched on and on the same network as this PC.".to_string()
    })?;
    if console.state == DiscoveryHostState::Standby {
        return Err("The console is in rest mode. Switch it on and open Settings > Remote Play > Link Device.".into());
    }
    // A console whose system software this code can't place is treated as the newest of its
    // kind, which is what a current console is.
    let target = match console.target {
        t if !t.is_unknown() => t,
        _ if console.ps5 => Target::Ps5_1,
        _ => Target::Ps4_10,
    };

    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let regist = Regist::start(
        RegistInfo {
            target,
            host: console.addr.clone(),
            broadcast: false,
            psn_online_id: None,
            psn_account_id: account_id,
            pin,
            console_pin: 0,
        },
        Arc::new(move |event| {
            let _ = tx.lock().unwrap_or_else(PoisonError::into_inner).send(event);
        }),
    )
    .map_err(|e| format!("Could not start pairing ({e})."))?;
    let event = rx.recv_timeout(REGIST_TIMEOUT);
    if matches!(event, Err(RecvTimeoutError::Timeout)) {
        regist.stop();
    }
    regist.fini(); // waits for chiaki's registration thread

    let host = match event {
        Ok(RegistEvent::FinishedSuccess(host)) => host,
        Ok(RegistEvent::FinishedCanceled) | Err(_) => {
            return Err("Pairing took too long and was stopped. Try again with a fresh PIN.".into())
        }
        Ok(RegistEvent::FinishedFailed) => {
            // chiaki reports only "failed" here; its log (in XMouse's log) has the detail.
            return Err("The console refused the pairing. Check that the PIN is the one on the console's \
                        Link Device screen right now, and that the PSN account ID belongs to the account \
                        signed in on the console."
                .into());
        }
    };

    let name = if host.server_nickname.trim().is_empty() { console.name.clone() } else { host.server_nickname.clone() };
    store.consoles.insert(
        console_id.to_string(),
        Pairing {
            name: name.chars().filter(|c| !c.is_control()).take(80).collect(),
            ps5: target.is_ps5(),
            target: target.value(),
            regist_key: store::to_hex(&host.rp_regist_key),
            rp_key: store::to_hex(&host.rp_key),
            mac: store::to_hex(&host.server_mac),
            last_host: console.addr,
        },
    );
    store.save(data_dir).map_err(|e| format!("Paired, but the pairing could not be saved: {e}"))
}

/// The number the console expects in a wake-up packet. The registration key is text (hex
/// digits, padded with zero bytes); the console wants that text read as one hexadecimal
/// number. This is how chiaki derives it.
pub fn wake_credential(regist_key: &[u8; store::KEY_SIZE]) -> Option<u64> {
    let end = regist_key.iter().position(|&b| b == 0).unwrap_or(regist_key.len());
    let text = std::str::from_utf8(&regist_key[..end]).ok()?;
    if text.is_empty() || text.len() > 16 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(text, 16).ok()
}

/// Send the wake-up packet to `host` (an IP address, or the broadcast address).
pub fn send_wake(pairing: &Pairing, host: &str) -> Result<(), String> {
    let key = pairing.regist_key_bytes().ok_or("The stored pairing is damaged. Pair the console again.")?;
    let credential = wake_credential(&key).ok_or("The stored pairing can't be used to wake the console. Pair it again.")?;
    discovery::wakeup(None, host, credential, pairing.ps5)
        .map_err(|e| format!("The wake-up message could not be sent ({e})."))
}

/// `wake`: bring a paired console out of rest mode. Succeeds when the wake-up message is on
/// its way (or the console is already on); it does not wait for the console to finish
/// starting.
pub fn wake(data_dir: &Path, console_id: &str) -> Result<(), String> {
    if !store::valid_console_id(console_id) {
        return Err("Unknown console.".into());
    }
    let (mut store, _) = Store::load(data_dir);
    let Some(pairing) = store.consoles.get(console_id).cloned() else {
        return Err("This console is not paired yet, so it can't be woken from here.".into());
    };
    chiaki_core::error::lib_init().map_err(|e| format!("The network is not available ({e})."))?;

    // A console in rest mode still answers the search, which tells us its current address.
    let found = search(Duration::from_millis(1500), &known_hosts(&store), &|list| {
        list.iter().any(|f| f.id == console_id)
    })
    .unwrap_or_default();
    let console = found.iter().find(|f| f.id == console_id);
    if remember_hosts(&mut store, &found) {
        let _ = store.save(data_dir);
    }
    if let Some(c) = console {
        if c.state == DiscoveryHostState::Ready {
            return Ok(()); // already on
        }
    }
    let mut hosts: Vec<String> = Vec::new();
    if let Some(c) = console {
        hosts.push(c.addr.clone());
    }
    if !pairing.last_host.is_empty() && !hosts.contains(&pairing.last_host) {
        hosts.push(pairing.last_host.clone());
    }
    hosts.push(Ipv4Addr::BROADCAST.to_string());
    let mut last_error = None;
    let mut sent = false;
    for host in &hosts {
        match send_wake(&pairing, host) {
            Ok(()) => sent = true,
            Err(e) => last_error = Some(e),
        }
    }
    if sent {
        Ok(())
    } else {
        Err(last_error.unwrap_or_else(|| "The wake-up message could not be sent.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, code_ready: bool, ps5: bool) -> DiscoveryHost {
        DiscoveryHost {
            state: if code_ready { DiscoveryHostState::Ready } else { DiscoveryHostState::Standby },
            host_request_port: 997,
            host_addr: "192.168.1.50".into(),
            system_version: Some(if ps5 { "09000002" } else { "11000000" }.into()),
            device_discovery_protocol_version: Some(
                if ps5 { DISCOVERY_PROTOCOL_VERSION_PS5 } else { DISCOVERY_PROTOCOL_VERSION_PS4 }.into(),
            ),
            host_name: Some("Living room".into()),
            host_type: Some(if ps5 { "PS5" } else { "PS4" }.into()),
            host_id: Some(id.into()),
            running_app_titleid: None,
            running_app_name: None,
        }
    }

    fn pairing(ps5: bool) -> Pairing {
        Pairing {
            name: "Old name".into(),
            ps5,
            target: 0,
            regist_key: store::to_hex(b"4f6a2b1c\0\0\0\0\0\0\0\0"),
            rp_key: "00112233445566778899aabbccddeeff".into(),
            mac: String::new(),
            last_host: "192.168.1.7".into(),
        }
    }

    #[test]
    fn discovery_answers_become_consoles() {
        let f = found_from_host(&host("AABBCCDDEEFF", true, true)).unwrap();
        assert_eq!((f.id.as_str(), f.name.as_str(), f.model.as_str()), ("AABBCCDDEEFF", "Living room", "PS5"));
        assert!(f.ps5);
        assert_eq!(f.target, Target::Ps5_1);
        let f = found_from_host(&host("001122334455", false, false)).unwrap();
        assert!(!f.ps5);
        assert_eq!(f.state, DiscoveryHostState::Standby);
        assert_eq!(f.target, Target::Ps4_10);
    }

    #[test]
    fn answers_from_the_network_are_not_trusted() {
        // No ID, or an ID that could not safely be a settings key: skipped.
        let mut h = host("x", true, true);
        h.host_id = None;
        assert_eq!(found_from_host(&h), None);
        for bad in ["", "../../x", "a b", "id\nwith\nlines", &"A".repeat(300)] {
            assert_eq!(found_from_host(&host(bad, true, true)), None, "{bad:?}");
        }
        // Missing or odd names fall back; control characters are removed; length is bounded.
        let mut h = host("AB12", true, false);
        h.host_name = None;
        h.host_type = None;
        h.system_version = Some("not a number".into());
        let f = found_from_host(&h).unwrap();
        assert_eq!((f.name.as_str(), f.model.as_str()), ("PS4", "PS4"));
        h.host_name = Some(format!("evil\u{7}\n{}", "n".repeat(500)));
        let f = found_from_host(&h).unwrap();
        assert_eq!(f.name.chars().count(), 80);
        assert!(!f.name.chars().any(char::is_control));
    }

    #[test]
    fn the_list_joins_what_answered_with_what_is_paired() {
        let mut store = Store::default();
        store.consoles.insert("PAIREDON".into(), pairing(true));
        store.consoles.insert("PAIREDOFF".into(), pairing(false));
        let found = vec![
            found_from_host(&host("PAIREDON", true, true)).unwrap(),
            found_from_host(&host("NEWONE", false, false)).unwrap(),
        ];
        let list = merge_list(&found, &store);
        assert_eq!(list.len(), 3);
        assert_eq!((list[0].id.as_str(), list[0].state, list[0].needs_pairing), ("PAIREDON", ConsoleState::On, false));
        assert_eq!((list[1].id.as_str(), list[1].state, list[1].needs_pairing), ("NEWONE", ConsoleState::Standby, true));
        // Paired but silent: still listed, with its stored name.
        assert_eq!(
            list[2],
            ConsoleEntry {
                id: "PAIREDOFF".into(),
                name: "Old name".into(),
                model: "PS4".into(),
                state: ConsoleState::Unknown,
                needs_pairing: false
            }
        );
        // And the paired console's new address and name are noted.
        assert!(remember_hosts(&mut store, &found));
        assert_eq!(store.consoles["PAIREDON"].last_host, "192.168.1.50");
        assert_eq!(store.consoles["PAIREDON"].name, "Living room");
        assert!(!remember_hosts(&mut store, &found), "nothing changes the second time");
        assert_eq!(known_hosts(&store), ["192.168.1.7", "192.168.1.50"]);
    }

    #[test]
    fn pins_are_eight_digits_however_they_are_typed() {
        assert_eq!(parse_pin("12345678"), Ok(12345678));
        assert_eq!(parse_pin("1234 5678"), Ok(12345678));
        assert_eq!(parse_pin(" 1234-5678 "), Ok(12345678));
        assert_eq!(parse_pin("00000042"), Ok(42));
        for bad in ["", "1234567", "123456789", "1234567a", "+1234567", "١٢٣٤٥٦٧٨", "12345678\n9"] {
            assert!(parse_pin(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_account_id_comes_from_the_request_or_from_the_store() {
        let none = BTreeMap::new();
        let mut store = Store::default();
        let e = account_for_pairing(&mut store, &none).unwrap_err();
        assert!(e.contains("--set-account"), "{e}");
        // An empty value counts as not given.
        let empty = BTreeMap::from([("account_id".to_string(), "  ".to_string())]);
        assert!(account_for_pairing(&mut store, &empty).is_err());
        // Given in the request: used and remembered.
        let given = BTreeMap::from([("account_id".to_string(), "0102030405060708".to_string())]);
        assert_eq!(account_for_pairing(&mut store, &given), Ok(([1, 2, 3, 4, 5, 6, 7, 8], true)));
        assert_eq!(account_for_pairing(&mut store, &given), Ok(([1, 2, 3, 4, 5, 6, 7, 8], false)));
        // Now the stored one serves requests that bring none.
        assert_eq!(account_for_pairing(&mut store, &none), Ok(([1, 2, 3, 4, 5, 6, 7, 8], false)));
        // A broken one in the request is an error, not silently replaced by the stored one.
        let broken = BTreeMap::from([("account_id".to_string(), "nonsense".to_string())]);
        assert!(account_for_pairing(&mut store, &broken).is_err());
    }

    #[test]
    fn pair_and_wake_refuse_bad_requests_before_touching_the_network() {
        let dir = store::test_dir("pair-refuse");
        let none = BTreeMap::new();
        assert_eq!(pair(&dir, "bad id!", "12345678", &none).unwrap_err(), "Unknown console.");
        assert!(pair(&dir, "AABB", "12345678", &none).unwrap_err().contains("account ID"));
        let with_account = BTreeMap::from([("account_id".to_string(), "ASNFZ4mrze8=".to_string())]);
        assert!(pair(&dir, "AABB", "12", &with_account).unwrap_err().contains("PIN"));
        // The account ID from that request was remembered even though pairing went no further.
        assert_eq!(Store::load(&dir).0.account(), Some([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]));
        assert_eq!(wake(&dir, "..").unwrap_err(), "Unknown console.");
        assert!(wake(&dir, "AABB").unwrap_err().contains("not paired"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wake_credential_is_the_key_text_read_as_hex() {
        assert_eq!(wake_credential(b"4f6a2b1c\0\0\0\0\0\0\0\0"), Some(0x4f6a2b1c));
        assert_eq!(wake_credential(b"0000000a\0\0\0\0\0\0\0\0"), Some(10));
        assert_eq!(wake_credential(b"ffffffffffffffff"), Some(u64::MAX));
        assert_eq!(wake_credential(&[0; 16]), None);
        assert_eq!(wake_credential(b"not hex!\0\0\0\0\0\0\0\0"), None);
        assert_eq!(wake_credential(b"+f6a2b1c\0\0\0\0\0\0\0\0"), None);
        assert_eq!(wake_credential(&[0xff; 16]), None);
    }
}
