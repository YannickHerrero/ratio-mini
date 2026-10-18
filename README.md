# Ratio Mini

Petit outil Windows écrit en Rust pour simuler des compteurs d’upload auprès de trackers BitTorrent, avec une interface graphique et une commande CLI. Il ne transfère pas les fichiers du torrent.

- Plusieurs sessions, débit réglable et plafond de volume.
- Liste sauvegardée et interface compacte avec détails repliables.
- Affichage Direct3D 12 et mode logiciel de secours.

## Compiler

```sh
cargo build --release --locked --features gui --bins
```

Lancer `target/release/ratio-mini-gui.exe` pour l’interface, ou `ratio-mini --help` pour la CLI. Les fichiers `.torrent` doivent être conservés pour pouvoir relancer les sessions.

Voir le [guide](docs/GUIDE.md) pour les réglages et les tests. Les éléments issus de RatioMaster.NET conservent leur [licence MIT](third-party/RatioMaster.NET-LICENSE.txt). Aucune garantie d’indétectabilité ou d’acceptation par un tracker.
