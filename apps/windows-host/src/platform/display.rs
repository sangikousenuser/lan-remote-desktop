//! Display transactions are journaled before changing the desktop. Recovery only
//! restores a mode that still equals our applied mode, preserving local changes.
use super::Result;
use lrd_session::{
    identity,
    wire::{DisplayMode, Monitor},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use windows::{
    core::HSTRING,
    Win32::Graphics::{Dxgi::*, Gdi::*},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Snapshot {
    width: u32,
    height: u32,
    frequency: u32,
    bits: u32,
    flags: u32,
    x: i32,
    y: i32,
    orientation: u32,
    fixed: u32,
}
impl Snapshot {
    fn from_mode(m: &DEVMODEW) -> Self {
        unsafe {
            Self {
                width: m.dmPelsWidth,
                height: m.dmPelsHeight,
                frequency: m.dmDisplayFrequency,
                bits: m.dmBitsPerPel,
                flags: m.Anonymous2.dmDisplayFlags,
                x: m.Anonymous1.Anonymous2.dmPosition.x,
                y: m.Anonymous1.Anonymous2.dmPosition.y,
                orientation: m.Anonymous1.Anonymous2.dmDisplayOrientation.0,
                fixed: m.Anonymous1.Anonymous2.dmDisplayFixedOutput.0,
            }
        }
    }
    fn mode(&self) -> DEVMODEW {
        let mut m = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            dmFields: DM_PELSWIDTH
                | DM_PELSHEIGHT
                | DM_DISPLAYFREQUENCY
                | DM_BITSPERPEL
                | DM_DISPLAYFLAGS
                | DM_POSITION
                | DM_DISPLAYORIENTATION
                | DM_DISPLAYFIXEDOUTPUT,
            dmPelsWidth: self.width,
            dmPelsHeight: self.height,
            dmBitsPerPel: self.bits,
            dmDisplayFrequency: self.frequency,
            ..Default::default()
        };
        m.Anonymous2.dmDisplayFlags = self.flags;
        m.Anonymous1.Anonymous2 = DEVMODEW_0_1 {
            dmPosition: windows::Win32::Foundation::POINTL {
                x: self.x,
                y: self.y,
            },
            dmDisplayOrientation: DEVMODE_DISPLAY_ORIENTATION(self.orientation),
            dmDisplayFixedOutput: DEVMODE_DISPLAY_FIXED_OUTPUT(self.fixed),
        };
        m
    }
}
fn settings(name: &str, index: ENUM_DISPLAY_SETTINGS_MODE) -> Result<DEVMODEW> {
    let mut m = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    if !unsafe {
        EnumDisplaySettingsExW(
            &HSTRING::from(name),
            index,
            &mut m,
            ENUM_DISPLAY_SETTINGS_FLAGS(0),
        )
    }
    .as_bool()
    {
        return Err("display mode is unavailable".into());
    }
    Ok(m)
}
fn name(factory: &IDXGIFactory1, adapter: u32, output: u32) -> Result<String> {
    let desc = unsafe {
        factory
            .EnumAdapters1(adapter)?
            .EnumOutputs(output)?
            .GetDesc()?
    };
    Ok(String::from_utf16_lossy(
        &desc.DeviceName[..desc
            .DeviceName
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(desc.DeviceName.len())],
    ))
}
pub fn monitors(factory: &IDXGIFactory1) -> Result<Vec<Monitor>> {
    let mut result = Vec::new();
    for a in 0..16 {
        let adapter = match unsafe { factory.EnumAdapters1(a) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        for o in 0..16 {
            let output = match unsafe { adapter.EnumOutputs(o) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            if !unsafe { output.GetDesc()? }.AttachedToDesktop.as_bool() {
                continue;
            }
            let name = name(factory, a, o)?;
            let current = settings(&name, ENUM_CURRENT_SETTINGS)?;
            let mut modes = Vec::new();
            for i in 0..4096 {
                let Ok(m) = settings(&name, ENUM_DISPLAY_SETTINGS_MODE(i)) else {
                    break;
                };
                let mode = DisplayMode {
                    width: m.dmPelsWidth,
                    height: m.dmPelsHeight,
                    hz: m.dmDisplayFrequency,
                };
                if m.dmBitsPerPel == current.dmBitsPerPel
                    && mode.width >= 640
                    && mode.height >= 480
                    && mode.width <= 8192
                    && mode.height <= 8192
                    && mode.width.is_multiple_of(2)
                    && mode.height.is_multiple_of(2)
                    && !modes.contains(&mode)
                    && modes.len() < 128
                {
                    modes.push(mode);
                }
            }
            modes.sort_by_key(|m| (m.width, m.height, m.hz));
            result.push(Monitor {
                id: (a << 16) | o,
                name,
                width: current.dmPelsWidth,
                height: current.dmPelsHeight,
                modes,
            });
            if result.len() >= 16 {
                return Ok(result);
            }
        }
    }
    Ok(result)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    device: String,
    original: Snapshot,
    applied: Snapshot,
}
fn journal() -> Result<PathBuf> {
    Ok(identity::data_directory()?.join("display-transaction.json"))
}
fn save(lease: &Lease) -> Result<()> {
    let path = journal()?;
    let tmp = path.with_extension(format!("{}.tmp", lrd_session::random_u64()?));
    identity::write_private_new(&tmp, &serde_json::to_vec(lease)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}
impl Lease {
    pub fn change(
        factory: &IDXGIFactory1,
        adapter: u32,
        output: u32,
        requested: &DisplayMode,
    ) -> Result<Self> {
        let device = name(factory, adapter, output)?;
        let original = Snapshot::from_mode(&settings(&device, ENUM_CURRENT_SETTINGS)?);
        let mut chosen = None;
        for i in 0..4096 {
            let Ok(m) = settings(&device, ENUM_DISPLAY_SETTINGS_MODE(i)) else {
                break;
            };
            if m.dmPelsWidth == requested.width
                && m.dmPelsHeight == requested.height
                && m.dmDisplayFrequency == requested.hz
                && m.dmBitsPerPel == original.bits
            {
                chosen = Some(m);
                break;
            }
        }
        let mut applied = Snapshot::from_mode(&chosen.ok_or("unsupported display mode")?);
        applied.x = original.x;
        applied.y = original.y;
        applied.orientation = original.orientation;
        applied.fixed = original.fixed;
        let lease = Self {
            device,
            original,
            applied,
        };
        let mode = lease.applied.mode();
        let name = HSTRING::from(&lease.device);
        if unsafe { ChangeDisplaySettingsExW(&name, Some(&mode), None, CDS_TEST, None) }
            != DISP_CHANGE_SUCCESSFUL
        {
            return Err("display driver rejected the mode".into());
        }
        save(&lease)?;
        if unsafe { ChangeDisplaySettingsExW(&name, Some(&mode), None, CDS_TYPE(0), None) }
            != DISP_CHANGE_SUCCESSFUL
        {
            let _ = lease.restore();
            return Err("display mode change failed".into());
        }
        Ok(lease)
    }
    pub fn restore(&self) -> Result<()> {
        let current = Snapshot::from_mode(&settings(&self.device, ENUM_CURRENT_SETTINGS)?);
        if current == self.applied {
            let mode = self.original.mode();
            if unsafe {
                ChangeDisplaySettingsExW(
                    &HSTRING::from(&self.device),
                    Some(&mode),
                    None,
                    CDS_TYPE(0),
                    None,
                )
            } != DISP_CHANGE_SUCCESSFUL
            {
                return Err("display restoration pending; restart Host to retry".into());
            }
        } else if current != self.original {
            eprintln!("表示設定がローカルで変更されたため、自動復元を省略しました");
        }
        if journal()?.exists() {
            std::fs::remove_file(journal()?)?;
        }
        Ok(())
    }
}
pub fn recover() -> Result<()> {
    let path = journal()?;
    if path.exists() {
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > 16384 {
            return Err("invalid display recovery journal".into());
        }
        let lease: Lease = serde_json::from_slice(&std::fs::read(path)?)?;
        if !lease.device.starts_with("\\\\.\\DISPLAY") || lease.device.len() > 64 {
            return Err("invalid display device journal".into());
        }
        lease.restore()?;
    }
    Ok(())
}
