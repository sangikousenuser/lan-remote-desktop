//! The tray and file chooser run in the logged-in user's desktop, never Session 0.
use super::{remote, Result};
use std::sync::{Mutex, OnceLock};
use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::*,
        System::Com::*,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};
static TRANSFER_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static TRANSFER_STATUS: OnceLock<Mutex<String>> = OnceLock::new();
pub fn transfer_status(message: String) {
    *TRANSFER_STATUS
        .get_or_init(|| Mutex::new(String::new()))
        .lock()
        .unwrap() = message;
    let h = TRANSFER_WINDOW.load(std::sync::atomic::Ordering::Acquire);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut _), WM_APP + 2, WPARAM(0), LPARAM(0));
        }
    }
}
static INFO: OnceLock<Mutex<String>> = OnceLock::new();
fn info() -> &'static Mutex<String> {
    INFO.get_or_init(|| Mutex::new(String::new()))
}
pub fn start(description: String) {
    *info().lock().unwrap() = description;
    static START: std::sync::Once = std::sync::Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            if let Err(e) = unsafe { run() } {
                eprintln!("Tray: {e}");
            }
        });
    });
}
unsafe fn run() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    let class = w!("LRD.Host.Tray.v1");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(proc),
        lpszClassName: class,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&wc) } == 0 {
        return Err(windows::core::Error::from_win32().into());
    }
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("LAN Remote Desktop"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )?
    };
    let mut icon = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_APP + 1,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION)? },
        ..Default::default()
    };
    for (i, c) in "LAN Remote Desktop — Ethernet Host"
        .encode_utf16()
        .take(127)
        .enumerate()
    {
        icon.szTip[i] = c;
    }
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &icon) }.as_bool() {
        return Err("could not create tray icon".into());
    }
    lrd_session::updates::service::ready(env!("CARGO_PKG_VERSION"));
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon);
        let _ = DestroyWindow(window);
        CoUninitialize();
    }
    Ok(())
}
unsafe extern "system" fn proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_APP + 2 {
        unsafe {
            if let Ok(label) = GetDlgItem(window, 900) {
                if let Ok(text) = TRANSFER_STATUS
                    .get_or_init(|| Mutex::new(String::new()))
                    .lock()
                {
                    let _ = SetWindowTextW(label, &HSTRING::from(text.as_str()));
                }
            }
        }
        return LRESULT(0);
    }
    if message == WM_DROPFILES {
        unsafe {
            let drop = HDROP(wparam.0 as *mut _);
            let count = DragQueryFileW(drop, u32::MAX, None);
            if count <= 4 {
                for i in 0..count {
                    let len = DragQueryFileW(drop, i, None);
                    if len > 32767 {
                        continue;
                    }
                    let mut path = vec![0u16; len as usize + 1];
                    DragQueryFileW(drop, i, Some(&mut path));
                    let path = String::from_utf16_lossy(&path[..len as usize]);
                    if remote::send_file(path.into()).is_err() {
                        transfer_status("Mac is not connected".into());
                    }
                }
            } else {
                transfer_status("Select at most four items".into());
            }
            DragFinish(drop);
        }
        return LRESULT(0);
    }
    if message == WM_DESTROY
        && TRANSFER_WINDOW.load(std::sync::atomic::Ordering::Acquire) == window.0 as isize
    {
        TRANSFER_WINDOW.store(0, std::sync::atomic::Ordering::Release);
    }
    if message == WM_APP + 1 && (lparam.0 as u32 == WM_RBUTTONUP || lparam.0 as u32 == WM_LBUTTONUP)
    {
        let result = (|| -> Result<()> {
            unsafe {
                let menu = CreatePopupMenu()?;
                for (id, title) in [
                    (1, "接続情報"),
                    (9, "ファイル転送画面を開く…"),
                    (2, "ファイルをMacへ送信…"),
                    (3, "フォルダーをMacへ送信…"),
                    (4, "受信フォルダーを開く"),
                    (5, "転送をキャンセル"),
                    (6, "切断 / 接続猶予を終了"),
                    (7, "新しい接続の許可を切り替え"),
                    (10, "性能ログを書き出す…"),
                    (8, "Hostを終了"),
                ] {
                    AppendMenuW(menu, MF_STRING, id, &HSTRING::from(title))?;
                }
                let mut point = POINT::default();
                GetCursorPos(&mut point)?;
                let _ = SetForegroundWindow(window);
                let id = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY,
                    point.x,
                    point.y,
                    0,
                    window,
                    None,
                )
                .0;
                let _ = DestroyMenu(menu);
                match id {
                    1 => {
                        let text = info().lock().map_err(|_| "tray lock")?.clone();
                        let metrics=lrd_session::diagnostics::latest().map(|s|{
                            let value=|v:Option<f64>|v.map(|v|format!("{v:.1}")).unwrap_or_else(||"—".into());
                            format!("\n\nRTT {} ms / encode {} ms / capture {} fps\nCPU {}% / RAM {} MiB / codec {}\nログ取りこぼし: {}",
                                value(s.rtt_ms),value(s.encode_ms),value(s.capture_fps),value(s.cpu_percent),value(s.resident_bytes.map(|n|n as f64/1048576.)),
                                s.encoder_hardware.map(|b|if b{"GPU"}else{"software"}).unwrap_or("未測定"),lrd_session::diagnostics::dropped_samples())
                        }).unwrap_or_default();
                        MessageBoxW(window,&HSTRING::from(format!("{text}\n\n新規接続: {}\n保存先: Downloads\\LAN Remote Desktop{metrics}",if remote::accepting(){"許可"}else{"停止"})),w!("LAN Remote Desktop"),MB_OK);
                    }
                    2 | 3 => {
                        if let Err(e) = choose(window, id == 3) {
                            MessageBoxW(
                                window,
                                &HSTRING::from(e.to_string()),
                                w!("ファイル転送"),
                                MB_OK | MB_ICONINFORMATION,
                            );
                        }
                    }
                    4 => {
                        let path = lrd_session::transfer::downloads()?;
                        std::fs::create_dir_all(&path)?;
                        ShellExecuteW(
                            window,
                            w!("open"),
                            &HSTRING::from(path.to_string_lossy().as_ref()),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                    9 => {
                        open_transfer()?;
                    }
                    10 => export_logs(window)?,
                    5 => remote::cancel_files(),
                    6 => remote::disconnect(),
                    7 => remote::toggle_accepting(),
                    8 => {
                        remote::shutdown();
                        PostQuitMessage(0);
                    }
                    _ => {}
                }
                Ok(())
            }
        })();
        if let Err(e) = result {
            eprintln!("Tray operation failed: {e}");
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
unsafe fn export_logs(window: HWND) -> Result<()> {
    unsafe {
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_OVERWRITEPROMPT)?;
        dialog.SetFileName(w!("LAN-Remote-Desktop-diagnostics.json"))?;
        dialog.SetDefaultExtension(w!("json"))?;
        if dialog.Show(window).is_err() {
            return Ok(());
        }
        let selected = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = selected.to_string();
        CoTaskMemFree(Some(selected.0.cast()));
        let path = path?;
        std::thread::spawn(move || {
            let result = lrd_session::diagnostics::export(std::path::Path::new(&path));
            let message=match result {Ok(count)=>format!("数値ログを書き出しました（{count}件）。画面・入力・クリップボード本文は含みません。"),Err(_)=>"ログを書き出せませんでした。アプリの保存領域以外の保存先を選択してください。".into()};
            MessageBoxW(
                None,
                &HSTRING::from(message),
                w!("性能ログ"),
                MB_OK | MB_ICONINFORMATION,
            );
        });
        Ok(())
    }
}
unsafe fn choose(window: HWND, folder: bool) -> Result<()> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        let options = FOS_FORCEFILESYSTEM
            | FOS_PATHMUSTEXIST
            | FOS_ALLOWMULTISELECT
            | if folder {
                FOS_PICKFOLDERS
            } else {
                FOS_FILEMUSTEXIST
            };
        dialog.SetOptions(options)?;
        if dialog.Show(window).is_err() {
            return Ok(());
        }
        let results = dialog.GetResults()?;
        let count = results.GetCount()?;
        if count > 4 {
            return Err("一度に4件まで選択してください".into());
        }
        for i in 0..count {
            let path = results.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
            let value = path.to_string()?;
            CoTaskMemFree(Some(path.0.cast()));
            remote::send_file(value.into())?;
        }
    }
    Ok(())
}

unsafe fn open_transfer() -> Result<()> {
    unsafe {
        let existing = TRANSFER_WINDOW.load(std::sync::atomic::Ordering::Acquire);
        if existing != 0 {
            let _ = ShowWindow(HWND(existing as *mut _), SW_SHOWNORMAL);
            let _ = SetForegroundWindow(HWND(existing as *mut _));
            return Ok(());
        }
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("LRD.Host.Tray.v1"),
            w!("LAN Remote Desktop — File Transfer"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            680,
            300,
            None,
            None,
            None,
            None,
        )?;
        CreateWindowExW(WINDOW_EX_STYLE(0),w!("STATIC"),w!("Drop files/folders here to send to the paired Mac.\nUp to four items. Destination: Downloads / LAN Remote Desktop\nReceive folder / cancel / file chooser: right-click the tray icon."),WS_CHILD|WS_VISIBLE,20,20,620,90,window,None,None,None)?;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Ready"),
            WS_CHILD | WS_VISIBLE,
            20,
            130,
            620,
            80,
            window,
            HMENU(900usize as *mut _),
            None,
            None,
        )?;
        TRANSFER_WINDOW.store(window.0 as isize, std::sync::atomic::Ordering::Release);
        DragAcceptFiles(window, true);
        let _ = ShowWindow(window, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(window);
    }
    Ok(())
}
