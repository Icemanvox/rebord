//! Le fil d'encodage : il prend la dernière image capturée quand l'encodeur en demande une,
//! la convertit en NV12, l'encode et remet chaque unité d'accès au serveur.

use super::capture::{Capture, Gpu, Mailbox};
use super::convert::Converter;
use super::encoder::{Encoder, EncoderEvent};
use crate::log;
use crate::platform::{FrameSink, StreamConfig, StreamEvent, VideoStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

pub struct Shared {
    pub stop: AtomicBool,
    pub keyframe: AtomicBool,
    pub fps: AtomicU32,
}

pub struct WinStream {
    pub shared: Arc<Shared>,
    pub mailbox: Mailbox,
    pub capture: Option<Capture>,
    pub thread: Option<JoinHandle<()>>,
}

impl VideoStream for WinStream {
    fn request_keyframe(&self) {
        self.shared.keyframe.store(true, Ordering::SeqCst);
        self.mailbox.1.notify_all();
    }
    fn set_fps(&self, fps: u32) {
        self.shared.fps.store(fps.max(1), Ordering::SeqCst);
    }
}

impl Drop for WinStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        {
            let mut l = self.mailbox.0.lock().unwrap();
            l.closed = true;
            self.mailbox.1.notify_all();
        }
        self.capture.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// La taille encodée : celle de la source, réduite pour tenir dans `max_side`, en nombres pairs.
pub fn encoded_size(w: u32, h: u32, max_side: u32) -> (u32, u32) {
    let scale = (max_side as f64 / w.max(h).max(1) as f64).min(1.0);
    let even = |x: f64| ((x.floor() as u32) & !1).max(64);
    (even(w as f64 * scale), even(h as f64 * scale))
}

/// Délai sans changement de taille avant de recréer l'encodeur (un redimensionnement à la souris
/// en envoie des dizaines).
const SETTLE: Duration = Duration::from_millis(300);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Au plus autant de répétitions de la même image pour faire sortir la dernière vraie.
const MAX_REPEATS: u32 = 8;

/// Faut-il redonner la dernière image à l'encodeur ? Un encodeur matériel (NVENC vu ainsi) garde
/// quelques images avant d'en rendre une ; une fenêtre immobile n'en fournit qu'une, qui restait
/// coincée : rien ne s'affichait. Tant que la dernière vraie image (`last_real`, son rang parmi
/// les entrées) n'est pas sortie (`out` sorties, dans l'ordre des entrées), on répète la même
/// texture à la cadence, quelques fois au plus : des images P minuscules.
pub fn should_repeat(out: u64, last_real: Option<u64>, repeats: u32, idle: Duration, interval: Duration) -> bool {
    match last_real {
        Some(r) => out <= r && repeats < MAX_REPEATS && idle >= interval,
        None => false,
    }
}

pub fn run(gpu: Arc<Gpu>, mailbox: Mailbox, shared: Arc<Shared>, cfg: StreamConfig, sink: FrameSink) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    if let Err(e) = encode_loop(&gpu, &mailbox, &shared, cfg, &sink) {
        if !shared.stop.load(Ordering::SeqCst) {
            sink(StreamEvent::Failed(e));
        }
    }
    unsafe { CoUninitialize() };
}

fn encode_loop(
    gpu: &Gpu,
    mailbox: &Mailbox,
    shared: &Shared,
    cfg: StreamConfig,
    sink: &FrameSink,
) -> Result<(), String> {
    let started = Instant::now();
    let mut converter: Option<Converter> = None;
    let mut encoder: Option<Encoder> = None;
    let mut need = 0u32;
    let mut fed_seq = 0u64;
    let mut fed_at: Option<Instant> = None;
    let mut resized_at: Option<Instant> = None;
    let mut first_feed = true;
    let mut frames = 0u64;
    // Entrées et sorties de l'encodeur courant, rang de la dernière vraie image, répétitions depuis.
    let (mut fed_count, mut out_count, mut last_real, mut repeats) = (0u64, 0u64, None::<u64>, 0u32);
    let mut last_report = Instant::now();

    while !shared.stop.load(Ordering::SeqCst) {
        let (seq, texture, w, h) = {
            let l = mailbox.0.lock().unwrap();
            (l.seq, l.texture.clone(), l.width, l.height)
        };
        let Some(texture) = texture else {
            if started.elapsed() > FIRST_FRAME_TIMEOUT {
                return Err("aucune image capturée en 5 s".into());
            }
            let l = mailbox.0.lock().unwrap();
            let _ = mailbox.1.wait_timeout(l, Duration::from_millis(50));
            continue;
        };
        let fps = shared.fps.load(Ordering::SeqCst).max(1);

        // Encodeur et convertisseur à la taille de la source.
        let want = encoded_size(w, h, cfg.max_side);
        match &encoder {
            None => {
                let enc = Encoder::new(&gpu.device, gpu.luid, want, fps, cfg.bitrate)?;
                log!("capture {w}×{h} → encodage {}×{} par {}", want.0, want.1, enc.name);
                converter = Some(Converter::new(&gpu.device, &gpu.context, (w, h), want, fps)?);
                encoder = Some(enc);
                need = 0;
                first_feed = true;
                resized_at = None;
                (fed_count, out_count, last_real, repeats) = (0, 0, None, 0);
            }
            Some(enc) => {
                if enc.size != want {
                    let since = *resized_at.get_or_insert_with(Instant::now);
                    if since.elapsed() >= SETTLE {
                        log!("la source fait maintenant {w}×{h} : nouvel encodeur");
                        encoder = None;
                        converter = None;
                        continue;
                    }
                } else {
                    resized_at = None;
                }
                let out = enc.size;
                if converter.as_ref().map(|c| c.in_size) != Some((w, h)) {
                    // En attendant la nouvelle taille, l'image est mise à l'échelle dans l'ancienne.
                    converter = Some(Converter::new(&gpu.device, &gpu.context, (w, h), out, fps)?);
                }
            }
        }
        let enc = encoder.as_mut().unwrap();

        // Ce que l'encodeur a à dire.
        while let Some(ev) = enc.poll()? {
            match ev {
                EncoderEvent::NeedInput => need += 1,
                EncoderEvent::HaveOutput => {
                    if let Some(au) = enc.pull()? {
                        frames += 1;
                        out_count += 1;
                        sink(StreamEvent::Frame(au));
                    }
                }
                EncoderEvent::Other => {}
            }
        }

        if need > 0 {
            let interval = Duration::from_secs_f64(0.95 / fps as f64);
            let due = fed_at.is_none_or(|t| t.elapsed() >= interval);
            let fresh = seq > fed_seq;
            let key = shared.keyframe.load(Ordering::SeqCst);
            let idle = fed_at.map(|t| t.elapsed()).unwrap_or_default();
            let repeat = !fresh && should_repeat(out_count, last_real, repeats, idle, interval);
            // Une image clé demandée réencode la dernière image, même si la source n'a pas bougé.
            if (fresh && due) || key || first_feed || repeat {
                let force = shared.keyframe.swap(false, Ordering::SeqCst) || first_feed;
                let nv12 = converter.as_mut().unwrap().convert(&texture)?;
                let t = started.elapsed().as_nanos() as i64 / 100;
                enc.feed(&nv12, t, force)?;
                if repeat && !force {
                    repeats += 1;
                } else {
                    last_real = Some(fed_count);
                    repeats = 0;
                }
                fed_count += 1;
                need -= 1;
                fed_seq = seq;
                fed_at = Some(Instant::now());
                first_feed = false;
                continue;
            }
            // Attendre une image neuve ou l'échéance de la cadence, par tranches d'1 ms au plus :
            // une image encodée qui sort entre-temps ne doit pas attendre.
            let wait = if fresh || last_real.is_some_and(|r| out_count <= r) {
                interval.saturating_sub(idle)
            } else {
                Duration::from_millis(1)
            };
            let l = mailbox.0.lock().unwrap();
            if l.seq == seq && !l.closed {
                let _ = mailbox.1.wait_timeout(l, wait.clamp(Duration::from_micros(300), Duration::from_millis(1)));
            }
        } else {
            // L'encodeur travaille : ses événements arrivent en quelques millisecondes.
            std::thread::sleep(Duration::from_micros(700));
        }

        if last_report.elapsed() >= Duration::from_secs(30) {
            log!("encodage : {:.0} i/s en moyenne sur 30 s", frames as f64 / last_report.elapsed().as_secs_f64());
            frames = 0;
            last_report = Instant::now();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{encoded_size, should_repeat};
    use std::time::Duration;

    #[test]
    fn repeats_until_the_real_frame_is_out() {
        let ms = Duration::from_millis;
        // Une seule vraie image (rang 0), rien sorti : on répète à la cadence.
        assert!(should_repeat(0, Some(0), 0, ms(10), ms(8)));
        assert!(!should_repeat(0, Some(0), 0, ms(5), ms(8)), "pas avant l'échéance");
        // Elle est sortie : on s'arrête.
        assert!(!should_repeat(1, Some(0), 3, ms(10), ms(8)));
        // Jamais plus de 8 fois.
        assert!(!should_repeat(0, Some(0), 8, ms(10), ms(8)));
        assert!(!should_repeat(0, None, 0, ms(10), ms(8)));
    }

    #[test]
    fn sizes() {
        assert_eq!(encoded_size(1920, 1080, 3840), (1920, 1080));
        assert_eq!(encoded_size(5120, 2880, 3840), (3840, 2160));
        assert_eq!(encoded_size(801, 601, 3840), (800, 600));
        assert_eq!(encoded_size(20, 10, 3840), (64, 64));
    }
}
