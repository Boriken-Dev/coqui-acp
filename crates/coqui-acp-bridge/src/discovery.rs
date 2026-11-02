//! Finding agents: DNS-SD records on the link, and descriptor files for agents on this machine, which no multicast answer can
//! name for others. Everything found here is a hint from whoever could send it: [`Candidate::peer`] says what a key is claimed to
//! be, and only a proof at connect shows who holds it.
//!
//! The record is specified in the page contract, `docs/browser.md`.

use core::fmt;
use std::collections::HashMap;
use std::io::{self, Read as _};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde_json::{Value, json};
use url::{Host, Url};

use crate::Error;

/// The DNS-SD service type, provisional until `acp-agent` is registered with IANA.
pub const SERVICE_TYPE: &str = "_acp-agent._tcp.local.";
/// A path nothing was said about in a record.
pub const DEFAULT_PATH: &str = "/acp";
/// Overrides where descriptors are kept.
pub const DESCRIPTORS_ENV: &str = "COQUI_ACP_DESCRIPTORS";
/// Candidates one search reports: anyone on the link can send records, and a native message is limited to 1 MB.
pub const MAX_CANDIDATES: usize = 64;

/// The longest instance name DNS can carry, in bytes.
const MAX_LABEL: usize = 63;
const MAX_PATH: usize = 128;
const MAX_HOST: usize = 253;
const MAX_VERSIONS: usize = 8;
/// Directory entries looked at in one search, whatever else is in the directory.
const MAX_DESCRIPTOR_FILES: usize = 256;
const MAX_DESCRIPTOR_BYTES: u64 = 8 * 1024;
/// Longer than Windows takes to report a refused loopback port, about 2 s, so a dead agent is told from a slow answer.
const LIVENESS: Duration = Duration::from_secs(3);

/// Where a candidate was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// A DNS-SD record on the link, which anyone there can send.
    Network,
    /// A descriptor in this user's own directory.
    ThisMachine,
}

impl Source {
    const fn name(self) -> &'static str {
        match self {
            Self::Network => "mdns",
            Self::ThisMachine => "local",
        }
    }
}

/// An agent somebody claims is there.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Candidate {
    /// What it calls itself, with control characters removed. Trust it no further than any other text from a stranger.
    pub label: String,
    /// Where to connect: `wss://`, or `ws://` on loopback.
    pub url: String,
    /// The peer id it claims, if it claimed a well-formed one.
    pub peer: Option<String>,
    /// Where it was found.
    pub source: Source,
    /// A UDP port claimed to serve WebTransport on the same host.
    pub webtransport: Option<u16>,
    /// The hash of its short-lived certificate, for a browser to open that WebTransport. A hint like the rest: the proof at
    /// connect is what authenticates, so a false one only fails to open.
    pub webtransport_hash: Option<String>,
    /// ACP versions it claims.
    pub acp_versions: Vec<u32>,
}

impl Candidate {
    /// The form `discover` prints and the companion sends.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "url": self.url,
            "peer": self.peer,
            "source": self.source.name(),
            "webtransport": self.webtransport,
            "webtransportHash": self.webtransport_hash,
            "acpVersions": self.acp_versions,
        })
    }
}

/// What a `serve` says about itself.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Advertisement {
    /// The name people see.
    pub label: String,
    /// Whether the listener speaks TLS.
    pub tls: bool,
    /// The peer id its proof shows, when it has one.
    pub peer: Option<String>,
    /// A UDP port serving WebTransport.
    pub webtransport: Option<u16>,
    /// The hash of the certificate it serves, when a browser can pin that.
    pub webtransport_hash: Option<String>,
    /// A host name to publish, for a certificate a browser accepts. Without it, clients get an address.
    pub host: Option<String>,
}

impl Advertisement {
    /// A checked advertisement.
    ///
    /// # Errors
    /// [`Error::Discovery`] when the label cannot be carried, or the host is not a host name.
    pub fn new(label: &str, tls: bool) -> Result<Self, Error> {
        if label.is_empty() || label.len() > MAX_LABEL || label.chars().any(char::is_control) {
            return Err(Error::Discovery(format!(
                "a label is 1 to {MAX_LABEL} bytes with no control characters"
            )));
        }
        Ok(Self {
            label: label.to_owned(),
            tls,
            peer: None,
            webtransport: None,
            webtransport_hash: None,
            host: None,
        })
    }

    /// The peer id its proof shows.
    #[must_use]
    pub fn with_peer(mut self, peer: Option<String>) -> Self {
        self.peer = peer;
        self
    }

    /// The UDP port serving WebTransport.
    #[must_use]
    pub const fn with_webtransport(mut self, port: Option<u16>) -> Self {
        self.webtransport = port;
        self
    }

    /// The hash of the certificate it serves.
    #[must_use]
    pub fn with_webtransport_hash(mut self, hash: Option<String>) -> Self {
        self.webtransport_hash = hash;
        self
    }

    /// A host name to publish.
    ///
    /// # Errors
    /// [`Error::Discovery`] when it is not a host name.
    pub fn with_host(mut self, host: Option<String>) -> Result<Self, Error> {
        if let Some(name) = &host
            && !valid_host(name)
        {
            return Err(Error::Discovery("that is not a host name".to_owned()));
        }
        self.host = host;
        Ok(self)
    }

    fn txt(&self) -> HashMap<String, String> {
        let mut txt = HashMap::from([
            ("txtvers".to_owned(), "1".to_owned()),
            ("proto".to_owned(), "acp".to_owned()),
            ("path".to_owned(), DEFAULT_PATH.to_owned()),
        ]);
        if self.tls {
            txt.insert("tls".to_owned(), "1".to_owned());
        }
        if let Some(port) = self.webtransport {
            txt.insert("wt".to_owned(), port.to_string());
        }
        if let Some(hash) = &self.webtransport_hash {
            txt.insert("wth".to_owned(), hash.clone());
        }
        if let Some(peer) = &self.peer {
            txt.insert("peer".to_owned(), peer.clone());
        }
        txt
    }
}

fn valid_host(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_HOST
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
}

fn clean_label(raw: &str) -> Option<String> {
    let label: String = raw.chars().filter(|c| !c.is_control()).collect();
    let label = label.trim();
    if label.is_empty() {
        return None;
    }
    let mut end = label.len().min(MAX_LABEL);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    Some(label[..end].to_owned())
}

fn clean_peer(raw: Option<&str>) -> Option<String> {
    let text = raw?;
    text.parse::<coqui_peer_webcrypto::PeerId>()
        .ok()
        .map(|_| text.to_owned())
}

fn clean_path(raw: Option<&str>) -> Option<String> {
    let path = raw.unwrap_or(DEFAULT_PATH);
    let fine = path.starts_with('/')
        && path.len() <= MAX_PATH
        && path
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'#');
    fine.then(|| path.to_owned())
}

fn clean_versions(raw: Option<&str>) -> Vec<u32> {
    raw.unwrap_or("")
        .split(',')
        .filter_map(|text| text.trim().parse().ok())
        .take(MAX_VERSIONS)
        .collect()
}

/// An address worth giving a client. A record naming loopback would point a person at a service on their own machine, and a
/// link-local one needs a scope no URL carries.
fn usable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback() || v4.is_unspecified() || v4.is_multicast() || v4.is_link_local())
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unicast_link_local())
        }
    }
}

/// One DNS-SD answer, as it came off the wire.
#[derive(Debug)]
pub struct Record<'a> {
    /// The instance name.
    pub instance: &'a str,
    /// The SRV target.
    pub host: &'a str,
    /// The addresses given for it.
    pub addresses: Vec<IpAddr>,
    /// The SRV port.
    pub port: u16,
    /// The TXT keys, in lower case, since DNS-SD ignores their case.
    pub txt: &'a HashMap<String, String>,
}

/// Reads an answer, which anyone on the link can send. `None` for one this version does not understand, for one that says it
/// is plain beyond loopback, which no client may use, and for one naming nothing usable.
#[must_use]
pub fn candidate_from(record: &Record<'_>) -> Option<Candidate> {
    let txt = |key: &str| record.txt.get(key).map(String::as_str);
    if txt("txtvers") != Some("1") || txt("proto") != Some("acp") || record.port == 0 {
        return None;
    }
    if txt("tls") != Some("1") {
        return None;
    }
    let label = clean_label(record.instance)?;
    let path = clean_path(txt("path"))?;
    let host = record.host.trim_end_matches('.');
    let authority = if host.to_ascii_lowercase().ends_with(".local") || !valid_host(host) {
        let mut addresses: Vec<IpAddr> = record
            .addresses
            .iter()
            .copied()
            .filter(|ip| usable(*ip))
            .collect();
        // The same answer gives the same address, and IPv4 first since a client is likelier to reach it.
        addresses.sort_by_key(|ip| (ip.is_ipv6(), *ip));
        SocketAddr::new(*addresses.first()?, record.port).to_string()
    } else {
        format!("{host}:{}", record.port)
    };
    let url = Url::parse(&format!("wss://{authority}{path}")).ok()?;
    Some(Candidate {
        label,
        url: url.to_string(),
        peer: clean_peer(txt("peer")),
        source: Source::Network,
        webtransport: txt("wt")
            .and_then(|port| port.parse().ok())
            .filter(|port| *port != 0),
        webtransport_hash: clean_peer(txt("wth")),
        acp_versions: clean_versions(txt("acpv")),
    })
}

/// This user's descriptor directory: only they can write there, so a descriptor is as good as anything else they run.
#[must_use]
pub fn descriptors_dir() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var(DESCRIPTORS_ENV) {
        return Some(dir);
    }
    let base = var("LOCALAPPDATA")
        .or_else(|| var("XDG_RUNTIME_DIR"))
        .or_else(|| var("HOME").map(|home| home.join(".local").join("state")))?;
    Some(base.join("coqui-acp").join("agents"))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    }
}

fn candidate_from_descriptor(text: &str) -> Option<Candidate> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("txtvers").and_then(Value::as_u64) != Some(1)
        || value.get("proto").and_then(Value::as_str) != Some("acp")
    {
        return None;
    }
    let url = Url::parse(value.get("url")?.as_str()?).ok()?;
    if !matches!(url.scheme(), "ws" | "wss") || !is_loopback_host(&url) {
        return None;
    }
    Some(Candidate {
        label: clean_label(value.get("label")?.as_str()?)?,
        url: url.to_string(),
        peer: clean_peer(value.get("peer").and_then(Value::as_str)),
        source: Source::ThisMachine,
        webtransport: value
            .get("wt")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0),
        webtransport_hash: clean_peer(value.get("wth").and_then(Value::as_str)),
        acp_versions: clean_versions(value.get("acpv").and_then(Value::as_str)),
    })
}

fn listening(url: &str) -> bool {
    Url::parse(url)
        .and_then(|url| {
            url.socket_addrs(|| None)
                .map_err(|_| url::ParseError::InvalidPort)
        })
        .is_ok_and(|addresses| {
            addresses
                .iter()
                .any(|address| TcpStream::connect_timeout(address, LIVENESS).is_ok())
        })
}

/// Agents on this machine that are still there. A descriptor nothing answers at is left over from a `serve` that was killed,
/// and is removed.
#[must_use]
pub fn read_descriptors(dir: &Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten().take(MAX_DESCRIPTOR_FILES) {
        if found.len() >= MAX_CANDIDATES {
            break;
        }
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut text = String::new();
        if file
            .take(MAX_DESCRIPTOR_BYTES)
            .read_to_string(&mut text)
            .is_err()
        {
            continue;
        }
        let Some(candidate) = candidate_from_descriptor(&text) else {
            continue;
        };
        if listening(&candidate.url) {
            found.push(candidate);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    found
}

/// Searches the link for `wait`, and reads this user's descriptors. An mDNS service that cannot start leaves the descriptors.
#[must_use]
pub fn discover(wait: Duration) -> Vec<Candidate> {
    let mut found = descriptors_dir().map_or_else(Vec::new, |dir| read_descriptors(&dir));
    match browse(wait) {
        Ok(network) => {
            for candidate in network {
                if found.len() >= MAX_CANDIDATES {
                    break;
                }
                if !found
                    .iter()
                    .any(|known| known.label == candidate.label && known.url == candidate.url)
                {
                    found.push(candidate);
                }
            }
        }
        Err(error) => eprintln!("the network could not be searched: {error}"),
    }
    found
}

/// Searches the link for `wait`.
///
/// # Errors
/// [`Error::Discovery`] when the mDNS service cannot start.
pub fn browse(wait: Duration) -> Result<Vec<Candidate>, Error> {
    let daemon = ServiceDaemon::new().map_err(|error| Error::Discovery(error.to_string()))?;
    let events = daemon
        .browse(SERVICE_TYPE)
        .map_err(|error| Error::Discovery(error.to_string()))?;
    let suffix = format!(".{SERVICE_TYPE}");
    let until = Instant::now() + wait;
    // By instance, since one record arrives more than once, and an answer replaces the one before it.
    let mut found: HashMap<String, Candidate> = HashMap::new();
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(service)) => {
                let fullname = service.get_fullname();
                let instance = fullname
                    .strip_suffix(&suffix)
                    .unwrap_or(fullname)
                    .to_owned();
                if found.len() >= MAX_CANDIDATES && !found.contains_key(&instance) {
                    continue;
                }
                // The first of a repeated key counts, as RFC 6763 says.
                let mut txt = HashMap::new();
                for property in service.get_properties().iter() {
                    txt.entry(property.key().to_ascii_lowercase())
                        .or_insert_with(|| property.val_str().to_owned());
                }
                let record = Record {
                    instance: &instance,
                    host: service.get_hostname(),
                    addresses: service
                        .get_addresses()
                        .iter()
                        .map(mdns_sd::ScopedIp::to_ip_addr)
                        .collect(),
                    port: service.get_port(),
                    txt: &txt,
                };
                match candidate_from(&record) {
                    Some(candidate) => {
                        found.insert(instance, candidate);
                    }
                    None => {
                        found.remove(&instance);
                    }
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                found.remove(fullname.strip_suffix(&suffix).unwrap_or(&fullname));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    let mut list: Vec<Candidate> = found.into_values().collect();
    list.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.url.cmp(&b.url)));
    Ok(list)
}

/// A `serve`'s advertisement, for as long as this lasts.
pub struct Advertised(Published);

enum Published {
    Network(ServiceDaemon),
    Descriptor(PathBuf),
}

impl fmt::Debug for Advertised {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Published::Network(_) => "Advertised(network)",
            Published::Descriptor(_) => "Advertised(descriptor)",
        })
    }
}

impl Drop for Advertised {
    fn drop(&mut self) {
        match &self.0 {
            Published::Network(daemon) => {
                let _ = daemon.shutdown();
            }
            Published::Descriptor(path) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Says that a listener at `bound` exists: a DNS-SD record, or a descriptor when it is on loopback, which nothing on the link
/// could reach. Off unless asked for, since it tells everyone on the link that an agent exists.
///
/// # Errors
/// [`Error::Discovery`] when it could not be published.
pub fn advertise(advertisement: &Advertisement, bound: SocketAddr) -> Result<Advertised, Error> {
    if bound.ip().is_loopback() {
        return write_descriptor(advertisement, bound);
    }
    let fail = |error: mdns_sd::Error| Error::Discovery(error.to_string());
    let daemon = ServiceDaemon::new().map_err(fail)?;
    // Not the machine's name: nothing here needs it known, and it would tell the link more than the agent's label does.
    let host = advertisement.host.as_ref().map_or_else(
        || format!("acp-{}-{}.local.", std::process::id(), bound.port()),
        |name| format!("{}.", name.trim_end_matches('.')),
    );
    let txt = advertisement.txt();
    let info = if bound.ip().is_unspecified() {
        ServiceInfo::new(
            SERVICE_TYPE,
            &advertisement.label,
            &host,
            "",
            bound.port(),
            txt,
        )
        .map_err(fail)?
        .enable_addr_auto()
    } else {
        ServiceInfo::new(
            SERVICE_TYPE,
            &advertisement.label,
            &host,
            bound.ip(),
            bound.port(),
            txt,
        )
        .map_err(fail)?
    };
    daemon.register(info).map_err(fail)?;
    Ok(Advertised(Published::Network(daemon)))
}

fn write_descriptor(advertisement: &Advertisement, bound: SocketAddr) -> Result<Advertised, Error> {
    let fail = |error: io::Error| {
        Error::Discovery(format!("the descriptor could not be written: {error}"))
    };
    let dir = descriptors_dir()
        .ok_or_else(|| Error::Discovery("no directory for descriptors".to_owned()))?;
    create_private_dir(&dir).map_err(fail)?;
    let scheme = if advertisement.tls { "wss" } else { "ws" };
    let text = json!({
        "txtvers": 1,
        "proto": "acp",
        "label": advertisement.label,
        "url": format!("{scheme}://{bound}{DEFAULT_PATH}"),
        "peer": advertisement.peer,
        "wt": advertisement.webtransport,
        "wth": advertisement.webtransport_hash,
    })
    .to_string();
    let name = format!("{}-{}", std::process::id(), bound.port());
    let unfinished = dir.join(format!("{name}.tmp"));
    let path = dir.join(format!("{name}.json"));
    std::fs::write(&unfinished, text).map_err(fail)?;
    std::fs::rename(&unfinished, &path).map_err(fail)?;
    Ok(Advertised(Published::Descriptor(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn read(
        instance: &str,
        host: &str,
        addresses: &[&str],
        port: u16,
        keys: &[(&str, &str)],
    ) -> Option<Candidate> {
        let txt: HashMap<String, String> = keys
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        candidate_from(&Record {
            instance,
            host,
            addresses: addresses.iter().map(|text| text.parse().unwrap()).collect(),
            port,
            txt: &txt,
        })
    }

    const GOOD: &[(&str, &str)] = &[("txtvers", "1"), ("proto", "acp"), ("tls", "1")];

    #[test]
    fn a_plain_record_gives_a_wss_url_and_the_default_path() {
        let found = read("Mine", "acp-1.local.", &["10.0.0.5"], 8765, GOOD).unwrap();
        assert_eq!(found.url, "wss://10.0.0.5:8765/acp");
        assert_eq!(found.label, "Mine");
        assert_eq!(found.source, Source::Network);
        assert_eq!(found.peer, None);
    }

    #[test]
    fn a_published_host_name_is_used_for_a_certificate_to_match() {
        let found = read("Mine", "agent.example.com.", &["10.0.0.5"], 443, GOOD).unwrap();
        assert_eq!(found.url, "wss://agent.example.com/acp");
    }

    #[test]
    fn it_ignores_what_it_does_not_understand() {
        assert!(
            read(
                "x",
                "h.local.",
                &["10.0.0.5"],
                1,
                &[("proto", "acp"), ("tls", "1")]
            )
            .is_none()
        );
        assert!(
            read(
                "x",
                "h.local.",
                &["10.0.0.5"],
                1,
                &[("txtvers", "2"), ("proto", "acp"), ("tls", "1")]
            )
            .is_none()
        );
        assert!(
            read(
                "x",
                "h.local.",
                &["10.0.0.5"],
                1,
                &[("txtvers", "1"), ("proto", "a2a"), ("tls", "1")]
            )
            .is_none()
        );
        assert!(read("x", "h.local.", &["10.0.0.5"], 0, GOOD).is_none());
    }

    #[test]
    fn a_record_cannot_point_a_person_at_their_own_machine_or_at_plain_text() {
        for address in [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "169.254.1.1",
            "fe80::1",
            "224.0.0.1",
        ] {
            assert!(
                read("x", "h.local.", &[address], 1, GOOD).is_none(),
                "{address}"
            );
        }
        assert!(
            read(
                "x",
                "h.local.",
                &["10.0.0.5"],
                1,
                &[("txtvers", "1"), ("proto", "acp")]
            )
            .is_none()
        );
        // A usable address beside it is still taken, and the loopback one is not.
        let found = read("x", "h.local.", &["127.0.0.1", "10.0.0.5"], 1, GOOD).unwrap();
        assert_eq!(found.url, "wss://10.0.0.5:1/acp");
    }

    #[test]
    fn hostile_text_is_cut_down_and_a_bad_peer_is_no_peer() {
        let long = "é".repeat(100);
        let bad_peer = [
            ("txtvers", "1"),
            ("proto", "acp"),
            ("tls", "1"),
            ("peer", "sha256:zz"),
        ];
        let found = read(
            &format!("a\u{7}\n{long}"),
            "h.local.",
            &["10.0.0.5"],
            1,
            &bad_peer,
        )
        .unwrap();
        assert!(found.label.len() <= MAX_LABEL);
        assert!(!found.label.chars().any(char::is_control));
        assert_eq!(found.peer, None);
        assert!(read("\u{7}", "h.local.", &["10.0.0.5"], 1, GOOD).is_none());
        let good_peer = [
            ("txtvers", "1"),
            ("proto", "acp"),
            ("tls", "1"),
            ("peer", PEER),
            ("wt", "99999"),
            ("acpv", "2, 1, x"),
        ];
        let found = read("x", "h.local.", &["10.0.0.5"], 1, &good_peer).unwrap();
        assert_eq!(found.peer.as_deref(), Some(PEER));
        assert_eq!(found.webtransport, None);
        assert_eq!(found.acp_versions, [2, 1]);
    }

    #[test]
    fn a_path_cannot_smuggle_anything_into_the_url() {
        for path in ["acp", "/a b", "/a#b", "/\u{e9}"] {
            let keys = [
                ("txtvers", "1"),
                ("proto", "acp"),
                ("tls", "1"),
                ("path", path),
            ];
            assert!(
                read("x", "h.local.", &["10.0.0.5"], 1, &keys).is_none(),
                "{path}"
            );
        }
    }

    #[test]
    fn a_descriptor_may_only_name_this_machine() {
        let descriptor =
            |url: &str| format!(r#"{{"txtvers":1,"proto":"acp","label":"x","url":"{url}"}}"#);
        assert!(candidate_from_descriptor(&descriptor("ws://127.0.0.1:1/acp")).is_some());
        assert!(candidate_from_descriptor(&descriptor("ws://[::1]:1/acp")).is_some());
        assert!(candidate_from_descriptor(&descriptor("ws://10.0.0.5:1/acp")).is_none());
        assert!(candidate_from_descriptor(&descriptor("http://127.0.0.1:1/")).is_none());
        assert!(candidate_from_descriptor("not json").is_none());
    }

    #[test]
    fn an_advertisement_refuses_what_a_record_cannot_carry() {
        assert!(Advertisement::new("", false).is_err());
        assert!(Advertisement::new(&"x".repeat(64), false).is_err());
        assert!(Advertisement::new("a\nb", false).is_err());
        assert!(
            Advertisement::new("Mine", false)
                .unwrap()
                .with_host(Some("a b".to_owned()))
                .is_err()
        );
    }
}
