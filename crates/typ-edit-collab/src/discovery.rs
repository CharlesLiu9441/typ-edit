//! mDNS discovery over `mdns-sd` (plan `§5.8`).
//!
//! The service type is `_typ-edit._tcp.local.`. TXT records carry only
//! `ver`/`user`/`project`/`pw` — never the salt or password, so the mDNS
//! information surface stays minimal. [`Advertiser`] registers the server's
//! presence (unregistering on drop); [`Browser`] browses and surfaces resolved
//! services as [`DiscoveryEvent`]s on a channel, stopping on drop. Pure helpers
//! build and parse the TXT map so the encoding is unit-testable without a
//! network.

use crate::error::DiscoveryError;
use crossbeam_channel::{Receiver, Sender, unbounded};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

/// The mDNS service type advertised and browsed.
pub const SERVICE_TYPE: &str = "_typ-edit._tcp.local.";

/// TXT key for the protocol version.
const TXT_VER: &str = "ver";
/// TXT key for the server user display name.
const TXT_USER: &str = "user";
/// TXT key for the project name.
const TXT_PROJECT: &str = "project";
/// TXT key for the password-required flag (`0`/`1`).
const TXT_PW: &str = "pw";

/// A service resolved by [`Browser`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredService {
    /// The full service instance name.
    pub instance: String,
    /// The advertised project name.
    pub project: String,
    /// The advertised server user name.
    pub user: String,
    /// Whether the server requires a password.
    pub needs_password: bool,
    /// The advertised TCP port.
    pub port: u16,
    /// Candidate addresses to connect to, in advertisement order.
    pub addresses: Vec<SocketAddr>,
}

/// An event delivered by [`Browser`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// A service was resolved.
    Discovered(DiscoveredService),
    /// A service's full name was removed (TTL expiry or explicit good-bye).
    Removed(String),
}

/// The parsed TXT record of a discovered service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceTxt {
    /// The protocol version string.
    pub version: String,
    /// The server user name.
    pub user: String,
    /// The project name.
    pub project: String,
    /// Whether a password is required.
    pub needs_password: bool,
}

/// Build the TXT map advertised for a server.
pub fn build_txt(project: &str, user: &str, needs_password: bool) -> HashMap<String, String> {
    HashMap::from([
        (TXT_VER.to_owned(), "1".to_owned()),
        (TXT_USER.to_owned(), user.to_owned()),
        (TXT_PROJECT.to_owned(), project.to_owned()),
        (
            TXT_PW.to_owned(),
            if needs_password { "1" } else { "0" }.to_owned(),
        ),
    ])
}

/// Parse a TXT map into a [`ServiceTxt`], rejecting records that are not a
/// supported version or lack the required keys.
pub fn parse_txt(entries: &HashMap<String, String>) -> Option<ServiceTxt> {
    let version = entries.get(TXT_VER)?;
    if version != "1" {
        return None;
    }
    let user = entries.get(TXT_USER)?.clone();
    let project = entries.get(TXT_PROJECT)?.clone();
    let needs_password = entries.get(TXT_PW).map(String::as_str) == Some("1");
    Some(ServiceTxt {
        version: version.clone(),
        user,
        project,
        needs_password,
    })
}

/// A registered mDNS advertisement; unregisters and shuts its daemon down on drop.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// Register `project`/`server_user` on `port` under a random instance name.
    pub fn start(
        project: &str,
        server_user: &str,
        needs_password: bool,
        port: u16,
    ) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let instance = format!("typ-edit-{project}-{}", random_suffix());
        let host = format!("{instance}.local.");
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host,
            "",
            port,
            build_txt(project, server_user, needs_password),
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        Ok(Self { daemon, fullname })
    }

    /// The advertised instance name.
    pub fn instance(&self) -> &str {
        &self.fullname
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// A running browse; stops and shuts its daemon down on drop.
pub struct Browser {
    daemon: ServiceDaemon,
    events: Receiver<DiscoveryEvent>,
}

impl Browser {
    /// Start browsing for `_typ-edit._tcp.local.` services.
    pub fn start() -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let mdns_events = daemon.browse(SERVICE_TYPE)?;
        let (tx, events) = unbounded();
        std::thread::spawn(move || forward_events(mdns_events, tx));
        Ok(Self { daemon, events })
    }

    /// Receive the next event, waiting up to `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<DiscoveryEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Receive the next event if one is already queued.
    pub fn try_recv(&self) -> Option<DiscoveryEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

fn forward_events(mdns_events: mdns_sd::Receiver<ServiceEvent>, tx: Sender<DiscoveryEvent>) {
    while let Ok(event) = mdns_events.recv() {
        let output = match event {
            ServiceEvent::ServiceResolved(info) => resolve(&info),
            ServiceEvent::ServiceRemoved(_, fullname) => Some(DiscoveryEvent::Removed(fullname)),
            _ => None,
        };
        if let Some(event) = output
            && tx.send(event).is_err()
        {
            break;
        }
    }
}

fn resolve(info: &ResolvedService) -> Option<DiscoveryEvent> {
    let properties = info.get_properties();
    let txt = HashMap::from([
        (
            TXT_VER.to_owned(),
            properties
                .get_property_val_str(TXT_VER)
                .unwrap_or("")
                .to_owned(),
        ),
        (
            TXT_USER.to_owned(),
            properties
                .get_property_val_str(TXT_USER)
                .unwrap_or("")
                .to_owned(),
        ),
        (
            TXT_PROJECT.to_owned(),
            properties
                .get_property_val_str(TXT_PROJECT)
                .unwrap_or("")
                .to_owned(),
        ),
        (
            TXT_PW.to_owned(),
            properties
                .get_property_val_str(TXT_PW)
                .unwrap_or("0")
                .to_owned(),
        ),
    ]);
    let parsed = parse_txt(&txt)?;
    let port = info.get_port();
    let addresses: Vec<SocketAddr> = info
        .get_addresses()
        .iter()
        .map(|ip| SocketAddr::new(ip.to_ip_addr(), port))
        .collect();
    if addresses.is_empty() {
        return None;
    }
    Some(DiscoveryEvent::Discovered(DiscoveredService {
        instance: info.get_fullname().to_owned(),
        project: parsed.project,
        user: parsed.user,
        needs_password: parsed.needs_password,
        port,
        addresses,
    }))
}

fn random_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return format!("{:04x}", std::process::id() & 0xffff);
    }
    bytes
        .iter()
        .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn txt_round_trips() {
        let txt = build_txt("myproj", "ann", true);
        let parsed = parse_txt(&txt).expect("parse");
        assert_eq!(parsed.version, "1");
        assert_eq!(parsed.project, "myproj");
        assert_eq!(parsed.user, "ann");
        assert!(parsed.needs_password);
    }

    #[test]
    fn txt_password_flag_off() {
        let txt = build_txt("p", "bob", false);
        assert_eq!(txt.get(TXT_PW).map(String::as_str), Some("0"));
        assert!(!parse_txt(&txt).expect("parse").needs_password);
    }

    #[test]
    fn txt_never_carries_secrets() {
        let txt = build_txt("p", "u", true);
        assert_eq!(txt.len(), 4);
        assert!(!txt.contains_key("salt"));
        assert!(!txt.contains_key("password"));
    }

    #[test]
    fn parse_rejects_wrong_version_and_missing_keys() {
        let mut wrong = build_txt("p", "u", false);
        wrong.insert(TXT_VER.to_owned(), "2".to_owned());
        assert!(parse_txt(&wrong).is_none());

        let mut missing = build_txt("p", "u", false);
        missing.remove(TXT_USER);
        assert!(parse_txt(&missing).is_none());
    }

    #[test]
    fn random_suffix_is_four_alphanumerics() {
        let suffix = random_suffix();
        assert_eq!(suffix.len(), 4);
        assert!(suffix.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
