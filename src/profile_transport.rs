//! HTTP/1.1 transport preserving the request bytes defined by bundled profiles.
use crate::{
    AnnounceError,
    profiles::{ClientProfile, request_bytes},
};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

const LIMIT: usize = 1024 * 1024;
const HEADER_LIMIT: usize = 64 * 1024;
pub struct Response {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Vec<u8>,
}

enum Connection {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}
impl Connection {
    fn socket(&self) -> &TcpStream {
        match self {
            Self::Plain(s) => s,
            Self::Tls(s) => &s.sock,
        }
    }
}
impl Read for Connection {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(bytes),
            Self::Tls(s) => s.read(bytes),
        }
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(bytes),
            Self::Tls(s) => s.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}
struct Deadline {
    stream: Connection,
    until: Instant,
}
impl Deadline {
    fn timeout(&self) -> io::Result<()> {
        let left = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "tracker timeout"))?;
        self.stream.socket().set_read_timeout(Some(left))?;
        self.stream.socket().set_write_timeout(Some(left))
    }
}
impl Read for Deadline {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.timeout()?;
        self.stream.read(bytes)
    }
}
impl Write for Deadline {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.timeout()?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.timeout()?;
        self.stream.flush()
    }
}
fn network(error: io::Error) -> AnnounceError {
    if error.kind() == io::ErrorKind::InvalidData {
        return AnnounceError::Rejected("Réponse réseau ou certificat TLS invalide".into());
    }
    AnnounceError::Temporary {
        message: "Échec du transport HTTP/TLS du profil (adresse masquée)".into(),
        retry_after: None,
    }
}
fn invalid(message: &str) -> AnnounceError {
    AnnounceError::Rejected(message.into())
}

pub fn get(profile: ClientProfile, url: &str) -> std::result::Result<Response, AnnounceError> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    get_with_config(profile, url, Arc::new(config))
}

fn get_with_config(
    profile: ClientProfile,
    url: &str,
    config: Arc<rustls::ClientConfig>,
) -> std::result::Result<Response, AnnounceError> {
    let request = request_bytes(profile, url).map_err(AnnounceError::Rejected)?;
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("URL du tracker invalide"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| invalid("Hôte absent"))?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| invalid("Port absent"))?;
    let until = Instant::now() + Duration::from_secs(20);
    // Resolver runs separately so a stuck OS DNS lookup cannot block session shutdown indefinitely.
    let (tx, rx) = std::sync::mpsc::channel();
    let dns_host = host.clone();
    std::thread::Builder::new()
        .name("tracker-dns".into())
        .spawn(move || {
            let addresses = (dns_host.as_str(), port)
                .to_socket_addrs()
                .map(|a| a.collect::<Vec<_>>());
            let _ = tx.send(addresses);
        })
        .map_err(network)?;
    let addresses = rx
        .recv_timeout(Duration::from_secs(20))
        .map_err(|_| network(io::Error::from(io::ErrorKind::TimedOut)))?
        .map_err(network)?;
    let mut socket = None;
    for addr in addresses {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        if let Ok(stream) = TcpStream::connect_timeout(&addr, remaining) {
            socket = Some(stream);
            break;
        }
    }
    let socket = socket.ok_or_else(|| network(io::Error::from(io::ErrorKind::NotConnected)))?;
    let connection = if parsed.scheme() == "https" {
        let name = rustls::pki_types::ServerName::try_from(host)
            .map_err(|_| invalid("Nom TLS invalide"))?;
        let conn = rustls::ClientConnection::new(config, name)
            .map_err(|_| invalid("Initialisation TLS impossible"))?;
        Connection::Tls(Box::new(rustls::StreamOwned::new(conn, socket)))
    } else if parsed.scheme() == "http" {
        Connection::Plain(socket)
    } else {
        return Err(invalid("Transport de profil limité à HTTP(S)"));
    };
    let mut stream = Deadline {
        stream: connection,
        until,
    };
    stream.write_all(&request).map_err(network)?;
    stream.flush().map_err(network)?;
    read_response(&mut BufReader::new(stream))
}

fn line(reader: &mut impl BufRead, limit: usize) -> std::result::Result<Vec<u8>, AnnounceError> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        reader.read_exact(&mut byte).map_err(network)?;
        line.push(byte[0]);
        if line.len() > limit {
            return Err(invalid("Ligne HTTP trop longue"));
        }
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
            return Ok(line);
        }
    }
}

fn read_response(reader: &mut impl BufRead) -> std::result::Result<Response, AnnounceError> {
    let mut header = Vec::new();
    loop {
        let next = line(reader, HEADER_LIMIT)?;
        header.extend_from_slice(&next);
        header.extend_from_slice(b"\r\n");
        if header.len() > HEADER_LIMIT {
            return Err(invalid("En-têtes HTTP trop volumineux"));
        }
        if next.is_empty() {
            break;
        }
    }
    let mut fields = [httparse::EMPTY_HEADER; 64];
    let mut response = httparse::Response::new(&mut fields);
    if !response
        .parse(&header)
        .map_err(|_| invalid("Réponse HTTP invalide"))?
        .is_complete()
    {
        return Err(invalid("En-têtes tronqués"));
    }
    let status = response.code.ok_or_else(|| invalid("Statut HTTP absent"))?;
    let mut length = None;
    let mut chunked = false;
    let mut compression = String::new();
    let mut retry_after = None;
    for field in response.headers.iter() {
        let value = std::str::from_utf8(field.value)
            .map_err(|_| invalid("Valeur d'en-tête invalide"))?
            .trim();
        if field.name.eq_ignore_ascii_case("content-length") {
            let size = value
                .parse::<usize>()
                .map_err(|_| invalid("Content-Length invalide"))?;
            if length.is_some_and(|n| n != size) {
                return Err(invalid("Longueurs HTTP contradictoires"));
            }
            length = Some(size);
        } else if field.name.eq_ignore_ascii_case("transfer-encoding") {
            if !value.eq_ignore_ascii_case("chunked") {
                return Err(invalid("Transfer-Encoding non pris en charge"));
            }
            chunked = true;
        } else if field.name.eq_ignore_ascii_case("content-encoding") {
            compression = value.to_ascii_lowercase();
        } else if field.name.eq_ignore_ascii_case("retry-after") {
            retry_after = value.parse().ok();
        }
    }
    if !(200..300).contains(&status) {
        return Ok(Response {
            status,
            retry_after,
            body: Vec::new(),
        });
    }
    if chunked && length.is_some() {
        return Err(invalid("Cadrage HTTP ambigu"));
    }
    let mut body = Vec::new();
    if chunked {
        let mut chunks = 0;
        loop {
            chunks += 1;
            if chunks > 65536 {
                return Err(invalid("Trop de fragments HTTP"));
            }
            let chunk_line = line(reader, 8192)?;
            let text =
                std::str::from_utf8(&chunk_line).map_err(|_| invalid("Fragment HTTP invalide"))?;
            let size = usize::from_str_radix(text.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| invalid("Taille de fragment invalide"))?;
            if size == 0 {
                let mut trailers = 0;
                loop {
                    let trailing = line(reader, HEADER_LIMIT)?;
                    trailers += trailing.len() + 2;
                    if trailers > HEADER_LIMIT {
                        return Err(invalid("Trailers HTTP trop longs"));
                    }
                    if trailing.is_empty() {
                        break;
                    }
                }
                break;
            }
            if size > LIMIT.saturating_sub(body.len()) {
                return Err(invalid("Réponse trop volumineuse"));
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..]).map_err(network)?;
            let mut end = [0; 2];
            reader.read_exact(&mut end).map_err(network)?;
            if end != *b"\r\n" {
                return Err(invalid("Fin de fragment invalide"));
            }
        }
    } else if let Some(size) = length {
        if size > LIMIT {
            return Err(invalid("Réponse trop volumineuse"));
        }
        body.resize(size, 0);
        reader.read_exact(&mut body).map_err(network)?;
    } else {
        reader
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut body)
            .map_err(network)?;
        if body.len() > LIMIT {
            return Err(invalid("Réponse trop volumineuse"));
        }
    }
    if !compression.is_empty() && compression != "identity" {
        let decoder: Box<dyn Read + '_> = match compression.as_str() {
            "gzip" => Box::new(flate2::read::GzDecoder::new(body.as_slice())),
            "deflate" => Box::new(flate2::read::ZlibDecoder::new(body.as_slice())),
            _ => return Err(invalid("Compression HTTP non prise en charge")),
        };
        let mut decoded = Vec::new();
        decoder
            .take(LIMIT as u64 + 1)
            .read_to_end(&mut decoded)
            .map_err(|_| invalid("Réponse compressée invalide"))?;
        if decoded.len() > LIMIT {
            return Err(invalid("Réponse décompressée trop volumineuse"));
        }
        body = decoded;
    }
    Ok(Response {
        status,
        retry_after,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn capture_and_reply(stream: &mut (impl Read + Write)) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            bytes.push(byte[0]);
            assert!(bytes.len() < HEADER_LIMIT);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc")
            .unwrap();
        stream.flush().unwrap();
        bytes
    }

    #[test]
    fn raw_bytes_reach_http_and_verified_https_servers() {
        for tls in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!(
                "{}://localhost:{}/announce?info_hash=%00%2f%2d",
                if tls { "https" } else { "http" },
                listener.local_addr().unwrap().port()
            );
            let rcgen::CertifiedKey { cert, key_pair } =
                rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert.der().clone()).unwrap();
            let config = Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            );
            let server_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
                )
                .unwrap();
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                if tls {
                    let conn = rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
                    capture_and_reply(&mut rustls::StreamOwned::new(conn, socket))
                } else {
                    capture_and_reply(&mut socket)
                }
            });
            let expected = request_bytes(ClientProfile::Transmission300, &url).unwrap();
            let response = get_with_config(ClientProfile::Transmission300, &url, config).unwrap();
            assert_eq!(response.body, b"abc");
            assert_eq!(worker.join().unwrap(), expected);
        }
    }

    #[test]
    fn untrusted_tls_certificate_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "https://localhost:{}/announce",
            listener.local_addr().unwrap().port()
        );
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
            )
            .unwrap();
        let worker = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut stream = rustls::StreamOwned::new(conn, socket);
            let _ = stream.read(&mut [0; 1]);
        });
        assert!(matches!(
            get(ClientProfile::Qbittorrent524, &url),
            Err(AnnounceError::Rejected(_))
        ));
        worker.join().unwrap();
    }
    #[test]
    fn content_length_and_chunked_finish_without_connection_close() {
        for bytes in [b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcEXTRA".as_slice(), b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n2\r\nbc\r\n0\r\n\r\nEXTRA"] {
            assert_eq!(read_response(&mut io::Cursor::new(bytes)).unwrap().body, b"abc");
        }
        assert!(
            read_response(&mut io::Cursor::new(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n"
            ))
            .is_err()
        );
    }
    #[test]
    fn compressed_bodies_and_retry_after_are_parsed() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"hello").unwrap();
        let body = encoder.finish().unwrap();
        let mut bytes = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend(body);
        assert_eq!(
            read_response(&mut io::Cursor::new(bytes)).unwrap().body,
            b"hello"
        );
        let reply = read_response(&mut io::Cursor::new(
            b"HTTP/1.1 429 Limit\r\nRetry-After: 120\r\n\r\n",
        ))
        .unwrap();
        assert_eq!((reply.status, reply.retry_after), (429, Some(120)));
    }
}
