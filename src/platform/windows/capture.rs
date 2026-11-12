//! Capture d'une fenêtre ou d'un écran par Windows.Graphics.Capture.
//!
//! Chaque image arrivée est copiée dans une texture BGRA à nous, « la dernière image » : la boîte
//! à une place que l'encodeur vide. Une image plus récente remplace l'ancienne.

use crate::log;
use std::sync::{Arc, Condvar, Mutex};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub luid: (u32, i32),
}

// ID3D11Device est libre de fils ; le contexte immédiat est protégé par ID3D11Multithread.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

pub fn create_gpu() -> Result<Gpu, String> {
    unsafe {
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice : {e}"))?;
        let device: ID3D11Device = device.ok_or("pas de périphérique D3D11")?;
        let context: ID3D11DeviceContext = context.ok_or("pas de contexte D3D11")?;
        // La capture, la conversion et l'encodeur partagent le contexte depuis plusieurs fils.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let dxgi: IDXGIDevice = device.cast().map_err(|e| e.to_string())?;
        let adapter = dxgi.GetAdapter().map_err(|e| e.to_string())?;
        let desc = adapter.GetDesc().map_err(|e| e.to_string())?;
        let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
        log!("GPU : {name}");
        Ok(Gpu { device, context, luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart) })
    }
}

pub enum Target {
    Window(HWND),
    Monitor(HMONITOR),
}

/// La dernière image capturée.
pub struct Latest {
    pub texture: Option<ID3D11Texture2D>,
    pub width: u32,
    pub height: u32,
    /// Augmente à chaque image ; 0 tant qu'aucune n'est arrivée.
    pub seq: u64,
    pub closed: bool,
}

unsafe impl Send for Latest {}

pub type Mailbox = Arc<(Mutex<Latest>, Condvar)>;

/// Un objet COM non agile passé au rappel de capture, qui tourne sur un fil du système. Le
/// périphérique D3D11 est sûr entre fils ; le contexte est protégé par ID3D11Multithread.
struct Shared<T>(T);
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}
impl<T> Shared<T> {
    // Passer par une méthode fait capturer l'enveloppe entière, pas seulement son champ.
    fn get(&self) -> &T {
        &self.0
    }
}

pub fn mailbox() -> Mailbox {
    Arc::new((Mutex::new(Latest { texture: None, width: 0, height: 0, seq: 0, closed: false }), Condvar::new()))
}

pub struct Capture {
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    token: i64,
}

unsafe impl Send for Capture {}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn bgra_texture(device: &ID3D11Device, w: u32, h: u32) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
    Ok(t.unwrap())
}

/// Sans bordure jaune autour de ce qui est capturé : Windows 11 l'accorde d'office à une
/// application de bureau, mais il faut le demander une fois.
fn request_borderless() {
    use std::sync::Once;
    use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Ok(op) = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless) {
            let _ = op.join();
        }
    });
}

pub fn start(gpu: &Gpu, target: Target, max_fps: u32, cursor: bool, mailbox: Mailbox) -> Result<Capture, String> {
    let e = |w: &'static str| move |x: windows::core::Error| format!("{w} : {x}");
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        return Err("Windows.Graphics.Capture n'est pas disponible sur ce système".into());
    }
    unsafe {
        let dxgi: IDXGIDevice = gpu.device.cast().map_err(e("IDXGIDevice"))?;
        let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).map_err(e("périphérique WinRT"))?;
        let rt_device: IDirect3DDevice = inspectable.cast().map_err(e("IDirect3DDevice"))?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(e("fabrique de capture"))?;
        let item: GraphicsCaptureItem = match target {
            Target::Window(h) => interop.CreateForWindow(h).map_err(e("capture de la fenêtre"))?,
            Target::Monitor(m) => interop.CreateForMonitor(m).map_err(e("capture de l'écran"))?,
        };
        let size = item.Size().map_err(e("taille"))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &rt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(e("réserve d'images"))?;
        let session = pool.CreateCaptureSession(&item).map_err(e("session de capture"))?;
        request_borderless();
        // Le curseur part à part ; la bordure jaune n'a pas lieu d'être. Ces réglages manquent
        // sur les Windows anciens : on continue sans.
        let _ = session.SetIsCursorCaptureEnabled(cursor);
        let _ = session.SetIsBorderRequired(false);
        let _ = session
            .SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: 10_000_000 / max_fps.max(1) as i64 });

        let shared = Shared((gpu.device.clone(), gpu.context.clone(), rt_device.clone()));
        let pool_size = Arc::new(Mutex::new((size.Width, size.Height)));
        let mb = mailbox.clone();
        let handler =
            TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(move |pool, _| {
                let Some(pool) = pool.as_ref() else { return Ok(()) };
                let (device, context, rt) = shared.get();
                let frame = pool.TryGetNextFrame()?;
                let content = frame.ContentSize()?;
                let (cw, ch) = (content.Width.max(1) as u32, content.Height.max(1) as u32);
                {
                    let mut ps = pool_size.lock().unwrap();
                    if (content.Width, content.Height) != *ps {
                        // La fenêtre a changé de taille : la réserve suit, l'image courante reste bonne.
                        *ps = (content.Width, content.Height);
                        pool.Recreate(
                            rt,
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            2,
                            SizeInt32 { Width: content.Width, Height: content.Height },
                        )?;
                    }
                }
                let surface = frame.Surface()?;
                let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
                let source: ID3D11Texture2D = access.GetInterface()?;
                let (lock, cv) = &*mb;
                let mut latest = lock.lock().unwrap();
                if latest.closed {
                    return Ok(());
                }
                if latest.texture.is_none() || latest.width != cw || latest.height != ch {
                    latest.texture = Some(bgra_texture(device, cw, ch)?);
                    latest.width = cw;
                    latest.height = ch;
                }
                let dst = latest.texture.as_ref().unwrap();
                // La surface de la réserve peut dépasser le contenu juste après un redimensionnement.
                let mut sdesc = D3D11_TEXTURE2D_DESC::default();
                source.GetDesc(&mut sdesc);
                let b = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: cw.min(sdesc.Width),
                    bottom: ch.min(sdesc.Height),
                    back: 1,
                };
                context.CopySubresourceRegion(dst, 0, 0, 0, 0, &source, 0, Some(&b));
                latest.seq += 1;
                cv.notify_all();
                Ok(())
            });
        let token = pool.FrameArrived(&handler).map_err(e("abonnement aux images"))?;
        session.StartCapture().map_err(e("démarrage de la capture"))?;
        log!("capture démarrée : {}×{}", size.Width, size.Height);
        Ok(Capture { session, pool, token })
    }
}
