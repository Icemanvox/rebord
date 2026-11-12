# Rebord

Hôte Windows pour l'app iPhone/iPad de [Sill](https://github.com/Saffsanity/sill) : l'app affiche une fenêtre du PC
(ou l'écran entier) et la pilote au doigt, au trackpad, au clavier ou à l'Apple Pencil. Rebord parle le protocole
`sill/1` de Sill octet pour octet ; l'app, inchangée, voit le PC comme un Mac.

« Rebord » (de fenêtre) est la traduction de *sill* ; un autre nom que Sill, que la licence Apache 2.0 ne couvre pas.

## État : jalon 1, plus la porte TLS

| Fonction | État |
| --- | --- |
| Annonce Bonjour `_sill._tcp` par un répondeur mDNS intégré | fait |
| Porte TLS 1.3 ouverte (`p=0`) : l'app publiée s'y connecte sans appairage | fait |
| Catalogue des fenêtres (comme Alt+Tab), ordre stable | fait |
| Capture d'une fenêtre ou de l'écran principal (Windows.Graphics.Capture) | fait |
| Encodage HEVC matériel (Media Foundation), images clés à la demande | fait |
| Souris, clic droit, molette, clavier, texte, ⌘ → Ctrl, gestes | fait |
| Forme du curseur et position quand la souris du PC bouge (dessinés par l'app) | fait |
| Fermer, réduire, agrandir une fenêtre depuis l'app | fait |
| Appairage (porte réservée aux appareils appairés), presse-papiers, menus, son, accès à distance | jalons suivants |

Ce qui est vérifié ici, sans iPad ni PC Windows : le protocole (trame, JSON, HEVC) contre les vecteurs de test de Sill,
une session complète contre une mire de test, la même session avec l'exécutable Windows sous Wine, et la vidéo reçue
décodée par ffmpeg. **La capture, l'encodeur matériel et l'injection d'entrées n'ont pas encore tourné sur un vrai
Windows** : le premier essai dira s'il reste des réglages à faire. Le journal est fait pour ça.

## Prérequis

- **PC** : Windows 11 (Windows 10 21H2 devrait suffire), GPU avec encodeur HEVC matériel : Intel 6ᵉ génération, NVIDIA
  GTX 950 ou AMD RX 400, ou plus récent.
- **Réseau** : le PC et l'appareil sur le même réseau, profil réseau **Privé** sur le PC.
- **App** : l'app Sill pour iPhone/iPad, telle qu'elle est publiée. Une build Debug compilée soi-même marche aussi.

## Installation sur le PC

1. Copier `rebord.exe` où l'on veut, par exemple `C:\Program Files\Rebord\`.
2. Autoriser le programme dans le pare-feu (PowerShell administrateur). La règle couvre le port TCP 48100 de la session et
   le port UDP 5353 de l'annonce Bonjour :

   ```powershell
   New-NetFirewallRule -DisplayName "Rebord" -Direction Inbound -Program "C:\Program Files\Rebord\rebord.exe" -Action Allow -Profile Private
   Get-NetConnectionProfile   # NetworkCategory doit valoir Private ; sinon :
   Set-NetConnectionProfile -InterfaceAlias "Ethernet" -NetworkCategory Private
   ```

3. Lancer `rebord.exe` dans la session de l'utilisateur. Pour copier le journal, ouvrir le fichier plutôt que de
   sélectionner dans la console (pas en service : la capture se fait dans la session ouverte).
   La console montre le journal, aussi écrit dans `%LOCALAPPDATA%\Rebord\rebord.log`.

## Sécurité de la porte

Par défaut, Rebord ouvre une **porte TLS ouverte**, comme Sill sur Mac avec *Require pairing* décoché. Le trafic est
chiffré (TLS 1.3, clé P-256 de l'hôte gardée dans `%LOCALAPPDATA%\Rebord\identity-key.pem`), mais **tout appareil du
réseau local qui a l'app Sill peut se connecter et piloter le PC**, sans code. À réserver à un réseau de confiance,
jusqu'à l'arrivée de l'appairage.

`--plain` rouvre l'ancienne porte en clair, sans chiffrement, pour une build Debug de l'app.

## Options

```
rebord [options]
  --port N           port d'écoute TCP (défaut 48100)
  --name NOM         nom montré dans l'app (défaut : nom du PC)
  --bitrate MBPS     débit vidéo pour 60 i/s, en Mbit/s (défaut 20)
  --max-fps N        cadence maximale (défaut 120 ; l'app demande 120 sur un écran ProMotion)
  --max-side N       plus grand côté encodé, en pixels (défaut 3840)
  --cmd ctrl|win     la touche ⌘ de l'iPad devient Ctrl (défaut) ou Windows
  --cursor-in-video  dessine le curseur dans la vidéo, visible même sous le doigt
  --plain            porte en clair, sans TLS (seule une build Debug de l'app s'y connecte)
  --data-dir DOSSIER emplacement de la clé TLS de l'hôte (défaut %LOCALAPPDATA%\\Rebord)
  --synthetic        diffuse une mire de test au lieu de l'écran
  --mdns rebord|windows|off
                     annonce Bonjour par le répondeur de Rebord (défaut), par celui de Windows, ou aucune
  --log FICHIER      autre emplacement du journal
  --probe HÔTE:PORT [--plain] [--seconds S] [--out FICHIER]
                     se connecte à un hôte comme l'app et enregistre la vidéo reçue
```

### Le pointeur sur l'iPad

Comme avec Sill sur Mac, le curseur n'est pas dans la vidéo : l'app le dessine elle-même, avec la forme envoyée par
le PC (flèche, I, main…). Elle le montre quand la souris du PC le déplace, ou avec le trackpad à l'écran en mode
portrait. **Sous le doigt, l'app ne dessine pas de pointeur** : le doigt est le pointeur. Pour voir le curseur
Windows en toutes circonstances, lancer Rebord avec `--cursor-in-video`.

### Clavier et gestes

| Sur l'iPad | Sous Windows |
| --- | --- |
| ⌘C, ⌘V, ⌘Z… | Ctrl+C, Ctrl+V, Ctrl+Z… (`--cmd win` pour la touche Windows) |
| ⌘Espace (Spotlight) | menu Démarrer |
| ⌥ | Alt |
| Glisser à 3 doigts vers le haut ou le bas | Win+Tab (vue des tâches) |
| Glisser à 3 doigts à gauche / à droite | bureau virtuel suivant / précédent |
| Pincer / écarter | menu Démarrer / afficher le bureau |

## En cas de problème

Commencer par le journal. Puis, dans l'ordre :

1. **L'app ne voit pas le PC** : au lancement de l'app, le journal doit montrer `mDNS : question de 192.168.x.y`
   (l'adresse de l'iPad). Si rien ne vient, la question est bloquée en route : règle de pare-feu par programme absente,
   profil réseau Public, ou Wi-Fi « invité » isolé du reste du réseau. Si la question arrive mais que l'app ne montre
   rien, envoyer le journal. `rebord --probe 127.0.0.1:48100` sur le PC lui-même vérifie la session.
2. **L'app se connecte mais l'image ne vient pas** : lancer `rebord --synthetic`. Si l'app affiche la mire « Rebord n »,
   la liaison est bonne et le souci vient de la capture ou de l'encodeur ; leurs messages sont dans le journal.
3. **Depuis un autre PC** : `rebord --probe IP-DU-PC:48100 --out test.hevc` enregistre la vidéo reçue, lisible par VLC.
4. **Les clics n'arrivent pas dans une fenêtre** : si elle tourne en administrateur, Windows refuse les entrées d'un
   programme qui ne l'est pas (UIPI). Le journal le signale.

## Limites connues du jalon 1

- Porte ouverte : chiffrée, mais sans appairage ; tout appareil du réseau local avec l'app peut se connecter.
- Écran principal seulement pour « Bureau » ; pas d'écran verrouillé, d'UAC ni d'écran de connexion.
- Une fenêtre réduite ne se capture pas ; la sélectionner la restaure.
- Pas de réglages depuis l'app, de presse-papiers, de menus ni de son.
- Stylet traité comme une souris, sans pression.

## Compiler Rebord

- **Sous Windows** : installer Rust (`rustup`, chaîne MSVC), puis `cargo build --release`. Résultat :
  `target\release\rebord.exe`.
- **Depuis Linux** : `tools/build-windows.sh` (MinGW, avec les sources de la bibliothèque standard).
- **Tests** : `cargo test`. Ils tournent sur toute plateforme grâce à la mire de test.

## Organisation du code

| Chemin | Rôle |
| --- | --- |
| `src/proto/` | le protocole : trame de 14 octets, messages JSON au format de Swift, conversion HEVC Annex B |
| `src/tls.rs` | la porte TLS : identité P-256, TLS 1.3 seul, certificat d'appareil exigé, ALPN `sill/1` |
| `src/mdns.rs` | le répondeur Bonjour intégré |
| `src/server.rs` | la session : catalogue, source, tick toutes les 30 ms, images clés, abandon des images en retard |
| `src/input.rs` | entrées de l'app → actions Windows (coordonnées, molette, HID → code de balayage, ⌘ → Ctrl) |
| `src/platform/windows/` | Bonjour, fenêtres, capture, conversion NV12, encodeur HEVC, SendInput |
| `src/platform/synthetic.rs` | mire de test et entrées notées, pour les tests et le diagnostic |
| `src/probe.rs` | client minimal qui parle comme l'app |
| `tests/session.rs` | sessions complètes contre la mire |

## Licence et origine

Le protocole est celui de Sill (© 2026 Noah Saffer, Apache 2.0), dont les fichiers `Sources/StreamProtocol/*.swift` ont
servi de spécification. Le code de Rebord est écrit de zéro en Rust.
