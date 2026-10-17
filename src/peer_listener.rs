//! Bounded IPv4 handshake responder inspired by RatioMaster.NET PeerListener.
//! A port is shared across sessions; the exact info hash selects the peer identity.
use crate::Result;
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Route {
    peer: [u8; 20],
    answered: Arc<AtomicU64>,
}
type Routes = Arc<Mutex<HashMap<[u8; 20], Route>>>;
struct Hub {
    routes: Routes,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    address: SocketAddr,
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct Registration {
    hub: Arc<Hub>,
    hash: [u8; 20],
    answered: Arc<AtomicU64>,
}
impl Registration {
    pub fn port(&self) -> u16 {
        self.hub.address.port()
    }
    pub fn answered(&self) -> u64 {
        self.answered.load(Ordering::Relaxed)
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.hub.routes.lock() {
            routes.remove(&self.hash);
        }
    }
}

pub fn register(port: u16, hash: [u8; 20], peer: [u8; 20]) -> Result<Registration> {
    register_on(
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)),
        hash,
        peer,
    )
}

fn register_on(address: SocketAddr, hash: [u8; 20], peer: [u8; 20]) -> Result<Registration> {
    static HUBS: OnceLock<Mutex<HashMap<SocketAddr, Weak<Hub>>>> = OnceLock::new();
    let mut hubs = HUBS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| "Registre TCP indisponible")?;
    hubs.retain(|_, hub| hub.strong_count() > 0);
    let hub = if let Some(hub) = hubs.get(&address).and_then(Weak::upgrade) {
        hub
    } else {
        let listener = TcpListener::bind(address).map_err(|_| "Port de handshake indisponible : choisir un autre port ou arrêter l'application qui l'utilise.")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "Configuration de l'écoute TCP impossible")?;
        let address = listener
            .local_addr()
            .map_err(|_| "Lecture du port TCP impossible")?;
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_routes = routes.clone();
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("peer-listener".into())
            .spawn(move || {
                accept_loop(listener, worker_routes, worker_stop);
            })
            .map_err(|_| "Démarrage de l'écoute TCP impossible")?;
        let hub = Arc::new(Hub {
            routes,
            stop,
            worker: Some(worker),
            address,
        });
        hubs.insert(address, Arc::downgrade(&hub));
        hub
    };
    let answered = Arc::new(AtomicU64::new(0));
    {
        let mut routes = hub.routes.lock().map_err(|_| "Routage TCP indisponible")?;
        if routes.contains_key(&hash) {
            return Err("Ce torrent possède déjà un handshake sur ce port.".into());
        }
        routes.insert(
            hash,
            Route {
                peer,
                answered: answered.clone(),
            },
        );
    }
    Ok(Registration {
        hub,
        hash,
        answered,
    })
}

fn accept_loop(listener: TcpListener, routes: Routes, stop: Arc<AtomicBool>) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        workers.retain(|worker| !worker.is_finished());
        match listener.accept() {
            Ok((socket, _)) if workers.len() < 32 => {
                let routes = routes.clone();
                let stop = stop.clone();
                if let Ok(worker) =
                    thread::Builder::new()
                        .name("peer-handshake".into())
                        .spawn(move || {
                            handle(socket, routes, &stop);
                        })
                {
                    workers.push(worker);
                }
            }
            Ok((socket, _)) => drop(socket),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(_) => break,
        }
    }
    stop.store(true, Ordering::SeqCst);
    for worker in workers {
        let _ = worker.join();
    }
}

fn handle(mut socket: TcpStream, routes: Routes, stop: &AtomicBool) {
    if socket
        .set_read_timeout(Some(Duration::from_millis(150)))
        .is_err()
        || socket
            .set_write_timeout(Some(Duration::from_millis(500)))
            .is_err()
    {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut incoming = [0; 68];
    let mut read = 0;
    while read < incoming.len() {
        if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
            return;
        }
        match socket.read(&mut incoming[read..]) {
            Ok(0) => return,
            Ok(n) => read += n,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(_) => return,
        }
    }
    if incoming[0] != 19 || &incoming[1..20] != b"BitTorrent protocol" {
        return;
    }
    let hash: [u8; 20] = incoming[28..48].try_into().expect("fixed handshake size");
    let route = routes
        .lock()
        .ok()
        .and_then(|routes| routes.get(&hash).cloned());
    let Some(route) = route else {
        return;
    };
    let mut outgoing = [0; 68];
    outgoing[0] = 19;
    outgoing[1..20].copy_from_slice(b"BitTorrent protocol");
    // No extension, DHT or encryption capabilities are advertised by this base responder.
    outgoing[28..48].copy_from_slice(&hash);
    outgoing[48..68].copy_from_slice(&route.peer);
    if socket.write_all(&outgoing).is_ok() {
        route.answered.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn packet(hash: [u8; 20]) -> [u8; 68] {
        let mut bytes = [0; 68];
        bytes[0] = 19;
        bytes[1..20].copy_from_slice(b"BitTorrent protocol");
        bytes[28..48].copy_from_slice(&hash);
        bytes[48..].copy_from_slice(&[9; 20]);
        bytes
    }
    fn connect(address: SocketAddr, packet: &[u8; 68]) -> TcpStream {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream.write_all(&packet[..17]).unwrap();
        stream.write_all(&packet[17..]).unwrap();
        stream
    }
    #[test]
    fn shared_port_routes_exact_hashes_and_releases_each_session() {
        let first = register_on("127.0.0.1:0".parse().unwrap(), [1; 20], [11; 20]).unwrap();
        let address = first.hub.address;
        let second = register_on(address, [2; 20], [22; 20]).unwrap();
        assert_eq!(first.port(), second.port());
        for (hash, peer) in [([1; 20], [11; 20]), ([2; 20], [22; 20])] {
            let mut stream = connect(address, &packet(hash));
            let mut reply = [0; 68];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply[28..48], &hash);
            assert_eq!(&reply[48..68], &peer);
            assert_eq!(&reply[20..28], &[0; 8]);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while (first.answered(), second.answered()) != (1, 1) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!((first.answered(), second.answered()), (1, 1));
        drop(first);
        assert_eq!(
            connect(address, &packet([1; 20]))
                .read(&mut [0; 1])
                .unwrap(),
            0
        );
        let mut valid = [0; 68];
        connect(address, &packet([2; 20]))
            .read_exact(&mut valid)
            .unwrap();
        drop(second);
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err());
    }
    #[test]
    fn invalid_messages_and_unknown_hashes_are_rejected() {
        let entry = register_on("127.0.0.1:0".parse().unwrap(), [1; 20], [11; 20]).unwrap();
        let address = entry.hub.address;
        let mut invalid = packet([1; 20]);
        invalid[0] = 18;
        for message in [invalid, packet([3; 20])] {
            assert_eq!(connect(address, &message).read(&mut [0; 1]).unwrap(), 0);
        }
        assert_eq!(entry.answered(), 0);
        assert!(register_on(address, [1; 20], [33; 20]).is_err());
    }
    #[test]
    fn shutdown_interrupts_incomplete_handshakes_and_busy_ports_fail() {
        let busy = TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(register_on(busy.local_addr().unwrap(), [1; 20], [2; 20]).is_err());
        let entry = register_on("127.0.0.1:0".parse().unwrap(), [1; 20], [2; 20]).unwrap();
        let mut socket = TcpStream::connect(entry.hub.address).unwrap();
        socket.write_all(&[19]).unwrap();
        thread::sleep(Duration::from_millis(40));
        let now = Instant::now();
        drop(entry);
        assert!(now.elapsed() < Duration::from_secs(2));
    }
}
