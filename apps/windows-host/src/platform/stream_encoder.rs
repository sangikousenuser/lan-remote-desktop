//! Real-time H.264 MFT adapter. Software fallback is explicit in diagnostics.
use super::Result;
use std::mem::ManuallyDrop;
use windows::{
    core::{Interface, VARIANT},
    Win32::{
        Media::MediaFoundation::*,
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
    },
};

pub struct StreamEncoder {
    transform: IMFTransform,
    width: u32,
    height: u32,
    frame: u64,
    header: Vec<u8>,
    fps: u32,
    events: Option<IMFMediaEventGenerator>,
    input_ready: u32,
    output_ready: u32,
    started: std::time::Instant,
    produced: u64,
    last_output: std::time::Instant,
    last_encode_call: std::time::Instant,
}
impl StreamEncoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self> {
        if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err("H.264 requires even monitor dimensions".into());
        }
        for transform in hardware_transforms() {
            if let Ok(encoder) = Self::configure_transform(transform, width, height, fps, true) {
                eprintln!("H.264: hardware MFT selected; frame output will be validated");
                return Ok(encoder);
            }
        }
        Self::software(width, height, fps)
    }
    fn software(width: u32, height: u32, fps: u32) -> Result<Self> {
        eprintln!("H.264: Microsoft software MFT fallback");
        let transform =
            unsafe { CoCreateInstance(&CMSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)? };
        Self::configure_transform(transform, width, height, fps, false)
    }
    fn configure_transform(
        transform: IMFTransform,
        width: u32,
        height: u32,
        fps: u32,
        hardware: bool,
    ) -> Result<Self> {
        unsafe {
            let events = if hardware {
                transform
                    .GetAttributes()?
                    .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                Some(transform.cast::<IMFMediaEventGenerator>()?)
            } else {
                None
            };
            let codec: ICodecAPI = transform.cast()?;
            // Low-latency baseline with one-second recovery points; no B frames.
            let _ = codec.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
            let _ = codec.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(fps));
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, 8_000_000)?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, 66)?;
            configure(&output, width, height, fps)?;
            transform.SetOutputType(0, &output, 0)?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            configure(&input, width, height, fps)?;
            transform.SetInputType(0, &input, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            Ok(Self {
                transform,
                width,
                height,
                frame: 0,
                header: Vec::new(),
                fps,
                events,
                input_ready: 0,
                output_ready: 0,
                started: std::time::Instant::now(),
                produced: 0,
                last_output: std::time::Instant::now(),
                last_encode_call: std::time::Instant::now(),
            })
        }
    }
    pub fn set_bitrate(&self, value: u32) {
        unsafe {
            if let Ok(codec) = self.transform.cast::<ICodecAPI>() {
                let _ = codec.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &VARIANT::from(value));
            }
        }
    }
    pub fn request_keyframe(&self) {
        unsafe {
            if let Ok(codec) = self.transform.cast::<ICodecAPI>() {
                let _ = codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(true));
            }
        }
    }
    pub fn encode(&mut self, bgra: &[u8]) -> Result<Vec<Vec<u8>>> {
        let active = self.last_encode_call.elapsed() < std::time::Duration::from_millis(1500);
        self.last_encode_call = std::time::Instant::now();
        let stalled = self.events.is_some()
            && active
            && self.last_output.elapsed() > std::time::Duration::from_secs(2);
        let result = if stalled {
            Err("hardware encoder produced no frames".into())
        } else {
            self.encode_inner(bgra)
        };
        if result.is_err() && self.events.is_some() {
            *self = Self::software(self.width, self.height, self.fps)?;
            return self.encode_inner(bgra);
        }
        result
    }
    pub fn hardware_active(&self) -> Option<bool> {
        (self.produced > 0).then_some(self.events.is_some())
    }
    fn poll_events(&mut self) -> Result<()> {
        if let Some(events) = self.events.clone() {
            for _ in 0..64 {
                let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(event) => event,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(e) => return Err(e.into()),
                };
                unsafe {
                    event.GetStatus()?.ok()?;
                    match event.GetType()? {
                        x if x == METransformNeedInput.0 as u32 => {
                            self.input_ready = self.input_ready.saturating_add(1)
                        }
                        x if x == METransformHaveOutput.0 as u32 => {
                            self.output_ready = self.output_ready.saturating_add(1)
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
    fn encode_inner(&mut self, bgra: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.poll_events()?;
        let mut result = self.drain_output()?;
        if self.events.is_some() && self.input_ready == 0 {
            return Ok(result);
        }
        let nv12 =
            lrd_session::color::bgra_to_nv12(bgra, self.width as usize, self.height as usize)?;
        unsafe {
            let input = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(nv12.len() as u32)?;
            let mut p = std::ptr::null_mut();
            buffer.Lock(&mut p, None, None)?;
            if p.is_null() {
                let _ = buffer.Unlock();
                return Err("MFT input buffer missing".into());
            }
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), p, nv12.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(nv12.len() as u32)?;
            input.AddBuffer(&buffer)?;
            input.SetSampleTime(
                (self.started.elapsed().as_nanos() / 100).min(i64::MAX as u128) as i64,
            )?;
            input.SetSampleDuration(10_000_000 / self.fps as i64)?;
            self.transform.ProcessInput(0, &input, 0)?;
            self.frame += 1;
            if self.events.is_some() {
                self.input_ready = self.input_ready.saturating_sub(1);
            }
        }
        self.poll_events()?;
        result.extend(self.drain_output()?);
        Ok(result)
    }
    fn drain_output(&mut self) -> Result<Vec<Vec<u8>>> {
        let mut result = Vec::new();
        unsafe {
            for _ in 0..64 {
                if self.events.is_some() && self.output_ready == 0 {
                    break;
                }
                let info = self.transform.GetOutputStreamInfo(0)?;
                if info.cbSize > 8 * 1024 * 1024 {
                    return Err("MFT output buffer limit".into());
                }
                let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&MFCreateMemoryBuffer(info.cbSize.max(4 * 1024 * 1024))?)?;
                    Some(sample)
                } else {
                    None
                };
                let mut output = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    ..Default::default()
                }];
                let mut status = 0;
                let processed = self.transform.ProcessOutput(0, &mut output, &mut status);
                let sample = ManuallyDrop::take(&mut output[0].pSample);
                let events = ManuallyDrop::take(&mut output[0].pEvents);
                drop(events);
                match processed {
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let current = self.transform.GetOutputAvailableType(0, 0)?;
                        self.transform.SetOutputType(0, &current, 0)?;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                    Ok(()) => {}
                }
                let sample = sample.ok_or("MFT omitted output sample")?;
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut ptr = std::ptr::null_mut();
                let mut length = 0;
                buffer.Lock(&mut ptr, None, Some(&mut length))?;
                if ptr.is_null() || length > 8 * 1024 * 1024 {
                    let _ = buffer.Unlock();
                    return Err("invalid H.264 output size".into());
                }
                let encoded = std::slice::from_raw_parts(ptr, length as usize).to_vec();
                buffer.Unlock()?;
                if self.header.is_empty() {
                    let media = self.transform.GetOutputCurrentType(0)?;
                    if let Ok(n) = media.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) {
                        if n <= 65536 {
                            let mut h = vec![0; n as usize];
                            media.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut h, None)?;
                            self.header = lrd_session::h264::annex_b(&h)?;
                        }
                    }
                }
                let mut frame = self.header.clone();
                frame.extend(lrd_session::h264::annex_b(&encoded)?);
                result.push(frame);
                self.produced += 1;
                self.last_output = std::time::Instant::now();
                self.output_ready = self.output_ready.saturating_sub(1);
            }
            Ok(result)
        }
    }
}
impl Drop for StreamEncoder {
    fn drop(&mut self) {
        let _ = unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) };
    }
}
fn hardware_transforms() -> Vec<IMFTransform> {
    unsafe {
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut pointer = std::ptr::null_mut();
        let mut count = 0;
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut pointer,
            &mut count,
        )
        .is_err()
            || pointer.is_null()
        {
            return Vec::new();
        }
        let mut transforms = Vec::new();
        for entry in std::slice::from_raw_parts_mut(pointer, count as usize) {
            if let Some(activate) = entry.take() {
                if let Ok(transform) = activate.ActivateObject::<IMFTransform>() {
                    transforms.push(transform);
                }
            }
        }
        windows::Win32::System::Com::CoTaskMemFree(Some(pointer.cast()));
        transforms
    }
}

unsafe fn configure(media: &IMFMediaType, w: u32, h: u32, fps: u32) -> windows::core::Result<()> {
    media.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
    media.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
    media.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
    media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    media.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
    media.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
    media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_software_mft_encodes_generated_pixels_with_parameter_sets() {
        let _runtime = crate::platform::Runtime::initialize().unwrap();
        let mut encoder = StreamEncoder::software(640, 480, 30).unwrap();
        encoder.set_bitrate(4_000_000);
        encoder.request_keyframe();
        let mut output = Vec::new();
        let mut pixels = vec![0u8; 640 * 480 * 4];
        for i in 0..32u8 {
            for (n, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                pixel.copy_from_slice(&[(n % 256) as u8, i, 100, 255]);
            }
            output.extend(encoder.encode(&pixels).unwrap());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!output.is_empty(), "native MFT produced no H.264");
        if let Ok(path) = std::env::var("LRD_ENCODER_FIXTURE") {
            let first = &output[0];
            std::fs::write(&path, first).unwrap();
            let metadata = serde_json::json!({"schema":1,"source":"generated pixels / Microsoft software H.264 MFT",
                "width":640,"height":480,"bytes":first.len(),"sha256":lrd_session::identity::fingerprint(first),
                "desktop_captured":false,"hardware_encoder_tested":false});
            std::fs::write(
                std::path::Path::new(&path).with_extension("json"),
                serde_json::to_vec_pretty(&metadata).unwrap(),
            )
            .unwrap();
        }
        let mut kinds = std::collections::HashSet::new();
        for bytes in output {
            for i in 0..bytes.len().saturating_sub(4) {
                if bytes[i..].starts_with(&[0, 0, 0, 1]) {
                    kinds.insert(bytes[i + 4] & 31);
                } else if bytes[i..].starts_with(&[0, 0, 1]) {
                    kinds.insert(bytes[i + 3] & 31);
                }
            }
        }
        assert!(
            kinds.contains(&7) && kinds.contains(&8) && kinds.contains(&5),
            "missing SPS/PPS/IDR: {kinds:?}"
        );
    }
}
