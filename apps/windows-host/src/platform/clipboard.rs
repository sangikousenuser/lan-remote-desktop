use super::Result;
use lrd_session::{channel::Sender, clipboard as codec};
use std::{
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    time::Duration,
};
use windows::{
    core::w,
    Win32::{
        Foundation::{HANDLE, HGLOBAL},
        System::{DataExchange::*, Memory::*},
        UI::WindowsAndMessaging::*,
    },
};
struct Opened;
impl Drop for Opened {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}
pub fn start(sender: Sender, enabled: Arc<AtomicBool>) -> SyncSender<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(4);
    std::thread::spawn(move || unsafe {
        let Ok(window) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("LAN Remote Desktop Clipboard"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            None,
            None,
            None,
        ) else {
            return;
        };
        let png = RegisterClipboardFormatW(w!("PNG"));
        let mut sequence = GetClipboardSequenceNumber();
        while !sender.stopped() {
            if let Ok(bytes) = rx.try_recv() {
                if enabled.load(Ordering::Acquire) {
                    if let Ok((format, data)) = codec::unpack(&bytes) {
                        if OpenClipboard(window).is_ok() {
                            let _opened = Opened;
                            if write(format, data, png).is_ok() {
                                sequence = GetClipboardSequenceNumber();
                            }
                        }
                    }
                }
            }
            let current = GetClipboardSequenceNumber();
            if enabled.load(Ordering::Acquire)
                && current != sequence
                && OpenClipboard(window).is_ok()
            {
                let _opened = Opened;
                if let Ok(Some((format, data))) = read(png) {
                    if let Ok(packet) = codec::packet(format, &data) {
                        if sender.send(packet).is_ok() {
                            sequence = current;
                        }
                    }
                } else {
                    sequence = current;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = DestroyWindow(window);
    });
    tx
}
unsafe fn memory(format: u32, limit: usize) -> Result<Vec<u8>> {
    let handle = GetClipboardData(format)?;
    let global = HGLOBAL(handle.0);
    let n = GlobalSize(global);
    if n == 0 || n > limit {
        return Err("clipboard memory size limit".into());
    }
    let p = GlobalLock(global);
    if p.is_null() {
        return Err("clipboard lock failed".into());
    }
    let bytes = std::slice::from_raw_parts(p.cast::<u8>(), n).to_vec();
    let _ = GlobalUnlock(global);
    Ok(bytes)
}
unsafe fn read(png: u32) -> Result<Option<(u8, Vec<u8>)>> {
    if IsClipboardFormatAvailable(png).is_ok() {
        let b = memory(png, codec::MAX_BYTES)?;
        codec::validate(1, &b)?;
        return Ok(Some((1, b)));
    }
    if IsClipboardFormatAvailable(8).is_ok() {
        let dib = memory(8, 128 * 1024 * 1024)?;
        if dib.len() < 40 {
            return Err("DIB header truncated".into());
        }
        let header = u32::from_le_bytes(dib[..4].try_into()?) as usize;
        let bits = u16::from_le_bytes(dib[14..16].try_into()?);
        let colors = u32::from_le_bytes(dib[32..36].try_into()?) as usize;
        let width = i32::from_le_bytes(dib[4..8].try_into()?).unsigned_abs();
        let height = i32::from_le_bytes(dib[8..12].try_into()?).unsigned_abs();
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || width as u64 * height as u64 > 32 * 1024 * 1024
            || header > dib.len()
        {
            return Err("DIB dimensions limit".into());
        }
        let palette = if colors > 0 {
            colors.min(256) * 4
        } else if bits <= 8 {
            (1usize << bits) * 4
        } else {
            0
        };
        let compression = u32::from_le_bytes(dib[16..20].try_into()?);
        let masks = if header == 40 && compression == 3 {
            12
        } else {
            0
        };
        let mut bmp = b"BM".to_vec();
        bmp.extend(((dib.len() + 14) as u32).to_le_bytes());
        bmp.extend([0; 4]);
        bmp.extend(((14 + header + palette + masks) as u32).to_le_bytes());
        bmp.extend(dib);
        let image = image::load_from_memory_with_format(&bmp, image::ImageFormat::Bmp)?;
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png)?;
        let out = out.into_inner();
        codec::validate(1, &out)?;
        return Ok(Some((1, out)));
    }
    if IsClipboardFormatAvailable(13).is_ok() {
        let b = memory(13, codec::MAX_BYTES * 2)?;
        if !b.len().is_multiple_of(2) {
            return Err("UTF-16 clipboard alignment".into());
        }
        let wide = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .take_while(|c| *c != 0)
            .collect::<Vec<_>>();
        let text = String::from_utf16(&wide)?;
        return Ok(Some((0, text.into_bytes())));
    }
    Ok(None)
}
unsafe fn set(format: u32, bytes: &[u8]) -> Result<()> {
    let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
    let p = GlobalLock(memory);
    if p.is_null() {
        let _ = windows::Win32::Foundation::GlobalFree(memory);
        return Err("clipboard allocation lock failed".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
    let _ = GlobalUnlock(memory);
    if let Err(e) = SetClipboardData(format, HANDLE(memory.0)) {
        let _ = windows::Win32::Foundation::GlobalFree(memory);
        return Err(e.into());
    }
    Ok(())
}
unsafe fn write(format: u8, data: &[u8], png: u32) -> Result<()> {
    codec::validate(format, data)?;
    if format == 0 {
        let text = std::str::from_utf8(data)?;
        let mut b = Vec::new();
        for w in text.encode_utf16().chain(std::iter::once(0)) {
            b.extend(w.to_le_bytes());
        }
        EmptyClipboard()?;
        set(13, &b)?;
    } else {
        let image = image::load_from_memory_with_format(data, image::ImageFormat::Png)?;
        let rgba = image.to_rgba8();
        let (w, h) = rgba.dimensions();
        let mut dib = Vec::new();
        dib.extend(40u32.to_le_bytes());
        dib.extend((w as i32).to_le_bytes());
        dib.extend((-(h as i32)).to_le_bytes());
        dib.extend(1u16.to_le_bytes());
        dib.extend(32u16.to_le_bytes());
        dib.extend(0u32.to_le_bytes());
        dib.extend((w * h * 4).to_le_bytes());
        dib.extend([0; 16]);
        for p in rgba.pixels() {
            dib.extend([p[2], p[1], p[0], p[3]]);
        }
        EmptyClipboard()?;
        set(png, data)?;
        set(8, &dib)?;
    }
    Ok(())
}
