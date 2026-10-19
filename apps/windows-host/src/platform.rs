//! Windows interactive desktop host and explicit local diagnostics.
mod audio;
mod clipboard;
mod display;
mod encoder;
mod input;
mod network_ui;
mod remote;
mod stream_encoder;
mod tray;
use std::time::{Duration, Instant};
use windows::{
    core::Interface,
    Win32::{
        Foundation::{E_FAIL, HMODULE},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        Media::MediaFoundation::*,
        System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED},
    },
};

type Result<T> = lrd_session::Result<T>;
struct Runtime;
impl Runtime {
    fn initialize() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(error);
            }
        }
        Ok(Self)
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

pub fn run() -> Result<()> {
    // DXGI reports physical pixels. Keep monitor geometry and SendInput's
    // virtual desktop coordinates in that same space at 125%/150% and mixed DPI.
    unsafe {
        use windows::Win32::UI::HiDpi::*;
        if !AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
        .as_bool()
        {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        }
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--network-wizard"] {
        return network_ui::wizard(windows::Win32::Foundation::HWND::default());
    }
    if args.is_empty() || args.iter().any(|a| a == "--serve") {
        network_ui::offer_recovery();
    }
    if args.is_empty() {
        return remote::run(&["--serve".into(), "--auto".into()]);
    }
    if args.iter().any(|a| a == "--serve" || a == "--interfaces") {
        return remote::run(&args);
    }
    if args.is_empty() || args == ["--help"] {
        println!("LAN Remote Desktop Windows Host\n  --serve [--interface INDEX] [--adapter N] [--monitor N] [--fps 30]\n  --interfaces\n  --list-monitors\n  --probe [--seconds 5]\n  --capture-video output.mp4 [--seconds 5]\nUse --serve for authenticated Ethernet connections. Recording is explicit only.");
        return Ok(());
    }
    let mut adapter_index = 0;
    let mut output_index = 0;
    let mut seconds = 5;
    let mut video = None;
    let mut list = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--list-monitors" => list = true,
            "--probe" => {}
            "--adapter" | "--monitor" | "--seconds" | "--capture-video" => {
                let key = &args[i];
                i += 1;
                let value = args.get(i).ok_or("option requires a value")?;
                match key.as_str() {
                    "--adapter" => adapter_index = value.parse()?,
                    "--monitor" => output_index = value.parse()?,
                    "--seconds" => seconds = value.parse()?,
                    _ => video = Some(value.clone()),
                }
            }
            other => return Err(format!("unknown option: {other}").into()),
        }
        i += 1;
    }
    if !(1..=3600).contains(&seconds) {
        return Err("seconds must be 1..3600".into());
    }
    let _runtime = Runtime::initialize()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    if list {
        return list_monitors(&factory);
    }
    encoder::enumerate()?;
    let mut capture = Capture::new(&factory, adapter_index, output_index)?;
    let mut encoder = video
        .as_ref()
        .map(|path| encoder::Encoder::new(path, capture.width, capture.height))
        .transpose()?;
    let start = Instant::now();
    let mut frames = 0u64;
    let mut timeouts = 0u64;
    let mut resets = 0u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        match capture.frame() {
            Ok(Some(pixels)) => {
                if let Some(encoder) = &mut encoder {
                    encoder.write(&pixels, start.elapsed())?;
                }
                frames += 1;
            }
            Ok(None) => timeouts += 1,
            Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
                resets += 1;
                if resets > 20 {
                    return Err(
                        "desktop repeatedly unavailable; unlock the interactive desktop".into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(250));
                let replacement = Capture::new(&factory, adapter_index, output_index)?;
                if encoder.is_some()
                    && (capture.width != replacement.width || capture.height != replacement.height)
                {
                    return Err(
                        "display size changed during recording; start a new recording".into(),
                    );
                }
                capture = replacement;
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Some(encoder) = encoder {
        encoder.finish()?;
    }
    let elapsed = start.elapsed().as_secs_f64();
    println!("{{\"schema\":1,\"test\":\"dxgi_capture\",\"width\":{},\"height\":{},\"frames\":{},\"elapsed_seconds\":{:.3},\"capture_fps\":{:.3},\"timeouts\":{},\"reinitializations\":{},\"h264_file_written\":{},\"remote_connection_tested\":false}}",
        capture.width, capture.height, frames, elapsed, frames as f64/elapsed, timeouts, resets, video.is_some());
    if frames == 0 {
        return Err("no desktop frames acquired".into());
    }
    Ok(())
}

fn list_monitors(factory: &IDXGIFactory1) -> Result<()> {
    for a in 0..64 {
        let adapter = match unsafe { factory.EnumAdapters1(a) } {
            Ok(value) => value,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        for m in 0..64 {
            let output = match unsafe { adapter.EnumOutputs(m) } {
                Ok(value) => value,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = unsafe { output.GetDesc()? };
            let end = desc
                .DeviceName
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(desc.DeviceName.len());
            println!(
                "adapter={a} monitor={m} name={} size={}x{} attached={}",
                String::from_utf16_lossy(&desc.DeviceName[..end]),
                desc.DesktopCoordinates.right - desc.DesktopCoordinates.left,
                desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top,
                desc.AttachedToDesktop.as_bool()
            );
        }
    }
    Ok(())
}

struct Capture {
    processing_ms: std::cell::Cell<f64>,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    surface_width: u32,
    surface_height: u32,
    rotation: lrd_session::frame::Rotation,
    rect: windows::Win32::Foundation::RECT,
}
struct Acquired<'a>(&'a IDXGIOutputDuplication);
impl Drop for Acquired<'_> {
    fn drop(&mut self) {
        let _ = unsafe { self.0.ReleaseFrame() };
    }
}
struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}
impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(self.texture, 0);
        }
    }
}

impl Capture {
    fn new(
        factory: &IDXGIFactory1,
        adapter_index: u32,
        output_index: u32,
    ) -> windows::core::Result<Self> {
        unsafe {
            let adapter = factory.EnumAdapters1(adapter_index)?;
            let output: IDXGIOutput1 = adapter.EnumOutputs(output_index)?.cast()?;
            let rect = output.GetDesc()?.DesktopCoordinates;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device = device.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let context = context.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let duplication = output.DuplicateOutput(&device)?;
            let desc = duplication.GetDesc();
            use lrd_session::frame::Rotation;
            let rotation = match desc.Rotation {
                DXGI_MODE_ROTATION_UNSPECIFIED | DXGI_MODE_ROTATION_IDENTITY => Rotation::Identity,
                DXGI_MODE_ROTATION_ROTATE90 => Rotation::Clockwise90,
                DXGI_MODE_ROTATION_ROTATE180 => Rotation::Clockwise180,
                DXGI_MODE_ROTATION_ROTATE270 => Rotation::Clockwise270,
                _ => {
                    return Err(windows::core::Error::new(
                        E_FAIL,
                        "unknown desktop rotation",
                    ))
                }
            };
            let (width, height) = rotation.dimensions(desc.ModeDesc.Width, desc.ModeDesc.Height);
            if desc.ModeDesc.Width == 0
                || desc.ModeDesc.Height == 0
                || desc.ModeDesc.Width > 8192
                || desc.ModeDesc.Height > 8192
                || rect.right as i64 - rect.left as i64 != width as i64
                || rect.bottom as i64 - rect.top as i64 != height as i64
            {
                return Err(windows::core::Error::new(
                    E_FAIL,
                    "capture dimensions changed or exceed 8192 pixels",
                ));
            }
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.ModeDesc.Width,
                Height: desc.ModeDesc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut staging = None;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            Ok(Self {
                processing_ms: std::cell::Cell::new(0.),
                context,
                duplication,
                staging: staging.ok_or_else(|| windows::core::Error::from(E_FAIL))?,
                width,
                height,
                surface_width: desc.ModeDesc.Width,
                surface_height: desc.ModeDesc.Height,
                rotation,
                rect,
            })
        }
    }
    fn frame(&self) -> windows::core::Result<Option<Vec<u8>>> {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource = None;
            match self
                .duplication
                .AcquireNextFrame(100, &mut info, &mut resource)
            {
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                result => result?,
            }
            let _frame = Acquired(&self.duplication);
            let processing_at = std::time::Instant::now();
            let texture: ID3D11Texture2D = resource
                .ok_or_else(|| windows::core::Error::from(E_FAIL))?
                .cast()?;
            let mut source_desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut source_desc);
            if source_desc.Width != self.surface_width
                || source_desc.Height != self.surface_height
                || source_desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            {
                return Err(windows::core::Error::from(DXGI_ERROR_ACCESS_LOST));
            }
            self.context.CopyResource(&self.staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let _mapping = Mapped {
                context: &self.context,
                texture: &self.staging,
            };
            let row = self.surface_width as usize * 4;
            if mapped.pData.is_null() || (mapped.RowPitch as usize) < row {
                return Err(windows::core::Error::from(E_FAIL));
            }
            let length = (mapped.RowPitch as usize)
                .checked_mul(self.surface_height as usize - 1)
                .and_then(|n| n.checked_add(row))
                .filter(|n| *n <= isize::MAX as usize)
                .ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let source = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), length);
            let pixels = lrd_session::frame::copy_bgra(
                source,
                self.surface_width,
                self.surface_height,
                mapped.RowPitch as usize,
                self.rotation,
            )
            .map_err(|_| windows::core::Error::from(E_FAIL))?;
            self.processing_ms
                .set(processing_at.elapsed().as_secs_f64() * 1000.);
            Ok(Some(pixels))
        }
    }
}
