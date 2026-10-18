use super::Result;
use std::{path::Path, time::Duration};
use windows::{
    core::{HSTRING, PCWSTR, PWSTR},
    Win32::{Foundation::E_FAIL, Media::MediaFoundation::*, System::Com::CoTaskMemFree},
};

pub fn enumerate() -> windows::core::Result<()> {
    unsafe {
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut array = std::ptr::null_mut();
        let mut count = 0;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut array,
            &mut count,
        )?;
        if count > 0 && array.is_null() {
            return Err(windows::core::Error::from(E_FAIL));
        }
        let entries = if count == 0 {
            &mut []
        } else {
            std::slice::from_raw_parts_mut(array, count as usize)
        };
        eprintln!("H.264 hardware encoder candidates: {count} (enumeration is not proof of hardware encoding)");
        for entry in entries.iter_mut() {
            if let Some(activate) = entry.take() {
                let mut name = PWSTR::null();
                let mut length = 0;
                if activate
                    .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut length)
                    .is_ok()
                    && !name.is_null()
                {
                    eprintln!(
                        "  {}",
                        String::from_utf16_lossy(std::slice::from_raw_parts(
                            name.0,
                            length as usize
                        ))
                    );
                    CoTaskMemFree(Some(name.0.cast()));
                }
            }
        }
        CoTaskMemFree(Some(array.cast()));
        Ok(())
    }
}

pub struct Encoder {
    writer: IMFSinkWriter,
    stream: u32,
    bytes: usize,
    last_time: i64,
}
impl Encoder {
    pub fn new(path: &str, width: u32, height: u32) -> Result<Self> {
        let path = Path::new(path);
        if path.exists() {
            return Err("output already exists; choose a new file".into());
        }
        if path.extension().and_then(|x| x.to_str()) != Some("mp4")
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            return Err("use .mp4 and even display dimensions".into());
        }
        unsafe {
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.ok_or("MF attributes missing")?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            let filename = HSTRING::from(path.as_os_str());
            let writer = MFCreateSinkWriterFromURL(PCWSTR(filename.as_ptr()), None, &attrs)?;
            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, 12_000_000)?;
            configure_video(&output, width, height)?;
            let stream = writer.AddStream(&output)?;
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)?;
            configure_video(&input, width, height)?;
            writer.SetInputMediaType(stream, &input, None)?;
            writer.BeginWriting()?;
            Ok(Self {
                writer,
                stream,
                bytes: width as usize * height as usize * 4,
                last_time: -1,
            })
        }
    }
    pub fn write(&mut self, pixels: &[u8], elapsed: Duration) -> Result<()> {
        if pixels.len() != self.bytes {
            return Err("captured image size changed".into());
        }
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(pixels.len() as u32)?;
            let mut pointer = std::ptr::null_mut();
            buffer.Lock(&mut pointer, None, None)?;
            if pointer.is_null() {
                let _ = buffer.Unlock();
                return Err("MF buffer lock returned null".into());
            }
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), pointer, pixels.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(pixels.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            let timestamp =
                ((elapsed.as_nanos() / 100).min(i64::MAX as u128) as i64).max(self.last_time + 1);
            sample.SetSampleTime(timestamp)?;
            sample.SetSampleDuration(10_000_000 / 60)?;
            self.writer.WriteSample(self.stream, &sample)?;
            self.last_time = timestamp;
            Ok(())
        }
    }
    pub fn finish(self) -> windows::core::Result<()> {
        unsafe { self.writer.Finalize() }
    }
}

unsafe fn configure_video(
    media: &IMFMediaType,
    width: u32,
    height: u32,
) -> windows::core::Result<()> {
    media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    media.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)?;
    media.SetUINT64(&MF_MT_FRAME_RATE, (60u64 << 32) | 1)?;
    media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
}
