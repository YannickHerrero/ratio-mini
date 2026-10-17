# Ratio Mini

Petit programme Rust pour Windows, avec interface graphique ou ligne de commande. Il lit un `.torrent` et déclare au tracker un compteur d'upload qui augmente à la vitesse choisie. Il ne transfère pas le contenu du torrent.

## Interface graphique

Double-cliquer sur **`ratio-mini-gui.exe`**. Aucune installation ni commande n'est nécessaire.

1. Dans le panneau de gauche, cliquer sur **Choisir un fichier .torrent…**.
2. Régler la vitesse moyenne, le volume maximal, les variations et la durée du démarrage progressif.
3. Cliquer sur **Ajouter et démarrer** : les annonces réseau commencent et la session apparaît à droite.
4. Recommencer pour ajouter d'autres torrents. Chaque session fonctionne en parallèle avec ses propres réglages ; les vitesses s'additionnent.

Le **Plafond d'upload** peut être défini en **Pourcentage du torrent** ou en **Volume fixe**. Le premier mode est proposé pour les nouveaux ajouts, à **300 %**, soit trois fois la taille totale du contenu : un torrent de 4 Gio donne un plafond de 12 Gio. Il ne s'agit pas de la taille du petit fichier `.torrent`, ni d'un ratio de compte calculé par le programme. Pour un torrent multifichier, toutes les longueurs sont additionnées. L'interface affiche la taille et le plafond après sélection, puis le plafond effectif dans la liste. Les anciennes sauvegardes conservent leurs plafonds fixes.

Le bouton **Valeurs par défaut** rétablit les réglages d'ajout : plafond à 300 %, vitesse moyenne 128 Kio/s par torrent, variations ±25 %, démarrage de 30 s, pause sans leecher activée, profil qBittorrent 5.2.4, handshake activé et port 6881. Il conserve le fichier sélectionné et les réglages des sessions existantes. Ces valeurs servent de point de départ ; elles ne constituent pas une validation de discrétion ou d'acceptation par un site. Les sauvegardes restent prioritaires au démarrage ; utiliser ce bouton pour retrouver le nouveau jeu de valeurs.

La vitesse est indépendante du plafond et s'applique à chaque torrent : dix sessions à 128 Kio/s représentent 1280 Kio/s, soit 1,25 Mio/s ou environ 10,49 Mbit/s déclarés. Le programme ne mesure pas la capacité réelle de la connexion. Un véritable client dépend aussi de la demande des autres utilisateurs ; les compteurs simulés de Ratio Mini ne sont pas une mesure d'upload réel.

Le menu **Profil du client** propose Ratio Mini, qBittorrent 5.2.4 / 5.2.3 / 5.1.4 / 4.6.7 et Transmission 2.94 / 3.00. Le profil choisi est conservé avec chaque torrent. Les anciennes sauvegardes utilisent Ratio Mini. Les six profils repris du dépôt RatioMaster.NET déterminent l'identifiant, la clé de session, l'encodage de la query string et les en-têtes HTTP dans leur ordre exact. Les identifiants restent stables pendant une session. Ces profils utilisent une connexion directe : ils ne reprennent pas la configuration proxy de reqwest. Ils ne reproduisent pas l'empreinte TLS, le système d'exploitation ni le comportement complet d'un client réel.

La case **Répondre au handshake TCP** active un répondeur entrant IPv4 sur le **Port partagé** choisi. Elle est cochée par défaut pour une nouvelle configuration ; les anciennes sauvegardes la conservent désactivée. Plusieurs torrents du même processus partagent ce port : le hash exact du torrent sélectionne l'identité à renvoyer. Le peer ID est identique à celui des annonces HTTP du profil choisi. L'interface affiche le port et le nombre de réponses. Le dernier arrêt libère l'écoute ; un port déjà occupé par une autre application fait échouer la session avant sa première annonce.

Le répondeur valide le protocole, la longueur et le hash, répond aux 68 octets du handshake, puis ferme la connexion. Il n'annonce aucune extension et ne transmet aucune pièce de fichier. Il accepte au maximum 32 échanges simultanés par port, avec un délai de lecture de cinq secondes ; les messages invalides ou les torrents inconnus sont refusés. Il ne modifie ni le routeur ni le pare-feu : la joignabilité depuis Internet dépend du réseau. Aucun port n'est ouvert en mode aperçu, ni lors de la restauration d'une liste.

La liste affiche le débit simulé, le volume calculé et le dernier volume accepté dans une réponse du tracker. Le journal contient les annonces et les erreurs. La barre progresse selon le volume simulé ; elle peut atteindre le plafond avant la prochaine annonce réseau. Une réponse acceptée ne garantit pas que le site créditera le ratio.

Les nombres de seeders et de leechers sont affichés quand le tracker les fournit (`?` sinon). Par défaut, le compteur d'upload est suspendu si une réponse annonce zéro leecher. Les annonces continuent à l'intervalle habituel pour recevoir de nouvelles statistiques ; la reprise n'ajoute pas de volume pour le temps passé en pause. Une réponse sans statistiques conserve les derniers chiffres connus ; elle ne transforme pas un zéro connu en reprise. La case **Pause si aucun leecher** permet de régler ce comportement pour chaque nouvel ajout. En ligne de commande, `--ignore-swarm` désactive cette pause.

**Arrêter** demande une annonce finale pour une session ; **Tout arrêter** agit sur toutes. **Démarrer** relance une session inactive avec ses réglages et de nouveaux compteurs. **Retirer** et **Nettoyer la liste** enlèvent uniquement des sessions inactives. Les doublons actifs sont reconnus par le hash du torrent, même si le fichier porte un autre nom.

Fermer la fenêtre demande d'abord l'arrêt des sessions. La fenêtre reste ouverte jusqu'à la fin des requêtes, dont le délai maximal est de 20 secondes chacune. Aucun tracker n'est contacté au simple démarrage de l'application.

La liste et les réglages sont sauvegardés automatiquement dans `ratio-mini-sessions.json`, à côté de l'exécutable, au plus tard deux secondes après une modification et à la fermeture. Le dossier doit être accessible en écriture. Le fichier contient les chemins vers les torrents et leurs paramètres ; il ne contient pas les URL de tracker, les passkeys ou les compteurs. La restauration n'ouvre pas les fichiers torrent et ne démarre aucune session. Les chemins doivent encore être valides au moment de cliquer sur **Démarrer**.

**Enregistrer…** exporte une copie JSON ; **Charger…** remplace la liste quand toutes les sessions sont arrêtées. L'écriture passe par un fichier temporaire puis un remplacement atomique. Un fichier invalide au démarrage est conservé et désactive la sauvegarde automatique pour cette exécution. La liste est limitée à 256 entrées ; un fichier JSON est limité à 1 Mio.

## Utilisation

Ouvrir PowerShell dans ce dossier. Vérifier les paramètres sans envoyer de requête :

```powershell
.\ratio-mini.exe "C:\chemin\fichier.torrent" --rate-kib 128 --variation-percent 25 --upload-percent 300
```

Démarrer les annonces :

```powershell
.\ratio-mini.exe "C:\chemin\fichier.torrent" --rate-kib 128 --variation-percent 25 --upload-percent 300 --send
```

- `--rate-kib` : vitesse moyenne visée en Kio/s, 128 par défaut.
- `--variation-percent` : amplitude aléatoire, 25 % par défaut. À 512 Kio/s et 25 %, le débit simulé varie entre 384 et 640 Kio/s. `0` donne un débit constant ; maximum : `100`.
- `--ramp-seconds` : démarrage progressif de zéro au débit simulé sur 30 secondes par défaut. `0` désactive cette montée ; maximum : 3600.
- `--upload-percent` : plafond en pourcentage de la taille du contenu, 300 % par défaut en l'absence de plafond fixe. 100 % = une fois la taille ; 300 % = trois fois. Valeurs entières de 1 à 100 000.
- `--upload-mib` : autre mode de plafond, fixe en Mio. Les deux options s'excluent mutuellement.
- `--port` : port déclaré et port d'écoute si le handshake est activé, 6881 par défaut.
- `--handshake` : active le répondeur TCP IPv4 ; désactivé par défaut en ligne de commande.
- `--send` : active les requêtes réseau. Sans cette option, seul un aperçu local est affiché.
- `--client-profile` : `native` (défaut), `qbittorrent524`, `qbittorrent523`, `qbittorrent514`, `qbittorrent467`, `transmission294` ou `transmission300`.
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

Le plafond proportionnel est recalculé à chaque démarrage d'une session à partir de son torrent. Le calcul utilise des entiers, arrondit au prochain octet si nécessaire et refuse les débordements ou un plafond nul. Le plafond reste un maximum : un arrêt manuel ou une pause peut produire un volume inférieur.

## Fonctionnement et limites

Le programme annonce `started` avec zéro octet, puis augmente `uploaded` selon le temps réellement écoulé. Il déclare `downloaded=0` et `left=0` : il se présente donc comme ayant le torrent complet. Chaque lancement crée une nouvelle session et les compteurs repartent de zéro. Un arrêt brutal ou une panne réseau peut empêcher l'annonce `stopped`.

Les erreurs réseau, les réponses HTTP 429 et les erreurs HTTP 5xx déclenchent au plus trois nouvelles tentatives, après 60, 120 puis 240 secondes. Une valeur `Retry-After` exprimée en secondes peut prolonger ce délai. Une attente demandée supérieure à 24 heures arrête la session. Les refus explicites, les autres erreurs HTTP et les réponses invalides arrêtent immédiatement la session. L'attente peut être interrompue par **Arrêter** ; la requête finale `stopped` n'est pas relancée afin de ne pas prolonger indéfiniment la fermeture.

Il utilise uniquement le champ `announce` du fichier : HTTP et HTTPS, torrents v1 ou hybrides. Pas d'UDP, de torrent v2 seul, de lien magnet ou de sélection parmi plusieurs trackers. Le profil natif utilise l'identité Ratio Mini ; les autres profils utilisent les valeurs définies dans leur catalogue. Aucun profil, débit ou variation ne garantit l'indétectabilité, l'acceptation des compteurs ou la hausse effective du ratio : cela dépend du tracker.

Le tracker reçoit l'IP utilisée pour la connexion, l'identifiant du torrent, le peer ID, le port et les compteurs. La passkey présente dans l'URL du `.torrent` lui est transmise. Le programme masque le chemin, la query string et le texte des erreurs du tracker dans la console. Il n'a ni télémétrie, ni mise à jour automatique, ni autre service distant. Les redirections HTTP sont refusées ; HTTPS vérifie les certificats. HTTP reste non chiffré. La configuration proxy reconnue par reqwest peut s'appliquer au profil natif ; les profils émulés utilisent le transport direct de `src/profile_transport.rs`. Celui-ci gère Content-Length, chunked, gzip et deflate, avec des limites sur les en-têtes et le corps de réponse.

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

Code examiné le 17 octobre 2026, au commit [`10d28c4`](https://github.com/NikolayIT/RatioMaster.NET/tree/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217). Cette comparaison décrit le code ; elle ne constitue pas un essai sur un tracker ni une preuve d'indétectabilité. Le tableau décrit les écarts relevés pendant la lecture initiale. Les statistiques du groupe, la pause sur zéro leecher, les nouvelles tentatives après erreur temporaire et la sauvegarde atomique des listes ont depuis été intégrées à notre moteur et à l'interface.

| Élément observé | Comparaison et intérêt |
| --- | --- |
| [Profils de clients](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Clients/ClientProfile.cs) | Le profil couvre HTTP, en-têtes, encodage et identité. Changer uniquement le User-Agent de notre client ne reproduirait pas cet ensemble. |
| [Gestion des sessions](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Sessions/TorrentSession.cs) | L'upload peut être suspendu lorsque le tracker indique zéro leecher. Le moteur prévoit des nouvelles tentatives et des conditions d'arrêt. Ratio Mini respecte déjà les intervalles mais s'arrête sur une erreur réseau et ne lit pas les statistiques du groupe. |
| [Variations de débit](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Sessions/TorrentSession.cs) | Leur jitter s'ajoute au débit de base ; il n'est donc pas forcément centré sur la moyenne demandée. Notre modèle centré, lissé et progressif correspond mieux à une moyenne visée, sans garantie sur la moyenne d'une session courte. |
| [Réponse aux connexions](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Networking/PeerListener.cs) | Le listener répond à un handshake puis ferme la connexion. Cela n'implémente pas le transfert de pièces et ne démontre pas un partage effectif. Ratio Mini n'ouvre aucun port. |
| [Lecture mémoire](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/MemoryScan/ClientValueScanner.cs) | Récupère des valeurs d'identité dans un client lancé. Cette capacité élargirait sensiblement les accès locaux de notre petite application ; elle n'est pas nécessaire à sa robustesse. |
| [Tests des requêtes](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core.Tests/Tracker/RequestGoldenTests.cs) | Les tests figent les octets attendus par leurs profils. C'est une bonne protection contre les régressions, mais pas une validation des contrôles d'un site réel. |
| [Sauvegarde des sessions](https://github.com/NikolayIT/RatioMaster.NET/blob/10d28c4ebb51b4ca4ef68136e777b8bf00e5d217/src/RatioMaster.Core/Sessions/SessionFile.cs) | Écriture temporaire puis remplacement du fichier. C'est une approche utile pour ajouter plus tard une persistance fiable de notre liste et de ses réglages. |

Les statistiques du tracker sont affichées en distinguant une valeur absente d'une valeur zéro. Elles ne constituent pas une mesure complète des transferts réels. Les variations restent centrées sur la moyenne visée. Les six profils qBittorrent et Transmission et le répondeur de handshake sont désormais intégrés ; la lecture mémoire d'autres logiciels n'est pas utilisée.

Les données `src/client_profiles.json`, les requêtes de référence de `tests/fixtures/upstream-requests.txt` et les générateurs adaptés dans `src/profiles.rs` proviennent de RatioMaster.NET, commit `10d28c4ebb51b4ca4ef68136e777b8bf00e5d217`. Leur licence MIT et le copyright de Nikolay Kostov sont conservés dans `third-party/RatioMaster.NET-LICENSE.txt`. Les tests comparent les octets des quatre événements de chaque profil aux références amont ; ils ne prouvent pas qu'un site réel accepte ces profils.
