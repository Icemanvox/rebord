//! BGRA → NV12 (BT.709, plage limitée) sur le GPU, avec mise à l'échelle, par le processeur vidéo
//! D3D11. Les textures NV12 tournent sur un anneau : l'encodeur garde la sienne le temps d'encoder.

use std::mem::ManuallyDrop;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC};
use windows::core::Interface;

const RING: usize = 6;

pub struct Converter {
    vctx: ID3D11VideoContext,
    vdev: ID3D11VideoDevice,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    input: Option<(usize, ID3D11VideoProcessorInputView)>,
    pub in_size: (u32, u32),
}

unsafe impl Send for Converter {}

impl Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        in_size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
    ) -> Result<Self, String> {
        let e = |w: &'static str| move |x: windows::core::Error| format!("{w} : {x}");
        unsafe {
            let vdev: ID3D11VideoDevice = device.cast().map_err(e("ID3D11VideoDevice"))?;
            let vctx: ID3D11VideoContext = context.cast().map_err(e("ID3D11VideoContext"))?;
            let rate = DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 };
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: rate,
                InputWidth: in_size.0,
                InputHeight: in_size.1,
                OutputFrameRate: rate,
                OutputWidth: out_size.0,
                OutputHeight: out_size.1,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let enumerator =
                vdev.CreateVideoProcessorEnumerator(&desc).map_err(e("énumérateur du processeur vidéo"))?;
            let processor = vdev.CreateVideoProcessor(&enumerator, 0).map_err(e("processeur vidéo"))?;

            // Entrée : RGB 0-255. Sortie : YCbCr BT.709, 16-235 (ce que l'app décode par défaut).
            // Champs de bits : Usage(1) RGB_Range(1) YCbCr_Matrix(1) YCbCr_xvYCC(1) Nominal_Range(2).
            let input_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
            let output_cs = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: (1 << 2) | (1 << 4) };
            vctx.VideoProcessorSetStreamColorSpace(&processor, 0, &input_cs);
            vctx.VideoProcessorSetOutputColorSpace(&processor, &output_cs);
            vctx.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let src = RECT { left: 0, top: 0, right: in_size.0 as i32, bottom: in_size.1 as i32 };
            let dst = RECT { left: 0, top: 0, right: out_size.0 as i32, bottom: out_size.1 as i32 };
            vctx.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            vctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            vctx.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));

            let mut outputs = Vec::with_capacity(RING);
            for _ in 0..RING {
                let tdesc = D3D11_TEXTURE2D_DESC {
                    Width: out_size.0,
                    Height: out_size.1,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_NV12,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut t = None;
                device.CreateTexture2D(&tdesc, None, Some(&mut t)).map_err(e("texture NV12"))?;
                let t = t.unwrap();
                let vdesc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut view = None;
                vdev.CreateVideoProcessorOutputView(&t, &enumerator, &vdesc, Some(&mut view))
                    .map_err(e("vue de sortie"))?;
                outputs.push((t, view.unwrap()));
            }
            Ok(Converter { vctx, vdev, enumerator, processor, outputs, next: 0, input: None, in_size })
        }
    }

    /// Convertit `source` (BGRA, de la taille d'entrée) et rend la texture NV12 remplie.
    pub fn convert(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D, String> {
        unsafe {
            let key = source.as_raw() as usize;
            if self.input.as_ref().map(|(k, _)| *k) != Some(key) {
                let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                    },
                };
                let mut view = None;
                self.vdev
                    .CreateVideoProcessorInputView(source, &self.enumerator, &desc, Some(&mut view))
                    .map_err(|x| format!("vue d'entrée : {x}"))?;
                self.input = Some((key, view.unwrap()));
            }
            let view = self.input.as_ref().unwrap().1.clone();
            let (texture, out_view) = &self.outputs[self.next];
            self.next = (self.next + 1) % self.outputs.len();
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM { Enable: true.into(), ..Default::default() };
            stream.pInputSurface = ManuallyDrop::new(Some(view));
            let r = self.vctx.VideoProcessorBlt(&self.processor, out_view, 0, std::slice::from_ref(&stream));
            ManuallyDrop::drop(&mut stream.pInputSurface);
            r.map_err(|x| format!("VideoProcessorBlt : {x}"))?;
            Ok(texture.clone())
        }
    }
}
