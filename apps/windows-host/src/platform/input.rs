use super::Result;
use lrd_session::wire::Input;
use std::collections::BTreeSet;
use windows::Win32::UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*};

pub struct Injector {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    rect: windows::Win32::Foundation::RECT,
}
impl Injector {
    pub fn new(rect: windows::Win32::Foundation::RECT) -> Self {
        Self {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            rect,
        }
    }
    pub fn set_rect(&mut self, rect: windows::Win32::Foundation::RECT) {
        self.release();
        self.rect = rect;
    }
    pub fn apply(&mut self, event: Input) -> Result<()> {
        match event {
            Input::Key { code, down } => {
                key(code, down, false)?;
                if down {
                    self.keys.insert(code);
                } else {
                    self.keys.remove(&code);
                }
            }
            Input::Text { text } => {
                for code in text.encode_utf16() {
                    key(code, true, true)?;
                    key(code, false, true)?;
                }
            }
            Input::Mouse { x, y } => unsafe {
                let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2);
                let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2);
                let px = self.rect.left as f64 + x * (self.rect.right - self.rect.left - 1) as f64;
                let py = self.rect.top as f64 + y * (self.rect.bottom - self.rect.top - 1) as f64;
                mouse(
                    ((px - vx as f64) * 65535.0 / (vw - 1) as f64).round() as i32,
                    ((py - vy as f64) * 65535.0 / (vh - 1) as f64).round() as i32,
                    0,
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                )?;
            },
            Input::Relative { dx, dy } => mouse(dx, dy, 0, MOUSEEVENTF_MOVE)?,
            Input::Button { button, down } => {
                let flags = match (button, down) {
                    (0, true) => MOUSEEVENTF_LEFTDOWN,
                    (0, false) => MOUSEEVENTF_LEFTUP,
                    (1, true) => MOUSEEVENTF_RIGHTDOWN,
                    (1, false) => MOUSEEVENTF_RIGHTUP,
                    (2, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (2, false) => MOUSEEVENTF_MIDDLEUP,
                    (_, true) => MOUSEEVENTF_XDOWN,
                    (_, false) => MOUSEEVENTF_XUP,
                };
                mouse(
                    0,
                    0,
                    if button >= 3 { (button - 2) as u32 } else { 0 },
                    flags,
                )?;
                if down {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
            }
            Input::Wheel { delta } => mouse(0, 0, delta as u32, MOUSEEVENTF_WHEEL)?,
            Input::Ime { enabled } => {
                // Windows 10+ exposes deterministic IME ON/OFF virtual keys.
                // Avoid toggling and avoid cross-process Imm context ownership.
                let code = if enabled { 0x16 } else { 0x1a };
                key(code, true, false)?;
                key(code, false, false)?;
            }
            Input::Release => self.release(),
            Input::KeepAlive => {}
        }
        Ok(())
    }
    pub fn release(&mut self) {
        for code in std::mem::take(&mut self.keys) {
            let _ = key(code, false, false);
        }
        for button in std::mem::take(&mut self.buttons) {
            let _ = self.apply(Input::Button {
                button,
                down: false,
            });
        }
    }
}
impl Drop for Injector {
    fn drop(&mut self) {
        self.release();
    }
}
fn key(code: u16, down: bool, unicode: bool) -> Result<()> {
    let extended = matches!(
        code,
        0x21..=0x28 | 0x2d | 0x2e | 0x5b | 0x5c | 0x6f | 0xa3 | 0xa5
    );
    let mut flags = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    if unicode {
        flags |= KEYEVENTF_UNICODE;
    } else if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(if unicode { 0 } else { code }),
                wScan: if unicode { code } else { 0 },
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    send(input)
}
fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> Result<()> {
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                ..Default::default()
            },
        },
    })
}
fn send(input: INPUT) -> Result<()> {
    if unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) } != 1 {
        return Err("Windows rejected input (locked desktop, privilege boundary, or UIPI)".into());
    }
    Ok(())
}
