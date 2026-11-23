//! Cast to this PC from a browser (the Python version's cast.py): a laptop opens Swiftlane's /cast
//! page, shares its screen, and the picture shows in a window here.
//!
//! The picture goes straight from the laptop's browser to Swiftlane's window over the home network
//! (WebRTC, always encrypted). The phone server only passes the two short connection descriptions
//! ("offer" and "answer") back and forth. Safety, the same as before:
//! - it only works while "Cast from a browser" is on, and the link carries a random code that's new
//!   every time you switch it on (separate from the phone pairing code), checked in constant time;
//! - every cast waits for you to click Allow on this PC;
//! - at most 3 requests wait at once, and they expire after 2 minutes;
//! - addresses in the descriptions are replaced with the ones the connection really came from, so
//!   the picture only flows between the laptop and this PC.

use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MAX_SDP: usize = 64 * 1024;
pub const WAIT: Duration = Duration::from_secs(120);
pub const MAX_WAITING: usize = 3;
const KEEP: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Waiting,
    Allowed,
    Denied,
    Answered,
    Expired,
    Ended,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Allowed => "allowed",
            Status::Denied => "denied",
            Status::Answered => "answered",
            Status::Expired => "expired",
            Status::Ended => "ended",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Offer {
    pub id: String,
    pub ip: IpAddr,
    pub name: String,
    pub sdp: String,
    pub status: Status,
    pub answer: Option<String>,
    time: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OfferError {
    /// 400: not a screen-sharing description
    NotAScreen,
    /// 429: too many requests waiting
    TooMany,
}

impl OfferError {
    pub fn message(&self) -> &'static str {
        match self {
            OfferError::NotAScreen => "That isn't a screen to show.",
            OfferError::TooMany => "Too many requests are waiting. Try again in a moment.",
        }
    }
}

#[derive(Default)]
struct Inner {
    token: Option<String>,
    offers: HashMap<String, Offer>,
}

#[derive(Default)]
pub struct CastHub {
    inner: Mutex<Inner>,
}

fn random_token(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("Windows' random number source failed");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// IPv4 inside IPv6 (::ffff:1.2.3.4) as plain IPv4.
pub fn host(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// Replace the browser's hidden addresses (random names ending in .local) with the address the
/// request really came from, and drop candidates that point anywhere else (relayed / "srflx" ones,
/// and other addresses).
pub fn pin_addresses(sdp: &str, ip: IpAddr) -> String {
    let ip = host(ip);
    let ip_text = ip.to_string();
    let mut out: Vec<String> = Vec::new();
    for line in sdp.replace("\r\n", "\n").split('\n') {
        if line.starts_with("a=candidate:") {
            let mut parts: Vec<&str> = line.split(' ').collect();
            if parts.len() < 8 || parts[7] != "host" {
                continue; // only direct addresses on the home network, nothing relayed elsewhere
            }
            let same = parts[4].parse::<IpAddr>().map(host).is_ok_and(|a| a == ip);
            if parts[4].ends_with(".local") || same {
                parts[4] = &ip_text;
            } else {
                continue;
            }
            out.push(parts.join(" "));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\r\n")
}

/// The name the laptop gave, cleaned up: letters, digits and a few signs, at most 40 characters.
pub fn clean_name(raw: &str) -> String {
    let kept: String = raw.chars().filter(|c| c.is_alphanumeric() || *c == '_' || " .'()-".contains(*c)).collect();
    let name: String = kept.trim().chars().take(40).collect();
    if name.is_empty() { "A browser".to_string() } else { name }
}

impl CastHub {
    pub fn on(&self) -> bool {
        self.inner.lock().unwrap().token.is_some()
    }

    /// The code in the link (None while off).
    pub fn token(&self) -> Option<String> {
        self.inner.lock().unwrap().token.clone()
    }

    pub fn enable(&self) {
        let mut i = self.inner.lock().unwrap();
        if i.token.is_none() {
            i.token = Some(random_token(9));
        }
    }

    pub fn disable(&self) {
        let mut i = self.inner.lock().unwrap();
        i.token = None;
        i.offers.clear();
    }

    /// Is `supplied` the link's code? (constant time; always false while off)
    pub fn check(&self, supplied: &str) -> bool {
        let token = self.inner.lock().unwrap().token.clone();
        token.is_some_and(|t| constant_time_eq::constant_time_eq(supplied.as_bytes(), t.as_bytes()))
    }

    fn prune(offers: &mut HashMap<String, Offer>) {
        let now = Instant::now();
        for o in offers.values_mut() {
            if o.status == Status::Waiting && now.duration_since(o.time) > WAIT {
                o.status = Status::Expired;
            }
        }
        offers.retain(|_, o| now.duration_since(o.time) <= KEEP);
    }

    /// A laptop asks to show its screen. Returns the new request (to ask on the PC).
    pub fn offer(&self, ip: IpAddr, sdp: Option<&str>, name: &str) -> Result<Offer, OfferError> {
        let sdp = sdp.filter(|s| s.starts_with("v=0") && s.len() <= MAX_SDP && s.contains("m=video")).ok_or(OfferError::NotAScreen)?;
        let name = clean_name(name);
        let mut i = self.inner.lock().unwrap();
        Self::prune(&mut i.offers);
        if i.offers.values().filter(|o| o.status == Status::Waiting).count() >= MAX_WAITING {
            return Err(OfferError::TooMany);
        }
        let id = random_token(12);
        let o = Offer { id: id.clone(), ip: host(ip), name, sdp: pin_addresses(sdp, ip), status: Status::Waiting, answer: None, time: Instant::now() };
        i.offers.insert(id, o.clone());
        Ok(o)
    }

    pub fn get(&self, id: &str) -> Option<Offer> {
        self.inner.lock().unwrap().offers.get(id).cloned()
    }

    /// Allow or deny a waiting request (None if it isn't waiting any more).
    pub fn decide(&self, id: &str, allow: bool) -> Option<Offer> {
        let mut i = self.inner.lock().unwrap();
        Self::prune(&mut i.offers);
        let o = i.offers.get_mut(id).filter(|o| o.status == Status::Waiting)?;
        o.status = if allow { Status::Allowed } else { Status::Denied };
        Some(o.clone())
    }

    /// The PC's window answers an allowed request.
    pub fn answer(&self, id: &str, sdp: &str) -> bool {
        let mut i = self.inner.lock().unwrap();
        match i.offers.get_mut(id) {
            Some(o) if o.status == Status::Allowed && sdp.starts_with("v=0") && sdp.len() <= MAX_SDP => {
                o.answer = Some(sdp.to_string());
                o.status = Status::Answered;
                true
            }
            _ => false,
        }
    }

    /// Ended by the PC (ip None) or by the laptop that sent it (its ip must match).
    /// True if it was running until now.
    pub fn end(&self, id: &str, ip: Option<IpAddr>) -> bool {
        let mut i = self.inner.lock().unwrap();
        let Some(o) = i.offers.get_mut(id) else { return false };
        if ip.is_some_and(|ip| o.ip != host(ip)) || o.status == Status::Ended {
            return false;
        }
        o.status = Status::Ended;
        true
    }

    /// What the laptop sees while it waits. own_ip: this PC's address on that connection.
    pub fn status(&self, id: &str, ip: IpAddr, own_ip: IpAddr) -> Value {
        let mut i = self.inner.lock().unwrap();
        Self::prune(&mut i.offers);
        match i.offers.get(id) {
            Some(o) if o.ip == host(ip) => match (&o.status, &o.answer) {
                (Status::Answered, Some(answer)) => json!({ "status": "answered", "sdp": pin_addresses(answer, own_ip) }),
                (s, _) => json!({ "status": s.as_str() }),
            },
            _ => json!({ "status": "unknown" }),
        }
    }

    #[cfg(test)]
    fn age(&self, id: &str, by: Duration) {
        if let Some(o) = self.inner.lock().unwrap().offers.get_mut(id) {
            o.time = Instant::now().checked_sub(by).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
        a=candidate:1 1 udp 2122260223 abcd-1234.local 54321 typ host generation 0\r\n\
        a=candidate:2 1 udp 2122260223 192.168.1.20 54322 typ host\r\n\
        a=candidate:3 1 udp 2122260223 10.9.9.9 54323 typ host\r\n\
        a=candidate:4 1 udp 1686052607 85.1.2.3 54324 typ srflx raddr 0.0.0.0 rport 0\r\n\
        a=candidate:5 1 tcp 1 short\r\n";

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn pins_addresses() {
        let out = pin_addresses(SDP, ip("::ffff:192.168.1.20"));
        assert!(out.contains("a=candidate:1 1 udp 2122260223 192.168.1.20 54321 typ host generation 0"));
        assert!(out.contains("a=candidate:2 1 udp 2122260223 192.168.1.20 54322 typ host"));
        assert!(!out.contains("10.9.9.9"), "a different address is dropped");
        assert!(!out.contains("srflx") && !out.contains("85.1.2.3"), "relayed/outside candidates are dropped");
        assert!(!out.contains("candidate:5"), "short candidate lines are dropped");
        assert!(!out.contains(".local"));
        assert!(out.starts_with("v=0\r\n") && out.contains("m=video"));
        assert!(!out.replace("\r\n", "").contains('\n'), "lines end with CRLF");
    }

    #[test]
    fn names() {
        assert_eq!(clean_name("  My <Mac> \"Book\"  "), "My Mac Book");
        assert_eq!(clean_name("<>\"/"), "A browser");
        assert_eq!(clean_name(&"x".repeat(80)).len(), 40);
        assert_eq!(clean_name("Zoë's (Laptop)-1"), "Zoë's (Laptop)-1");
    }

    #[test]
    fn token_and_switching() {
        let hub = CastHub::default();
        assert!(!hub.on() && !hub.check("") && !hub.check("anything"));
        hub.enable();
        let t = hub.token().unwrap();
        assert_eq!(t.len(), 12);
        assert!(hub.check(&t) && !hub.check("") && !hub.check(&t[..11]) && !hub.check(&format!("{t}x")));
        hub.enable();
        assert_eq!(hub.token().unwrap(), t, "switching on again keeps the code");
        hub.disable();
        assert!(!hub.check(&t));
        hub.enable();
        assert_ne!(hub.token().unwrap(), t, "a new code every time it's switched on");
    }

    #[test]
    fn offers_flow() {
        let hub = CastHub::default();
        hub.enable();
        let laptop = ip("192.168.1.20");
        let pc = ip("192.168.1.10");
        assert_eq!(hub.offer(laptop, Some("hello"), "x").unwrap_err(), OfferError::NotAScreen);
        assert_eq!(hub.offer(laptop, None, "x").unwrap_err(), OfferError::NotAScreen);
        assert_eq!(hub.offer(laptop, Some("v=0\r\nm=audio"), "x").unwrap_err(), OfferError::NotAScreen);
        let big = format!("v=0\r\nm=video{}", "a".repeat(MAX_SDP));
        assert_eq!(hub.offer(laptop, Some(&big), "x").unwrap_err(), OfferError::NotAScreen);

        let o = hub.offer(laptop, Some(SDP), "Mac").unwrap();
        assert_eq!(o.id.len(), 16);
        assert_eq!(hub.status(&o.id, laptop, pc)["status"], "waiting");
        assert_eq!(hub.status(&o.id, ip("192.168.1.99"), pc)["status"], "unknown", "only the sender sees it");
        assert!(!hub.answer(&o.id, "v=0\r\n"), "no answer before Allow");
        assert!(hub.decide(&o.id, true).is_some());
        assert!(hub.decide(&o.id, false).is_none(), "decided only once");
        assert!(!hub.answer(&o.id, "nope"));
        assert!(hub.answer(&o.id, "v=0\r\na=candidate:1 1 udp 1 abc.local 5 typ host\r\n"));
        let s = hub.status(&o.id, laptop, pc);
        assert_eq!(s["status"], "answered");
        assert!(s["sdp"].as_str().unwrap().contains("192.168.1.10 5 typ host"));
        assert!(!hub.end(&o.id, Some(ip("192.168.1.99"))), "another address can't end it");
        assert!(hub.end(&o.id, Some(laptop)));
        assert!(!hub.end(&o.id, None), "ends once");
        assert_eq!(hub.status(&o.id, laptop, pc)["status"], "ended");

        let d = hub.offer(laptop, Some(SDP), "Mac").unwrap();
        hub.decide(&d.id, false);
        assert_eq!(hub.status(&d.id, laptop, pc)["status"], "denied");
    }

    #[test]
    fn waiting_limit_and_expiry() {
        let hub = CastHub::default();
        hub.enable();
        let laptop = ip("10.0.0.2");
        let ids: Vec<String> = (0..MAX_WAITING).map(|_| hub.offer(laptop, Some(SDP), "x").unwrap().id).collect();
        assert_eq!(hub.offer(laptop, Some(SDP), "x").unwrap_err(), OfferError::TooMany);
        hub.age(&ids[0], WAIT + Duration::from_secs(1));
        assert_eq!(hub.status(&ids[0], laptop, laptop)["status"], "expired");
        assert!(hub.decide(&ids[0], true).is_none(), "an expired request can't be allowed");
        assert!(hub.offer(laptop, Some(SDP), "x").is_ok(), "expired ones free a place");
        hub.age(&ids[1], KEEP + Duration::from_secs(1));
        assert_eq!(hub.status(&ids[1], laptop, laptop)["status"], "unknown", "old ones are forgotten");
        hub.disable();
        assert!(hub.get(&ids[2]).is_none(), "switching off forgets every request");
    }
}
