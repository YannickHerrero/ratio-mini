//! Profiles and generators adapted from RatioMaster.NET, commit 10d28c4.
//! See third-party/RatioMaster.NET-LICENSE.txt for the upstream MIT license.
use clap::ValueEnum;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ClientProfile {
    #[default]
    Native,
    Qbittorrent524,
    Qbittorrent523,
    Qbittorrent514,
    Qbittorrent467,
    Transmission294,
    Transmission300,
}
impl ClientProfile {
    pub const ALL: [Self; 7] = [
        Self::Native,
        Self::Qbittorrent524,
        Self::Qbittorrent523,
        Self::Qbittorrent514,
        Self::Qbittorrent467,
        Self::Transmission294,
        Self::Transmission300,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "Ratio Mini",
            Self::Qbittorrent524 => "qBittorrent 5.2.4",
            Self::Qbittorrent523 => "qBittorrent 5.2.3",
            Self::Qbittorrent514 => "qBittorrent 5.1.4",
            Self::Qbittorrent467 => "qBittorrent 4.6.7",
            Self::Transmission294 => "Transmission 2.94",
            Self::Transmission300 => "Transmission 3.00",
        }
    }
    fn spec(self) -> Option<&'static Profile> {
        static CATALOG: OnceLock<Vec<Profile>> = OnceLock::new();
        CATALOG
            .get_or_init(|| {
                serde_json::from_str(include_str!("client_profiles.json"))
                    .expect("validated bundled profiles")
            })
            .iter()
            .find(|p| p.name == self.label())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    name: String,
    family: String,
    http_protocol: String,
    hash_upper_case: bool,
    unreserved: String,
    peer_id: PeerSpec,
    headers: Vec<String>,
    query: String,
    default_num_want: u16,
}
#[derive(Deserialize)]
struct PeerSpec {
    prefix: String,
}

pub struct Identity {
    pub profile: ClientProfile,
    pub peer: [u8; 20],
    pub key: String,
}

pub struct Values<'a> {
    pub hash: &'a [u8; 20],
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: Option<&'a str>,
    pub tracker_id: Option<&'a [u8]>,
}

pub fn encode(bytes: &[u8], upper: bool, unreserved: &str) -> String {
    let mut result = String::new();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || unreserved.as_bytes().contains(&byte) {
            result.push(char::from(byte));
        } else if upper {
            result.push_str(&format!("%{byte:02X}"));
        } else {
            result.push_str(&format!("%{byte:02x}"));
        }
    }
    result
}

impl Identity {
    pub fn new(profile: ClientProfile) -> Self {
        let mut rng = rand::thread_rng();
        let mut peer = *b"-RM0005-000000000000";
        let key;
        if let Some(spec) = profile.spec() {
            peer[..8].copy_from_slice(spec.peer_id.prefix.as_bytes());
            if spec.family == "Transmission" {
                const SET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
                let mut sum = 0;
                for byte in &mut peer[8..19] {
                    let n = rng.gen_range(0..36);
                    *byte = SET[n];
                    sum += n;
                }
                peer[19] = SET[(36 - sum % 36) % 36];
                key = format!("{:x}", rng.gen_range(0..i32::MAX));
            } else {
                const SET: &[u8] =
                    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz-_.!~*()";
                for byte in &mut peer[8..] {
                    *byte = SET[rng.gen_range(0..SET.len())];
                }
                key = format!("{:08X}", rng.next_u32());
            }
        } else {
            rng.fill_bytes(&mut peer[8..]);
            key = String::new();
        }
        Self { profile, peer, key }
    }

    pub fn url(&self, tracker: &str, values: &Values<'_>) -> Option<String> {
        let spec = self.profile.spec()?;
        let event = values
            .event
            .map_or(String::new(), |e| format!("&event={e}"));
        let query = spec
            .query
            .replace(
                "{infohash}",
                &encode(values.hash, spec.hash_upper_case, &spec.unreserved),
            )
            .replace(
                "{peerid}",
                &encode(&self.peer, spec.hash_upper_case, &spec.unreserved),
            )
            .replace("{port}", &values.port.to_string())
            .replace("{uploaded}", &values.uploaded.to_string())
            .replace("{downloaded}", &values.downloaded.to_string())
            .replace("{left}", &values.left.to_string())
            .replace(
                "{numwant}",
                &if values.event == Some("stopped") {
                    0
                } else {
                    spec.default_num_want
                }
                .to_string(),
            )
            .replace("{key}", &self.key)
            .replace("{event}", &event);
        let mut url = format!(
            "{tracker}{}{query}",
            if tracker.contains('?') { '&' } else { '?' }
        );
        if let Some(id) = values.tracker_id {
            url.push_str(&format!(
                "&trackerid={}",
                encode(id, spec.hash_upper_case, &spec.unreserved)
            ));
        }
        Some(url)
    }
}

pub fn request_bytes(profile: ClientProfile, url: &str) -> crate::Result<Vec<u8>> {
    let spec = profile
        .spec()
        .ok_or("Profil natif : utiliser le transport standard")?;
    let parsed = reqwest::Url::parse(url).map_err(|_| "URL d'annonce invalide")?;
    let host = parsed.host_str().ok_or("Hôte absent")?;
    let host = if let Some(port) = parsed.port() {
        format!("{host}:{port}")
    } else {
        host.to_owned()
    };
    let authority_end = url.find("://").ok_or("URL invalide")? + 3;
    let start = url[authority_end..]
        .find(['/', '?'])
        .map(|n| authority_end + n);
    let target = match start {
        None => "/".to_owned(),
        Some(n) if url.as_bytes()[n] == b'?' => format!("/{}", &url[n..]),
        Some(n) => url[n..].to_owned(),
    };
    if target.bytes().any(|b| b <= 32 || b >= 127) {
        return Err("Caractère interdit dans la requête".into());
    }
    let mut text = format!("GET {target} {}\r\n", spec.http_protocol);
    for header in &spec.headers {
        text.push_str(&header.replace("{host}", &host));
        text.push_str("\r\n");
    }
    text.push_str("\r\n");
    Ok(text.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_have_correct_length_alphabet_and_checksum() {
        for profile in ClientProfile::ALL {
            for _ in 0..100 {
                let identity = Identity::new(profile);
                assert_eq!(identity.peer.len(), 20);
                if let Some(spec) = profile.spec() {
                    assert!(identity.peer.starts_with(spec.peer_id.prefix.as_bytes()));
                    if spec.family == "Transmission" {
                        let total: usize = identity.peer[8..]
                            .iter()
                            .map(|byte| {
                                b"0123456789abcdefghijklmnopqrstuvwxyz"
                                    .iter()
                                    .position(|b| b == byte)
                                    .unwrap()
                            })
                            .sum();
                        assert_eq!(total % 36, 0);
                    } else {
                        assert_eq!(identity.key.len(), 8);
                        assert!(
                            identity
                                .key
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase())
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn requests_match_upstream_golden_bytes_for_every_profile_and_event() {
        let reference =
            include_str!("../tests/fixtures/upstream-requests.txt").replace("\r\n", "\n");
        let hash: [u8; 20] = std::array::from_fn(|i| (i * 11 + 5) as u8);
        for profile in ClientProfile::ALL
            .into_iter()
            .filter(|p| *p != ClientProfile::Native)
        {
            let spec = profile.spec().unwrap();
            let identity = Identity {
                profile,
                peer: format!("{}PEERIDSUFFIX", spec.peer_id.prefix)
                    .as_bytes()
                    .try_into()
                    .unwrap(),
                key: "KEY01234".into(),
            };
            let section = reference
                .split(&format!("=== {} ===\n", spec.name))
                .nth(1)
                .unwrap()
                .split("\n=== ")
                .next()
                .unwrap();
            for (label, event) in [
                ("Started", Some("started")),
                ("None", None),
                ("Completed", Some("completed")),
                ("Stopped", Some("stopped")),
            ] {
                let url = identity
                    .url(
                        "http://tracker.test/announce",
                        &Values {
                            hash: &hash,
                            port: 45678,
                            uploaded: 1_073_741_824,
                            downloaded: 536_870_912,
                            left: 268_435_456,
                            event,
                            tracker_id: None,
                        },
                    )
                    .unwrap();
                let actual = String::from_utf8(request_bytes(profile, &url).unwrap()).unwrap();
                let expected = section
                    .split(&format!("--- {label} ---\n"))
                    .nth(1)
                    .unwrap()
                    .split("--- ")
                    .next()
                    .unwrap();
                assert_eq!(
                    actual.replace('\r', "<CR>"),
                    expected,
                    "{} {label}",
                    spec.name
                );
            }
        }
    }
}
