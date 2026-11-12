//! rebord — hôte Windows pour le client iOS/iPadOS de Sill (protocole `sill/1`).

use rebord::input::CmdMode;
use rebord::log;
use rebord::platform::{self, Platform};
use rebord::server::{self, Config, DEFAULT_PORT};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::sync::Arc;

const HELP: &str = "\
rebord — affiche et pilote les fenêtres de ce PC depuis l'app Sill sur iPhone ou iPad.

Usage : rebord [options]

  --port N           port d'écoute TCP (défaut 48100)
  --name NOM         nom montré dans l'app (défaut : nom du PC)
  --bitrate MBPS     débit vidéo pour 60 i/s, en Mbit/s (défaut 20)
  --max-fps N        cadence maximale (défaut 120)
  --max-side N       plus grand côté encodé, en pixels (défaut 3840)
  --cmd ctrl|win     la touche ⌘ de l'iPad devient Ctrl (défaut) ou Windows
  --plain            porte en clair, sans TLS (seule une build Debug de l'app s'y connecte)
  --data-dir DOSSIER clé TLS de l'hôte (défaut : %LOCALAPPDATA%\\Rebord)
  --cursor-in-video  dessine le curseur dans la vidéo (visible aussi sous le doigt)
  --synthetic        diffuse une mire de test au lieu de l'écran (diagnostic)
  --mdns rebord|windows|off
                     annonce Bonjour par le répondeur de Rebord (défaut), par celui de Windows, ou aucune
  --no-advertise     comme --mdns off
  --log FICHIER      journal (défaut : %LOCALAPPDATA%\\Rebord\\rebord.log)

  --probe HÔTE:PORT [--plain] [--seconds S] [--out FICHIER]
                     se connecte à un hôte comme l'app et enregistre la vidéo reçue
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config = Config::default();
    let mut port = DEFAULT_PORT;
    let mut synthetic = false;
    let mut mdns_mode = "rebord".to_string();
    let mut plain = false;
    let mut data_dir: Option<std::path::PathBuf> = None;
    let mut name: Option<String> = None;
    let mut log_path: Option<String> = None;
    let mut probe: Option<String> = None;
    let mut seconds = 5.0;
    let mut out = "probe.hevc".to_string();

    let mut i = 0;
    let value = |i: &mut usize| -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_else(|| fail(&format!("valeur manquante après {}", args[*i - 1])))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--port" => port = value(&mut i).parse().unwrap_or_else(|_| fail("port invalide")),
            "--name" => name = Some(value(&mut i)),
            "--bitrate" => {
                let m: f64 = value(&mut i).parse().unwrap_or_else(|_| fail("débit invalide"));
                config.bitrate = (m.clamp(1.0, 200.0) * 1e6) as u32;
            }
            "--max-fps" => {
                config.max_fps =
                    value(&mut i).parse::<u32>().unwrap_or_else(|_| fail("cadence invalide")).clamp(24, 240)
            }
            "--max-side" => {
                config.max_side =
                    value(&mut i).parse::<u32>().unwrap_or_else(|_| fail("taille invalide")).clamp(320, 8192)
            }
            "--cmd" => {
                config.cmd_mode = match value(&mut i).as_str() {
                    "ctrl" => CmdMode::Ctrl,
                    "win" => CmdMode::Win,
                    _ => fail("--cmd attend ctrl ou win"),
                }
            }
            "--synthetic" => synthetic = true,
            "--cursor-in-video" => config.cursor_in_video = true,
            "--plain" => plain = true,
            "--data-dir" => data_dir = Some(value(&mut i).into()),
            "--no-advertise" => mdns_mode = "off".into(),
            "--mdns" => {
                mdns_mode = value(&mut i);
                if !["rebord", "windows", "off"].contains(&mdns_mode.as_str()) {
                    fail("--mdns attend rebord, windows ou off");
                }
            }
            "--log" => log_path = Some(value(&mut i)),
            "--probe" => probe = Some(value(&mut i)),
            "--seconds" => seconds = value(&mut i).parse().unwrap_or_else(|_| fail("durée invalide")),
            "--out" => out = value(&mut i),
            "-h" | "--help" | "/?" => {
                print!("{HELP}");
                return;
            }
            other => fail(&format!("option inconnue : {other}")),
        }
        i += 1;
    }

    if let Some(addr) = probe {
        if let Err(e) = rebord::probe::run_cli(&addr, plain, seconds, &out) {
            fail(&e);
        }
        return;
    }

    keep_console_responsive();
    if let Some(p) = log_path.map(std::path::PathBuf::from).or_else(default_log_path) {
        log::to_file(&p);
        log!("journal : {}", p.display());
    }
    log!("rebord {} — protocole sill/1, port {port}", env!("CARGO_PKG_VERSION"));

    let platform: Arc<dyn Platform> = if synthetic {
        log!("mode synthétique : diffusion d'une mire de test, entrées seulement notées");
        Arc::new(platform::synthetic::Synthetic::new())
    } else {
        native_platform()
    };
    config.name = name.unwrap_or_else(|| platform.host_name());

    if plain {
        log!("porte en clair : seule une build Debug de l'app s'y connecte");
    } else {
        let dir = data_dir.unwrap_or_else(rebord::tls::data_dir);
        let id = rebord::tls::Identity::load_or_create(&dir, &config.name)
            .unwrap_or_else(|e| fail(&format!("clé TLS : {e}")));
        let tls = rebord::tls::server_config(&id).unwrap_or_else(|e| fail(&format!("TLS : {e}")));
        log!(
            "porte TLS ouverte (p=0) : tout appareil du réseau peut se connecter, sans appairage ; clé {}…",
            id.short()
        );
        config.door = server::Door::Open(tls);
    }

    let mut listeners = Vec::new();
    for addr in [SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)), SocketAddr::from((Ipv6Addr::UNSPECIFIED, port))] {
        match TcpListener::bind(addr) {
            Ok(l) => {
                log!("écoute sur {addr}");
                listeners.push(l);
            }
            Err(e) => log!("pas d'écoute sur {addr} : {e}"),
        }
    }
    if listeners.is_empty() {
        fail(&format!("impossible d'écouter sur le port {port}"));
    }

    let _announce: Option<Box<dyn Send>> = match mdns_mode.as_str() {
        "rebord" => {
            let mut service = rebord::mdns::Service::new(&config.name, &rebord::mdns::computer_name(), port);
            if !plain {
                service.txt = vec!["p=0".into()];
            }
            match rebord::mdns::start(service) {
                Ok(r) => {
                    on_exit_say_goodbye();
                    Some(Box::new(r))
                }
                Err(e) => {
                    log!("répondeur mDNS de Rebord indisponible ({e}) : annonce par Windows");
                    system_advertise(&config.name, port, !plain)
                }
            }
        }
        "windows" => system_advertise(&config.name, port, !plain),
        _ => None,
    };

    server::run(platform, config, listeners);
}

fn system_advertise(name: &str, port: u16, tls: bool) -> Option<Box<dyn Send>> {
    match platform::advertise(name, port, tls) {
        Ok(a) => {
            log!("annoncé par Windows comme « {name} » (_sill._tcp)");
            Some(a)
        }
        Err(e) => {
            log!("pas d'annonce Bonjour : {e}");
            None
        }
    }
}

/// Sans « Édition rapide » : une sélection dans la console y bloque toute écriture, donc le
/// journal, donc Rebord entier (vu le 2026-11-12 : l'app se figeait le temps d'un copier).
#[cfg(windows)]
fn keep_console_responsive() {
    use windows::Win32::System::Console::*;
    unsafe {
        if let Ok(h) = GetStdHandle(STD_INPUT_HANDLE) {
            let mut mode = CONSOLE_MODE(0);
            if GetConsoleMode(h, &mut mode).is_ok() {
                let _ = SetConsoleMode(h, CONSOLE_MODE((mode.0 & !ENABLE_QUICK_EDIT_MODE.0) | ENABLE_EXTENDED_FLAGS.0));
            }
        }
    }
}

#[cfg(not(windows))]
fn keep_console_responsive() {}

/// Fermer la console ou Ctrl+C : l'au revoir mDNS part, pour que l'app retire le PC tout de suite.
#[cfg(windows)]
fn on_exit_say_goodbye() {
    use windows::Win32::System::Console::SetConsoleCtrlHandler;
    use windows::core::BOOL;
    unsafe extern "system" fn handler(_kind: u32) -> BOOL {
        rebord::mdns::goodbye_all();
        false.into() // le traitement habituel suit : le programme s'arrête
    }
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handler), true);
    }
}

#[cfg(not(windows))]
fn on_exit_say_goodbye() {}

#[cfg(windows)]
fn native_platform() -> Arc<dyn Platform> {
    match platform::windows::WindowsPlatform::new() {
        Ok(p) => Arc::new(p),
        Err(e) => fail(&format!("initialisation Windows : {e}")),
    }
}

#[cfg(not(windows))]
fn native_platform() -> Arc<dyn Platform> {
    log!("pas de plateforme native hors de Windows : mode synthétique");
    Arc::new(platform::synthetic::Synthetic::new())
}

fn default_log_path() -> Option<std::path::PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("Rebord").join("rebord.log"))
}

fn fail(msg: &str) -> ! {
    eprintln!("rebord : {msg}");
    std::process::exit(2)
}
