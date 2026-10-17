# Ratio Mini

Petit programme Rust pour Windows, avec interface graphique ou ligne de commande. Il lit un `.torrent` et déclare au tracker un compteur d'upload qui augmente à la vitesse choisie. Il ne transfère pas le contenu du torrent.

## Interface graphique

Double-cliquer sur **`ratio-mini-gui.exe`**. Aucune installation ni commande n'est nécessaire.

1. Dans le panneau de gauche, cliquer sur **Choisir un fichier .torrent…**.
2. Régler la vitesse moyenne, le volume maximal, les variations et la durée du démarrage progressif.
3. Cliquer sur **Ajouter et démarrer** : les annonces réseau commencent et la session apparaît à droite.
4. Recommencer pour ajouter d'autres torrents. Chaque session fonctionne en parallèle avec ses propres réglages ; les vitesses s'additionnent.

La liste affiche le débit simulé, le volume calculé et le dernier volume accepté dans une réponse du tracker. Le journal contient les annonces et les erreurs. La barre progresse selon le volume simulé ; elle peut atteindre le plafond avant la prochaine annonce réseau. Une réponse acceptée ne garantit pas que le site créditera le ratio.

Les nombres de seeders et de leechers sont affichés quand le tracker les fournit (`?` sinon). Par défaut, le compteur d'upload est suspendu si une réponse annonce zéro leecher. Les annonces continuent à l'intervalle habituel pour recevoir de nouvelles statistiques ; la reprise n'ajoute pas de volume pour le temps passé en pause. La case **Pause si aucun leecher** permet de régler ce comportement pour chaque nouvel ajout. En ligne de commande, `--ignore-swarm` désactive cette pause.

**Arrêter** demande une annonce finale pour une session ; **Tout arrêter** agit sur toutes. **Retirer** et **Nettoyer la liste** enlèvent uniquement des sessions inactives. Les doublons actifs sont reconnus par le hash du torrent, même si le fichier porte un autre nom. Après un arrêt ou une erreur, un torrent peut être ajouté à nouveau.

Fermer la fenêtre demande d'abord l'arrêt des sessions. La fenêtre reste ouverte jusqu'à la fin des requêtes, dont le délai maximal est de 20 secondes chacune. La liste et les réglages ne sont pas conservés après fermeture ; les torrents ne sont pas relancés automatiquement. Aucun tracker n'est contacté au simple démarrage de l'application.

## Utilisation

Ouvrir PowerShell dans ce dossier. Vérifier les paramètres sans envoyer de requête :

```powershell
.\ratio-mini.exe "C:\chemin\fichier.torrent" --rate-kib 512 --variation-percent 25 --upload-mib 1024
```

Démarrer les annonces :

```powershell
.\ratio-mini.exe "C:\chemin\fichier.torrent" --rate-kib 512 --variation-percent 25 --upload-mib 1024 --send
```

- `--rate-kib` : vitesse moyenne visée en Kio/s, 128 par défaut.
- `--variation-percent` : amplitude aléatoire, 25 % par défaut. À 512 Kio/s et 25 %, le débit simulé varie entre 384 et 640 Kio/s. `0` donne un débit constant ; maximum : `100`.
- `--ramp-seconds` : démarrage progressif de zéro au débit simulé sur 30 secondes par défaut. `0` désactive cette montée ; maximum : 3600.
- `--upload-mib` : plafond pour cette session en Mio, 1024 par défaut.
- `--port` : port déclaré, 6881 par défaut. Le programme n'ouvre pas ce port.
- `--send` : active les requêtes réseau. Sans cette option, seul un aperçu local est affiché.
- `--help` : affiche les options.
- **Ctrl+C** : demande une dernière annonce `stopped`, puis quitte. Une requête en cours peut prendre jusqu'à 20 secondes avant de se terminer.

| Vitesse | Volume déclaré par heure, environ |
| --- | --- |
| 128 Kio/s | 450 Mio |
| 256 Kio/s | 900 Mio |
| 512 Kio/s | 1,76 Gio |
| 1024 Kio/s | 3,52 Gio |

Une cible de débit est tirée uniformément autour de la valeur choisie environ toutes les 5 secondes. Le débit est lissé en conservant 75 % du débit précédent et en ajoutant 25 % de la nouvelle cible. Au départ, une rampe linéaire limite progressivement le débit. Le volume est intégré au fil du temps ; la moyenne réelle sur une session peut différer de la valeur visée, surtout pour une session courte. Pendant une requête réseau, le dernier débit reste en vigueur et la rampe continue.

Les compteurs sont transmis à chaque annonce, pas à chaque seconde. L'intervalle demandé par le tracker est respecté, avec un minimum de 30 secondes. À 128 Kio/s en moyenne, atteindre 1024 Mio prend environ 2 h 17 min, puis le programme attend la prochaine annonce pour transmettre le total final. Ctrl+C peut arrêter plus tôt.

## Fonctionnement et limites

Le programme annonce `started` avec zéro octet, puis augmente `uploaded` selon le temps réellement écoulé. Il déclare `downloaded=0` et `left=0` : il se présente donc comme ayant le torrent complet. Chaque lancement crée une nouvelle session ; aucun état n'est sauvegardé et les compteurs repartent de zéro. En cas d'erreur, il s'arrête sans réessayer automatiquement ; un arrêt brutal ou une panne réseau peut empêcher l'annonce `stopped`.

Il utilise uniquement le champ `announce` du fichier : HTTP et HTTPS, torrents v1 ou hybrides. Pas d'UDP, de torrent v2 seul, de lien magnet, de sélection parmi plusieurs trackers, ni d'imitation d'un autre client. Son identité est `ratio-mini/0.1.0` avec un peer ID propre. Aucun débit ni variation ne garantit l'indétectabilité, l'acceptation des compteurs ou la hausse effective du ratio : cela dépend du tracker.

Le tracker reçoit l'IP utilisée pour la connexion, l'identifiant du torrent, le peer ID, le port et les compteurs. La passkey présente dans l'URL du `.torrent` lui est transmise. Le programme masque le chemin, la query string et le texte des erreurs du tracker dans la console. Il n'a ni télémétrie, ni mise à jour automatique, ni autre service distant. Les redirections HTTP sont refusées ; HTTPS vérifie les certificats. HTTP reste non chiffré. La configuration proxy reconnue par reqwest peut s'appliquer aux connexions.

## Sources et compilation

Le moteur partagé est dans `src/lib.rs`, la commande dans `src/main.rs`, et l'interface egui dans `src/bin/gui.rs`. Les dépendances sont dans `Cargo.toml` et leurs versions résolues dans `Cargo.lock`. Le parseur bencode est intégré ; le SHA-1 est calculé sur les octets originaux du dictionnaire `info`.

Avec Rust et les outils de compilation Windows installés :

```powershell
cargo test --locked --features gui
cargo clippy --locked --all-targets --features gui -- -D warnings
cargo build --release --locked --features gui --bins
```

Les exécutables compilés se trouvent dans `target\release\ratio-mini.exe` et `target\release\ratio-mini-gui.exe`. Ceux livrés à la racine sont des copies de cette compilation. Les tests vérifient notamment deux sessions concurrentes et leur arrêt indépendant avec un faux tracker local, ainsi que les états de l'interface. Aucune validation sur ton compte ou ton tracker n'a été effectuée.

Référence du protocole : [spécification officielle BitTorrent, BEP 3](https://www.bittorrent.org/beps/bep_0003.html).

## Lecture comparative de RatioMaster.NET

Code examiné le 17 octobre 2026, au commit [`10d28c4`](https://github.com/NikolayIT/RatioMaster.NET/tree/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217). Cette comparaison décrit le code ; elle ne constitue pas un essai sur un tracker ni une preuve d'indétectabilité. Le tableau décrit les écarts relevés pendant la lecture initiale. Les statistiques du groupe et la pause sur zéro leecher ont depuis été intégrées à notre moteur et à l'interface.

| Élément observé | Comparaison et intérêt |
| --- | --- |
| [Profils de clients](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Clients/ClientProfile.cs) | Le profil couvre HTTP, en-têtes, encodage et identité. Changer uniquement le User-Agent de notre client ne reproduirait pas cet ensemble. |
| [Gestion des sessions](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Sessions/TorrentSession.cs) | L'upload peut être suspendu lorsque le tracker indique zéro leecher. Le moteur prévoit des nouvelles tentatives et des conditions d'arrêt. Ratio Mini respecte déjà les intervalles mais s'arrête sur une erreur réseau et ne lit pas les statistiques du groupe. |
| [Variations de débit](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Sessions/TorrentSession.cs) | Leur jitter s'ajoute au débit de base ; il n'est donc pas forcément centré sur la moyenne demandée. Notre modèle centré, lissé et progressif correspond mieux à une moyenne visée, sans garantie sur la moyenne d'une session courte. |
| [Réponse aux connexions](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Networking/PeerListener.cs) | Le listener répond à un handshake puis ferme la connexion. Cela n'implémente pas le transfert de pièces et ne démontre pas un partage effectif. Ratio Mini n'ouvre aucun port. |
| [Lecture mémoire](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/MemoryScan/ClientValueScanner.cs) | Récupère des valeurs d'identité dans un client lancé. Cette capacité élargirait sensiblement les accès locaux de notre petite application ; elle n'est pas nécessaire à sa robustesse. |
| [Tests des requêtes](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core.Tests/Tracker/RequestGoldenTests.cs) | Les tests figent les octets attendus par leurs profils. C'est une bonne protection contre les régressions, mais pas une validation des contrôles d'un site réel. |
| [Sauvegarde des sessions](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Sessions/SessionFile.cs) | Écriture temporaire puis remplacement du fichier. C'est une approche utile pour ajouter plus tard une persistance fiable de notre liste et de ses réglages. |

Les améliorations de fiabilité à prioriser seraient la sauvegarde de la liste sans relance automatique, la modification des réglages d'une session et une gestion explicite des erreurs temporaires. Les statistiques du tracker peuvent aussi être affichées, en distinguant une valeur absente d'une valeur zéro. Elles ne constituent pas une mesure complète des transferts réels.

Le dépôt amont est sous licence MIT ; toute reprise substantielle de son code doit conserver ses mentions de licence. La présente version utilise une implémentation Rust propre et n'incorpore pas de code provenant de ce dépôt.
