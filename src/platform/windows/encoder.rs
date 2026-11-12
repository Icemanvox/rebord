//! Encodeur HEVC matériel de Media Foundation (MFT asynchrone, entrée en textures D3D11).
//!
//! Réglages calqués sur l'hôte Mac (HEVCEncoder.swift) : temps réel, Main 8 bits, sans images B,
//! une image clé toutes les 4 s au plus, débit moyen donné, images clés à la demande.

use crate::log;
use crate::proto::{ParameterSets, hevc};
use std::mem::ManuallyDrop;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::core::{GUID, Interface, PWSTR};

fn variant_u32(v: u32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_UI4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { ulVal: v },
            }),
        },
    }
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

pub enum EncoderEvent {
    NeedInput,
    HaveOutput,
    Other,
}

pub struct Encoder {
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    _manager: IMFDXGIDeviceManager,
    provides_samples: bool,
    output_size: u32,
    pub name: String,
    pub size: (u32, u32),
    fps: u32,
    sequence_header: Option<ParameterSets>,
}

unsafe impl Send for Encoder {}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(s) = self.mft.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
    }
}

struct Candidate {
    activate: IMFActivate,
    name: String,
    luid: Option<(u32, i32)>,
}

fn candidates() -> Result<Vec<Candidate>, String> {
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_HEVC };
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0),
            Some(&input),
            Some(&output),
            &mut list,
            &mut count,
        )
        .map_err(|e| format!("MFTEnumEx : {e}"))?;
        let mut out = Vec::new();
        for i in 0..count as usize {
            if let Some(a) = std::ptr::read(list.add(i)) {
                let mut s = PWSTR::null();
                let mut n = 0u32;
                let name = if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut s, &mut n).is_ok() {
                    let v = s.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(s.0 as *const _));
                    v
                } else {
                    "encodeur HEVC".to_string()
                };
                let mut luid_bytes = [0u8; 8];
                let luid = a.GetBlob(&MFT_ENUM_ADAPTER_LUID, &mut luid_bytes, None).ok().map(|_| {
                    (
                        u32::from_le_bytes(luid_bytes[0..4].try_into().unwrap()),
                        i32::from_le_bytes(luid_bytes[4..8].try_into().unwrap()),
                    )
                });
                out.push(Candidate { activate: a, name, luid });
            }
        }
        if !list.is_null() {
            CoTaskMemFree(Some(list as *const _));
        }
        Ok(out)
    }
}

impl Encoder {
    /// Le premier encodeur matériel HEVC qui accepte ce GPU et ces réglages.
    pub fn new(
        device: &ID3D11Device,
        luid: (u32, i32),
        size: (u32, u32),
        fps: u32,
        bitrate: u32,
    ) -> Result<Encoder, String> {
        let mut found = candidates()?;
        if found.is_empty() {
            return Err("aucun encodeur HEVC matériel (il faut un GPU Intel 6e génération, NVIDIA GTX 950 / AMD RX 400 ou plus récent)".into());
        }
        // Ceux du GPU qui capture d'abord.
        found.sort_by_key(|c| c.luid.map(|l| l != luid).unwrap_or(true));
        let mut errors = Vec::new();
        for c in found {
            match Self::open(&c, device, size, fps, bitrate) {
                Ok(e) => return Ok(e),
                Err(e) => {
                    unsafe {
                        let _ = c.activate.ShutdownObject();
                    }
                    errors.push(format!("{} : {e}", c.name))
                }
            }
        }
        Err(errors.join(" ; "))
    }

    fn open(c: &Candidate, device: &ID3D11Device, size: (u32, u32), fps: u32, bitrate: u32) -> Result<Encoder, String> {
        let e = |w: &'static str| move |x: windows::core::Error| format!("{w} : {x}");
        unsafe {
            let mft: IMFTransform = c.activate.ActivateObject().map_err(e("activation"))?;
            let attrs = mft.GetAttributes().map_err(e("attributs"))?;
            if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
                return Err("encodeur synchrone, non géré".into());
            }
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(e("déverrouillage asynchrone"))?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(e("gestionnaire DXGI"))?;
            let manager = manager.unwrap();
            manager.ResetDevice(device, token).map_err(e("ResetDevice"))?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize).map_err(e("SET_D3D_MANAGER"))?;

            let codec = mft.cast::<ICodecAPI>().ok();
            if let Some(api) = &codec {
                let set = |g: &GUID, v: u32, what: &str| {
                    // Les encodeurs NVIDIA refusent le nombre d'images B, qu'ils n'utilisent pas en
                    // faible latence : inutile de le dire à chaque fenêtre.
                    if let Err(x) = api.SetValue(g, &variant_u32(v)) {
                        if *g != CODECAPI_AVEncMPVDefaultBPictureCount {
                            log!("encodeur : réglage {what} refusé ({x})");
                        }
                    }
                };
                set(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_CBR.0 as u32, "débit constant");
                set(&CODECAPI_AVEncCommonMeanBitRate, bitrate, "débit");
                set(&CODECAPI_AVLowLatencyMode, 1, "faible latence");
                set(&CODECAPI_AVEncMPVGOPSize, fps * 4, "GOP");
                set(&CODECAPI_AVEncMPVDefaultBPictureCount, 0, "images B");
                set(&CODECAPI_AVEncCommonQualityVsSpeed, 50, "qualité/vitesse");
            }

            let out = MFCreateMediaType().map_err(e("type de sortie"))?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("type"))?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC).map_err(e("HEVC"))?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(e("débit"))?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1)).map_err(e("taille"))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(e("cadence"))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(e("pixels"))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("progressif"))?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32).map_err(e("profil"))?;
            let _ = out.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32);
            let _ = out.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32);
            let _ = out.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32);
            let _ = out.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            mft.SetOutputType(0, &out, 0).map_err(e("SetOutputType"))?;

            let input = MFCreateMediaType().map_err(e("type d'entrée"))?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("type"))?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).map_err(e("NV12"))?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1)).map_err(e("taille"))?;
            input.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(e("cadence"))?;
            input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(e("pixels"))?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(e("progressif"))?;
            mft.SetInputType(0, &input, 0).map_err(e("SetInputType"))?;

            let info = mft.GetOutputStreamInfo(0).map_err(e("GetOutputStreamInfo"))?;
            let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;

            // Les VPS/SPS/PPS, si l'encodeur les publie dans son type : secours au cas où une
            // image clé ne les porterait pas.
            let sequence_header = mft.GetOutputCurrentType(0).ok().and_then(|t| {
                let n = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
                let mut b = vec![0u8; n as usize];
                t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut b, None).ok()?;
                hevc::parameter_sets_from_annexb(&b)
            });

            mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0).map_err(e("FLUSH"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(e("BEGIN_STREAMING"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(e("START_OF_STREAM"))?;
            let events: IMFMediaEventGenerator = mft.cast().map_err(e("générateur d'événements"))?;
            log!("encodeur : {} ({}×{}, {} i/s, {} Mbit/s)", c.name, size.0, size.1, fps, bitrate / 1_000_000);
            Ok(Encoder {
                mft,
                events,
                codec,
                _manager: manager,
                provides_samples,
                output_size: info.cbSize.max(size.0 * size.1 * 3 / 2).max(1 << 20),
                name: c.name.clone(),
                size,
                fps,
                sequence_header,
            })
        }
    }

    /// Le prochain événement de l'encodeur, sans attendre.
    pub fn poll(&self) -> Result<Option<EncoderEvent>, String> {
        unsafe {
            match self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                Ok(ev) => {
                    let t = ev.GetType().map_err(|e| e.to_string())?;
                    Ok(Some(match t as i32 {
                        x if x == METransformNeedInput.0 => EncoderEvent::NeedInput,
                        x if x == METransformHaveOutput.0 => EncoderEvent::HaveOutput,
                        _ => EncoderEvent::Other,
                    }))
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => Ok(None),
                Err(e) => Err(format!("GetEvent : {e}")),
            }
        }
    }

    pub fn feed(&mut self, texture: &ID3D11Texture2D, time_100ns: i64, keyframe: bool) -> Result<(), String> {
        let e = |w: &'static str| move |x: windows::core::Error| format!("{w} : {x}");
        unsafe {
            let buffer =
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false).map_err(e("tampon DXGI"))?;
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
                if let Ok(len) = b2.GetContiguousLength() {
                    let _ = buffer.SetCurrentLength(len);
                }
            }
            let sample = MFCreateSample().map_err(e("échantillon"))?;
            sample.AddBuffer(&buffer).map_err(e("AddBuffer"))?;
            sample.SetSampleTime(time_100ns).map_err(e("horodatage"))?;
            sample.SetSampleDuration(10_000_000 / self.fps.max(1) as i64).map_err(e("durée"))?;
            if keyframe {
                if let Some(api) = &self.codec {
                    let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1));
                }
            }
            self.mft.ProcessInput(0, &sample, 0).map_err(e("ProcessInput"))
        }
    }

    /// Une unité d'accès sortie de l'encodeur, déjà au format de l'app.
    pub fn pull(&mut self) -> Result<Option<hevc::AccessUnit>, String> {
        unsafe {
            let own = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample().map_err(|e| e.to_string())?;
                let b = MFCreateMemoryBuffer(self.output_size).map_err(|e| e.to_string())?;
                s.AddBuffer(&b).map_err(|e| e.to_string())?;
                Some(s)
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(own),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            drop(ManuallyDrop::take(&mut out[0].pEvents));
            match r {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // L'encodeur renégocie sa sortie : on reprend le type qu'il propose.
                    let t = self.mft.GetOutputAvailableType(0, 0).map_err(|e| e.to_string())?;
                    self.mft.SetOutputType(0, &t, 0).map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                Err(e) => return Err(format!("ProcessOutput : {e}")),
            }
            let Some(sample) = sample else { return Ok(None) };
            let clean = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let buf = sample.ConvertToContiguousBuffer().map_err(|e| e.to_string())?;
            let mut p: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut p, None, Some(&mut len)).map_err(|e| e.to_string())?;
            let bytes = std::slice::from_raw_parts(p, len as usize).to_vec();
            let _ = buf.Unlock();
            let Some(mut au) = hevc::from_annexb(&bytes) else { return Ok(None) };
            au.keyframe |= clean;
            if au.keyframe && au.parameter_sets.is_none() {
                au.parameter_sets = self.sequence_header.clone();
            }
            if let Some(ps) = &au.parameter_sets {
                self.sequence_header = Some(ps.clone());
            }
            Ok(Some(au))
        }
    }
}
