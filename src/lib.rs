use clap::Parser;
use rand::Rng;
use reqwest::{Url, blocking::Client, redirect::Policy};
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, String>;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
mod profile_transport;
pub mod profiles;

#[derive(Parser)]
#[command(
    version,
    about = "Petit client de déclaration d'upload HTTP(S). Aperçu sans réseau par défaut."
)]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Args {
    /// Fichier .torrent (v1 ou hybride)
    pub torrent: PathBuf,
    /// Volume total à déclarer pendant cette session, en Mio
    #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u64).range(1..))]
    pub upload_mib: u64,
    /// Vitesse moyenne visée en Kio/s
    #[arg(long, default_value_t = 128, value_parser = clap::value_parser!(u64).range(1..))]
    pub rate_kib: u64,
    /// Variation aléatoire autour de la moyenne, en pour cent (0 = constant)
    #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub variation_percent: u8,
    /// Durée du démarrage progressif en secondes (0 = immédiat)
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u16).range(0..=3600))]
    pub ramp_seconds: u16,
    /// Effectuer les requêtes vers le tracker
    #[arg(long)]
    pub send: bool,
    /// Port annoncé (aucun port n'est ouvert par le programme)
    #[arg(long, default_value_t = 6881, value_parser = clap::value_parser!(u16).range(1..))]
    pub port: u16,
    /// Continuer même si le tracker indique zéro leecher
    #[arg(long)]
    pub ignore_swarm: bool,
    /// Profil d'annonce du client (les profils utilisent une connexion directe)
    #[arg(long, value_enum, default_value_t = profiles::ClientProfile::Native)]
    #[serde(default)]
    pub client_profile: profiles::ClientProfile,
}

#[cfg(feature = "gui")]
pub mod session_store;

#[derive(Debug)]
enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    List(Vec<Node<'a>>),
    Dict(BTreeMap<&'a [u8], Node<'a>>),
}
#[derive(Debug)]
struct Node<'a> {
    value: Value<'a>,
    raw: &'a [u8],
}
impl<'a> Node<'a> {
    fn get(&self, key: &[u8]) -> Option<&Node<'a>> {
        match &self.value {
            Value::Dict(d) => d.get(key),
            _ => None,
        }
    }
    fn number(&self) -> Option<u64> {
        match self.value {
            Value::Int(n) => Some(n),
            _ => None,
        }
    }
    fn bytes(&self) -> Option<&'a [u8]> {
        match self.value {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

fn parse(data: &[u8]) -> Result<Node<'_>> {
    fn node<'a>(data: &'a [u8], pos: &mut usize, depth: usize) -> Result<Node<'a>> {
        if depth > 64 {
            return Err("Bencode trop imbriqué".into());
        }
        let start = *pos;
        let value = match data.get(*pos).copied() {
            Some(b'i') => {
                *pos += 1;
                let begin = *pos;
                while data.get(*pos).is_some_and(u8::is_ascii_digit) {
                    *pos += 1;
                }
                let digits = &data[begin..*pos];
                if digits.is_empty()
                    || (digits.len() > 1 && digits[0] == b'0')
                    || data.get(*pos) != Some(&b'e')
                {
                    return Err("Entier bencode invalide".into());
                }
                let n = std::str::from_utf8(digits)
                    .unwrap()
                    .parse()
                    .map_err(|_| "Entier trop grand")?;
                *pos += 1;
                Value::Int(n)
            }
            Some(b'0'..=b'9') => {
                let begin = *pos;
                while data.get(*pos).is_some_and(u8::is_ascii_digit) {
                    *pos += 1;
                }
                let digits = &data[begin..*pos];
                if (digits.len() > 1 && digits[0] == b'0') || data.get(*pos) != Some(&b':') {
                    return Err("Longueur bencode invalide".into());
                }
                let len: usize = std::str::from_utf8(digits)
                    .unwrap()
                    .parse()
                    .map_err(|_| "Chaîne trop longue")?;
                *pos += 1;
                let end = pos.checked_add(len).ok_or("Longueur invalide")?;
                let bytes = data.get(*pos..end).ok_or("Chaîne tronquée")?;
                *pos = end;
                Value::Bytes(bytes)
            }
            Some(b'l') => {
                *pos += 1;
                let mut list = Vec::new();
                while data.get(*pos) != Some(&b'e') {
                    list.push(node(data, pos, depth + 1)?);
                }
                *pos += 1;
                Value::List(list)
            }
            Some(b'd') => {
                *pos += 1;
                let mut dict = BTreeMap::new();
                while data.get(*pos) != Some(&b'e') {
                    let key = node(data, pos, depth + 1)?.bytes().ok_or("Clé invalide")?;
                    let value = node(data, pos, depth + 1)?;
                    if dict.insert(key, value).is_some() {
                        return Err("Clé bencode dupliquée".into());
                    }
                }
                *pos += 1;
                Value::Dict(dict)
            }
            _ => return Err("Bencode invalide ou tronqué".into()),
        };
        Ok(Node {
            value,
            raw: &data[start..*pos],
        })
    }
    let mut pos = 0;
    let result = node(data, &mut pos, 0)?;
    if pos != data.len() {
        return Err("Octets supplémentaires après le bencode".into());
    }
    Ok(result)
}

struct Torrent {
    tracker: Url,
    hash: [u8; 20],
    size: u64,
}
fn torrent(data: &[u8]) -> Result<Torrent> {
    let root = parse(data)?;
    let announce = root
        .get(b"announce")
        .and_then(Node::bytes)
        .ok_or("Champ announce absent")?;
    let tracker = Url::parse(std::str::from_utf8(announce).map_err(|_| "URL non UTF-8")?)
        .map_err(|_| "URL du tracker invalide")?;
    if !matches!(tracker.scheme(), "http" | "https") {
        return Err("Seuls les trackers HTTP(S) sont pris en charge".into());
    }
    if tracker.host_str().is_none()
        || !tracker.username().is_empty()
        || tracker.password().is_some()
        || tracker.fragment().is_some()
    {
        return Err("URL de tracker non prise en charge".into());
    }
    let reserved = [
        "info_hash",
        "peer_id",
        "port",
        "uploaded",
        "downloaded",
        "left",
        "event",
        "compact",
        "numwant",
    ];
    if tracker
        .query_pairs()
        .any(|(key, _)| reserved.contains(&key.as_ref()))
    {
        return Err("URL contenant déjà des paramètres d'annonce réservés".into());
    }
    let info = root.get(b"info").ok_or("Champ info absent")?;
    let pieces = info
        .get(b"pieces")
        .and_then(Node::bytes)
        .ok_or("Torrent v1 requis (v2 seul non pris en charge)")?;
    if pieces.len() % 20 != 0 {
        return Err("Champ pieces invalide".into());
    }
    let size = match (info.get(b"length"), info.get(b"files")) {
        (Some(length), None) => length.number().ok_or("Taille invalide")?,
        (None, Some(files)) => {
            let Value::List(files) = &files.value else {
                return Err("Liste files invalide".into());
            };
            files.iter().try_fold(0_u64, |sum, f| {
                sum.checked_add(
                    f.get(b"length")
                        .and_then(Node::number)
                        .ok_or("Taille de fichier invalide")?,
                )
                .ok_or("Taille totale trop grande")
            })?
        }
        _ => return Err("Champs length/files invalides".into()),
    };
    Ok(Torrent {
        tracker,
        hash: Sha1::digest(info.raw).into(),
        size,
    })
}

/// Validate locally and return the torrent's info hash (no tracker contact).
pub fn inspect(path: &std::path::Path) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "Impossible d'ouvrir le .torrent")?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Lecture du .torrent impossible")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Torrent supérieur à 16 Mio".into());
    }
    Ok(torrent(&bytes)?
        .hash
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn escape(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("%{b:02X}")).collect()
}
fn announce_url(
    t: &Torrent,
    peer: &[u8; 20],
    port: u16,
    uploaded: u64,
    event: Option<&str>,
    tracker_id: Option<&[u8]>,
) -> String {
    let mut url = t.tracker.as_str().to_owned();
    url.push(if t.tracker.query().is_some() {
        '&'
    } else {
        '?'
    });
    url.push_str(&format!("info_hash={}&peer_id={}&port={port}&uploaded={uploaded}&downloaded=0&left=0&compact=1&numwant=0", escape(&t.hash), escape(peer)));
    if let Some(event) = event {
        url.push_str(&format!("&event={event}"));
    }
    if let Some(id) = tracker_id {
        url.push_str(&format!("&trackerid={}", escape(id)));
    }
    url
}

struct Reply {
    interval: u64,
    tracker_id: Option<Vec<u8>>,
    seeders: Option<u64>,
    leechers: Option<u64>,
}
impl Reply {
    fn keep_known_statistics(&mut self, previous: &Self) {
        self.seeders = self.seeders.or(previous.seeders);
        self.leechers = self.leechers.or(previous.leechers);
    }
}
#[derive(Debug)]
enum AnnounceError {
    Temporary {
        message: String,
        retry_after: Option<u64>,
    },
    Rejected(String),
}
impl From<&str> for AnnounceError {
    fn from(value: &str) -> Self {
        Self::Rejected(value.into())
    }
}
impl From<String> for AnnounceError {
    fn from(value: String) -> Self {
        Self::Rejected(value)
    }
}
impl AnnounceError {
    fn message(&self) -> &str {
        match self {
            Self::Temporary { message, .. } | Self::Rejected(message) => message,
        }
    }
}

fn announce(client: &Client, url: String) -> std::result::Result<Reply, AnnounceError> {
    let response = client
        .get(url)
        .send()
        .map_err(|_| AnnounceError::Temporary {
            message: "Échec réseau/TLS (URL masquée pour préserver la passkey)".into(),
            retry_after: None,
        })?;
    if !response.status().is_success() {
        let message = format!("Le tracker a renvoyé HTTP {}", response.status().as_u16());
        return Err(
            if response.status().is_server_error() || response.status().as_u16() == 429 {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                AnnounceError::Temporary {
                    message,
                    retry_after,
                }
            } else {
                AnnounceError::Rejected(message)
            },
        );
    }
    let mut bytes = Vec::new();
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AnnounceError::Temporary {
            message: "Lecture de la réponse impossible".into(),
            retry_after: None,
        })?;
    if bytes.len() > 1024 * 1024 {
        return Err("Réponse du tracker trop volumineuse".into());
    }
    parse_reply(&bytes).map_err(AnnounceError::Rejected)
}

fn announce_profile(
    client: &Client,
    profile: profiles::ClientProfile,
    url: String,
) -> std::result::Result<Reply, AnnounceError> {
    if profile == profiles::ClientProfile::Native {
        return announce(client, url);
    }
    let response = profile_transport::get(profile, &url)?;
    if !(200..300).contains(&response.status) {
        let message = format!("Le tracker a renvoyé HTTP {}", response.status);
        return Err(if response.status == 429 || response.status >= 500 {
            AnnounceError::Temporary {
                message,
                retry_after: response.retry_after,
            }
        } else {
            AnnounceError::Rejected(message)
        });
    }
    parse_reply(&response.body).map_err(AnnounceError::Rejected)
}

fn retry_request(
    mut request: impl FnMut() -> std::result::Result<Reply, AnnounceError>,
    mut wait: impl FnMut(Duration) -> bool,
    emit: &impl Fn(Event),
) -> Result<Option<Reply>> {
    for attempt in 0..4_u8 {
        match request() {
            Ok(reply) => return Ok(Some(reply)),
            Err(AnnounceError::Temporary {
                message,
                retry_after,
            }) if attempt < 3 => {
                // An unusually long Retry-After must not be shortened into an earlier retry.
                if retry_after.is_some_and(|delay| delay > 86400) {
                    return Err(
                        "Le tracker demande une attente supérieure à 24 h ; session arrêtée."
                            .into(),
                    );
                }
                let seconds = (60_u64 << attempt).max(retry_after.unwrap_or(0));
                emit(Event::Log(format!(
                    "{message}. Nouvelle tentative dans {seconds} s ({}/3).",
                    attempt + 1
                )));
                emit(Event::Retrying {
                    seconds,
                    attempt: attempt + 1,
                });
                if !wait(Duration::from_secs(seconds)) {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error.message().to_owned()),
        }
    }
    unreachable!()
}

fn wait_until_retry(stop: &AtomicBool, delay: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < delay {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(Duration::from_millis(100).min(delay.saturating_sub(start.elapsed())));
    }
    !stop.load(Ordering::SeqCst)
}

fn parse_reply(bytes: &[u8]) -> Result<Reply> {
    let root = parse(bytes)?;
    if root.get(b"failure reason").is_some() {
        return Err(
            "Annonce refusée par le tracker (texte masqué car il peut contenir la passkey)".into(),
        );
    }
    let interval = root
        .get(b"interval")
        .and_then(Node::number)
        .filter(|v| *v > 0)
        .ok_or("Réponse sans intervalle valide")?;
    let minimum = root
        .get(b"min interval")
        .and_then(Node::number)
        .unwrap_or(0);
    Ok(Reply {
        interval: interval.max(minimum).max(30),
        tracker_id: root
            .get(b"tracker id")
            .and_then(Node::bytes)
            .map(<[u8]>::to_vec),
        seeders: root.get(b"complete").and_then(Node::number),
        leechers: root.get(b"incomplete").and_then(Node::number),
    })
}

struct Traffic {
    rate: u64,
    variation: u8,
    current_rate: u128,
    previous: Duration,
    changed: Duration,
    byte_nanos: u128,
    target: u64,
    ramp: Duration,
}

impl Traffic {
    fn new(rate: u64, variation: u8, target: u64, ramp_seconds: u16) -> Self {
        let mut traffic = Self {
            rate,
            variation,
            current_rate: 0,
            previous: Duration::ZERO,
            changed: Duration::ZERO,
            byte_nanos: 0,
            target,
            ramp: Duration::from_secs(u64::from(ramp_seconds)),
        };
        traffic.resample();
        traffic
    }

    fn resample(&mut self) {
        let width = u64::from(self.variation);
        let percent = rand::thread_rng().gen_range((100 - width)..=(100 + width));
        let sampled = u128::from(self.rate) * u128::from(percent) / 100;
        self.current_rate = if self.current_rate == 0 {
            sampled
        } else {
            (3 * self.current_rate + sampled) / 4
        };
    }

    #[cfg(test)]
    fn advance(&mut self, now: Duration) -> u64 {
        self.advance_paused(now, false)
    }

    fn advance_paused(&mut self, now: Duration, paused: bool) -> u64 {
        let elapsed = ramp_weight(self.previous, now, self.ramp);
        if !paused {
            self.byte_nanos = self
                .byte_nanos
                .saturating_add(elapsed.saturating_mul(self.current_rate))
                .min(u128::from(self.target) * 1_000_000_000);
        }
        self.previous = now;
        if now.saturating_sub(self.changed) >= Duration::from_secs(5) {
            self.resample();
            self.changed = now;
        }
        (self.byte_nanos / 1_000_000_000) as u64
    }
}

// Intégrale du facteur linéaire de démarrage, exprimée en nanosecondes.
fn ramp_weight(from: Duration, to: Duration, ramp: Duration) -> u128 {
    if to <= from {
        return 0;
    }
    if ramp.is_zero() {
        return to.saturating_sub(from).as_nanos();
    }
    let r = ramp.as_nanos();
    let a = from.as_nanos().min(r);
    let b = to.as_nanos().min(r);
    (b * b - a * a) / (2 * r)
        + to.as_nanos()
            .saturating_sub(r)
            .saturating_sub(from.as_nanos().saturating_sub(r))
}

#[derive(Debug, Clone)]
pub enum Event {
    Log(String),
    Retrying {
        seconds: u64,
        attempt: u8,
    },
    Swarm {
        seeders: Option<u64>,
        leechers: Option<u64>,
        paused: bool,
    },
    Progress {
        simulated: u64,
        announced: u64,
        rate_kib: f64,
    },
}

pub fn run_session(args: Args, stop: Arc<AtomicBool>, emit: impl Fn(Event)) -> Result<()> {
    macro_rules! report { ($($arg:tt)*) => { emit(Event::Log(format!($($arg)*))) }; }
    if args.rate_kib == 0
        || args.upload_mib == 0
        || args.variation_percent > 100
        || args.ramp_seconds > 3600
        || args.port == 0
    {
        return Err("Paramètres de session invalides".into());
    }
    let target = args
        .upload_mib
        .checked_mul(1024 * 1024)
        .ok_or("Volume trop grand")?;
    let rate = args
        .rate_kib
        .checked_mul(1024)
        .ok_or("Vitesse trop grande")?;
    let mut bytes = Vec::new();
    fs::File::open(&args.torrent)
        .map_err(|_| "Impossible d'ouvrir le .torrent")?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Lecture du .torrent impossible")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Torrent supérieur à 16 Mio".into());
    }
    let t = torrent(&bytes)?;
    report!(
        "Tracker : {}://{} (chemin et passkey masqués)",
        t.tracker.scheme(),
        t.tracker.host_str().unwrap_or("?")
    );
    report!("Taille du torrent : {} octets", t.size);
    report!(
        "Session : {} Mio, moyenne visée {} Kio/s, variation +/-{} %. downloaded=0, left=0.",
        args.upload_mib,
        args.rate_kib,
        args.variation_percent
    );
    report!(
        "Démarrage progressif : {} s ; variations lissées.",
        args.ramp_seconds
    );
    if !args.send {
        report!("APERÇU : aucune requête réseau. Ajouter --send pour démarrer.");
        return Ok(());
    }
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("ratio-mini/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Initialisation HTTP impossible")?;
    let identity = profiles::Identity::new(args.client_profile);
    let peer = identity.peer;
    report!("Profil : {}", args.client_profile.label());
    let make_url = |amount, event, tracker_id: Option<&[u8]>| {
        identity
            .url(
                t.tracker.as_str(),
                &profiles::Values {
                    hash: &t.hash,
                    port: args.port,
                    uploaded: amount,
                    downloaded: 0,
                    left: 0,
                    event,
                    tracker_id,
                },
            )
            .unwrap_or_else(|| announce_url(&t, &peer, args.port, amount, event, tracker_id))
    };
    let start_url = make_url(0, Some("started"), None);
    let Some(mut reply) = retry_request(
        || announce_profile(&client, args.client_profile, start_url.clone()),
        |delay| wait_until_retry(&stop, delay),
        &emit,
    )?
    else {
        return Ok(());
    };
    let mut paused = !args.ignore_swarm && reply.leechers == Some(0);
    emit(Event::Swarm {
        seeders: reply.seeders,
        leechers: reply.leechers,
        paused,
    });
    let start = Instant::now();
    let mut traffic = Traffic::new(rate, args.variation_percent, target, args.ramp_seconds);
    let mut last = start;
    let mut last_progress = start;
    let mut announced = 0;
    report!(
        "started accepté. Intervalle : {} s. Ctrl+C pour arrêter.",
        reply.interval
    );
    loop {
        let mut amount = traffic.advance_paused(start.elapsed(), paused);
        let stopping = stop.load(Ordering::SeqCst);
        if last_progress.elapsed() >= Duration::from_millis(500) {
            let ramp_factor = if args.ramp_seconds == 0 {
                1.0
            } else {
                (start.elapsed().as_secs_f64() / f64::from(args.ramp_seconds)).min(1.0)
            };
            emit(Event::Progress {
                simulated: amount,
                announced,
                rate_kib: if paused {
                    0.0
                } else {
                    traffic.current_rate as f64 / 1024.0 * ramp_factor
                },
            });
            last_progress = Instant::now();
        }
        if !stopping && last.elapsed().as_secs() < reply.interval {
            thread::sleep(Duration::from_millis(100));
            continue;
        }
        let mut done = stopping || amount == target;
        let url = make_url(
            amount,
            if done { Some("stopped") } else { None },
            reply.tracker_id.as_deref(),
        );
        let mut next = if done {
            announce_profile(&client, args.client_profile, url)
                .map_err(|error| error.message().to_owned())?
        } else {
            match retry_request(
                || announce_profile(&client, args.client_profile, url.clone()),
                |delay| wait_until_retry(&stop, delay),
                &emit,
            )? {
                Some(next) => next,
                None => {
                    done = true;
                    amount = traffic.advance_paused(start.elapsed(), paused);
                    announce_profile(
                        &client,
                        args.client_profile,
                        make_url(amount, Some("stopped"), reply.tracker_id.as_deref()),
                    )
                    .map_err(|error| error.message().to_owned())?
                }
            }
        };
        report!(
            "{} : {} octets déclarés acceptés par le tracker.",
            if done { "stopped" } else { "announce" },
            amount
        );
        announced = amount;
        emit(Event::Progress {
            simulated: amount,
            announced,
            rate_kib: 0.0,
        });
        if done {
            break;
        }
        // Count time spent in the request under the previous state before applying new statistics.
        traffic.advance_paused(start.elapsed(), paused);
        next.keep_known_statistics(&reply);
        let next_paused = !args.ignore_swarm && next.leechers == Some(0);
        if next_paused != paused {
            report!(
                "{}",
                if next_paused {
                    "Upload suspendu : aucun leecher annoncé."
                } else {
                    "Reprise : de nouvelles statistiques ont été reçues."
                }
            );
        }
        paused = next_paused;
        emit(Event::Swarm {
            seeders: next.seeders,
            leechers: next.leechers,
            paused,
        });
        let tracker_id = next.tracker_id.or(reply.tracker_id);
        reply = Reply {
            interval: next.interval,
            tracker_id,
            seeders: next.seeders,
            leechers: next.leechers,
        };
        last = Instant::now();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temporary_failures_retry_with_backoff_and_server_delay() {
        let mut attempts = 0;
        let mut waits = Vec::new();
        let reply = retry_request(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(AnnounceError::Temporary {
                        message: "indisponible".into(),
                        retry_after: Some(90),
                    })
                } else {
                    parse_reply(b"d8:intervali60e5:peers0:e").map_err(AnnounceError::Rejected)
                }
            },
            |delay| {
                waits.push(delay.as_secs());
                true
            },
            &|_| {},
        )
        .unwrap();
        assert!(reply.is_some());
        assert_eq!(waits, [90, 120]);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn rejection_never_retries_and_wait_can_be_cancelled() {
        let mut attempts = 0;
        assert!(
            retry_request(
                || {
                    attempts += 1;
                    Err(AnnounceError::Rejected("refus".into()))
                },
                |_| panic!("must not wait"),
                &|_| {}
            )
            .is_err()
        );
        assert_eq!(attempts, 1);
        let cancelled = retry_request(
            || {
                Err(AnnounceError::Temporary {
                    message: "réseau".into(),
                    retry_after: None,
                })
            },
            |_| false,
            &|_| {},
        )
        .unwrap();
        assert!(cancelled.is_none());
    }

    #[test]
    fn retries_are_bounded_and_never_shorten_extreme_server_delay() {
        let mut attempts = 0;
        let mut waits = 0;
        assert!(
            retry_request(
                || {
                    attempts += 1;
                    Err(AnnounceError::Temporary {
                        message: "réseau".into(),
                        retry_after: None,
                    })
                },
                |_| {
                    waits += 1;
                    true
                },
                &|_| {}
            )
            .is_err()
        );
        assert_eq!((attempts, waits), (4, 3));
        assert!(
            retry_request(
                || Err(AnnounceError::Temporary {
                    message: "limite".into(),
                    retry_after: Some(100000)
                }),
                |_| panic!("must stop"),
                &|_| {}
            )
            .is_err()
        );
    }
    #[test]
    fn swarm_statistics_distinguish_missing_and_zero() {
        let unknown = parse_reply(b"d8:intervali60e5:peers0:e").unwrap();
        assert_eq!(unknown.leechers, None);
        let empty =
            parse_reply(b"d8:completei12e10:incompletei0e8:intervali60e5:peers0:e").unwrap();
        assert_eq!((empty.seeders, empty.leechers), (Some(12), Some(0)));
        let mut missing = parse_reply(b"d8:intervali60e5:peers0:e").unwrap();
        missing.keep_known_statistics(&empty);
        assert_eq!(missing.leechers, Some(0));
        let mut positive = parse_reply(b"d10:incompletei2e8:intervali60e5:peers0:e").unwrap();
        positive.keep_known_statistics(&missing);
        assert_eq!((positive.seeders, positive.leechers), (Some(12), Some(2)));
    }

    #[test]
    fn pause_does_not_accumulate_hidden_upload_or_catch_up() {
        let mut traffic = Traffic::new(1000, 0, u64::MAX, 0);
        assert_eq!(traffic.advance_paused(Duration::from_secs(5), false), 5000);
        assert_eq!(traffic.advance_paused(Duration::from_secs(65), true), 5000);
        assert_eq!(
            traffic.advance_paused(Duration::from_secs(70), false),
            10000
        );
    }
    #[test]
    fn concurrent_sessions_stop_independently_and_send_final_announces() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let announce = format!("http://{}/announce", listener.local_addr().unwrap());
        let fixture =
            std::env::temp_dir().join(format!("ratio-mini-test-{}.torrent", rand::random::<u64>()));
        let mut data = format!("d8:announce{}:{}4:info", announce.len(), announce).into_bytes();
        data.extend_from_slice(b"d6:lengthi42e6:pieces0:ee");
        fs::write(&fixture, data).unwrap();
        let server = thread::spawn(move || {
            let mut starts = 0;
            let mut stops = 0;
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&request);
                starts += usize::from(text.contains("event=started"));
                stops += usize::from(text.contains("event=stopped"));
                let body = b"d8:intervali30e5:peers0:e";
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
            (starts, stops)
        });
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = fixture.clone();
            workers.push(thread::spawn(move || {
                let stop = Arc::new(AtomicBool::new(false));
                let signal = stop.clone();
                run_session(
                    Args {
                        torrent: path,
                        upload_mib: 1,
                        rate_kib: 128,
                        variation_percent: 25,
                        ramp_seconds: 30,
                        send: true,
                        port: 6881,
                        ignore_swarm: false,
                        client_profile: profiles::ClientProfile::Native,
                    },
                    stop,
                    |event| {
                        if let Event::Log(line) = event
                            && line.starts_with("started accepté")
                        {
                            signal.store(true, Ordering::SeqCst);
                        }
                    },
                )
            }));
        }
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert_eq!(server.join().unwrap(), (2, 2));
        fs::remove_file(fixture).unwrap();
    }
    #[test]
    fn malformed_inputs_are_rejected() {
        for data in [
            b"i01e".as_slice(),
            b"i-1e",
            b"3:ab",
            b"d1:ai1e1:ai2ee",
            b"leextra",
            b"d",
            b"999999999999999999999:a",
        ] {
            assert!(parse(data).is_err());
        }
        let mut deep = vec![b'l'; 100];
        deep.extend(vec![b'e'; 100]);
        assert!(parse(&deep).is_err());
    }
    #[test]
    fn hash_uses_original_info_bytes() {
        let data = b"d8:announce23:http://localhost/a?pk=x4:infod6:pieces0:6:lengthi42eee";
        let t = torrent(data).unwrap();
        assert_eq!(t.size, 42);
        assert_eq!(
            t.hash.as_slice(),
            Sha1::digest(b"d6:pieces0:6:lengthi42ee").as_slice()
        );
        let url = announce_url(&t, &[255; 20], 6881, 256, Some("started"), Some(b"a&b"));
        assert!(url.contains("?pk=x&info_hash="));
        assert!(url.contains(&format!("peer_id={}", "%FF".repeat(20))));
        assert!(url.ends_with("trackerid=%61%26%62"));
    }
    #[test]
    fn counters_are_bounded() {
        let mut constant = Traffic::new(1024, 0, 99999, 0);
        assert_eq!(constant.advance(Duration::from_millis(1500)), 1536);
        assert_eq!(constant.advance(Duration::from_secs(2)), 2048);
        assert_eq!(constant.advance(Duration::from_secs(u64::MAX)), 99999);
        let mut extreme = Traffic::new(u64::MAX, 100, 42, 0);
        assert!(extreme.advance(Duration::from_secs(u64::MAX)) <= 42);
    }
    #[test]
    fn variable_rate_stays_within_bounds_and_accumulates() {
        let mut traffic = Traffic::new(10000, 25, u64::MAX, 0);
        let mut previous = 0;
        for second in 1..=3600 {
            let amount = traffic.advance(Duration::from_secs(second));
            assert!((7500..=12500).contains(&(amount - previous)));
            previous = amount;
        }
    }
    #[test]
    fn ramp_integrates_partial_and_full_windows() {
        let mut traffic = Traffic::new(1000, 0, u64::MAX, 30);
        assert_eq!(traffic.advance(Duration::from_secs(15)), 3750);
        assert_eq!(traffic.advance(Duration::from_secs(30)), 15000);
        assert_eq!(traffic.advance(Duration::from_secs(60)), 45000);
        assert_eq!(
            ramp_weight(
                Duration::from_secs(45),
                Duration::from_secs(40),
                Duration::from_secs(30)
            ),
            0
        );
    }
    #[test]
    fn local_tracker_receives_request() {
        use std::{io::Write, net::TcpListener};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(
                String::from_utf8_lossy(&request).starts_with("GET /announce?uploaded=42 HTTP/1.1")
            );
            let body = b"d8:intervali60e5:peers0:e";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        assert_eq!(
            announce(&client, format!("http://{addr}/announce?uploaded=42"))
                .unwrap()
                .interval,
            60
        );
        server.join().unwrap();
    }
}
