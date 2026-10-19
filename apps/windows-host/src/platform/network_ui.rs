//! Local selection and UAC. The interactive Host never gains administrator rights.
use super::{remote, Result};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::*,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};
fn helper() -> Result<PathBuf> {
    let root = std::env::current_exe()?
        .parent()
        .ok_or("missing app parent")?
        .to_path_buf();
    let versioned = root.join(format!(
        "lrd-network-helper-{}.exe",
        env!("CARGO_PKG_VERSION")
    ));
    let path = if versioned.is_file() {
        versioned
    } else {
        root.join("lrd-network-helper.exe")
    };
    if !path.is_file() {
        return Err("IP設定helperがありません。配布フォルダーから起動してください。".into());
    }
    Ok(path)
}
fn status() -> Result<serde_json::Value> {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new(helper()?)
        .arg("--status")
        .creation_flags(0x08000000)
        .output()?;
    if !output.status.success() || output.stdout.len() > 4096 {
        return Err("helperの状態を読み取れません".into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}
pub fn offer_recovery() {
    let Ok(value) = status() else {
        return;
    };
    let phase = value["phase"].as_str().unwrap_or("");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if matches!(phase, "Active" | "Starting" | "Restoring" | "Deferred")
        && now.saturating_sub(value["updated"].as_u64().unwrap_or(0)) > 30
    {
        unsafe {
            if MessageBoxW(None,w!("前回の一時固定IPの状態が残っています。設定画面でOS承認後に照合・復元しますか？"),w!("Ethernetの復元"),MB_YESNO|MB_ICONQUESTION)==IDYES{let _=wizard(HWND::default());}
        }
    }
}
pub fn wizard(window: HWND) -> Result<()> {
    let response = unsafe {
        MessageBoxW(window,w!("DHCP／Link-Localで接続できない場合にだけ使用します。\n\nはい: 選択Ethernetへ10.254.240.1/30を一時追加\nいいえ: 前回の一時IPを復元／権限を終了\nキャンセル: 閉じる\n\nMac側でも10.254.240.2/30を設定してください。既存アドレス、Wi-Fi、DNS、デフォルトルートは保持します。抜線・取消・12時間後に一時IPを除去します。設定・復元はWindowsのUAC承認を使います。"),w!("有線LANの一時固定IP"),MB_YESNOCANCEL|MB_ICONINFORMATION)
    };
    let restore = match response {
        IDYES => false,
        IDNO => true,
        _ => return Ok(()),
    };
    let mut selected = None;
    if !restore {
        for adapter in lrd_session::network::ethernet_adapters()
            .into_iter()
            .filter(|a| a.active)
        {
            let answer = unsafe {
                MessageBoxW(window,&HSTRING::from(format!("{} ({})\nMAC: {}\n現在のIPv4: {:?}\n\nこの物理Ethernetへ一時IPを追加しますか？\nいいえで次のNIC、キャンセルで終了します。",adapter.label,adapter.index,adapter.mac,adapter.addresses)),w!("Ethernetを選択"),MB_YESNOCANCEL|MB_ICONQUESTION)
            };
            if answer == IDCANCEL {
                return Ok(());
            }
            if answer == IDYES {
                selected = Some(adapter);
                break;
            }
        }
        if selected.is_none() {
            unsafe {
                MessageBoxW(
                    window,
                    w!("接続中の物理Ethernetが選択されていません。"),
                    w!("一時固定IP"),
                    MB_OK,
                );
            }
            return Ok(());
        }
    }
    let arguments = if let Some(adapter) = &selected {
        format!(
            "--apply --interface {} --mac {}",
            adapter.index, adapter.mac
        )
    } else {
        "--restore".into()
    };
    remote::disconnect();
    let executable = helper()?;
    let result = unsafe {
        ShellExecuteW(
            window,
            w!("runas"),
            &HSTRING::from(executable.to_string_lossy().as_ref()),
            &HSTRING::from(arguments),
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        return Err("UACが取り消されたか、IP設定helperを起動できませんでした。".into());
    }
    let start = Instant::now();
    loop {
        if let Ok(value) = status() {
            let phase = value["phase"].as_str().unwrap_or("");
            let ready = if restore {
                phase == "Idle"
            } else {
                phase == "Active"
                    && selected.as_ref().is_some_and(|a| {
                        lrd_session::network::ethernet_adapters().iter().any(|n| {
                            n.mac == a.mac
                                && n.addresses
                                    .contains(&(std::net::Ipv4Addr::new(10, 254, 240, 1), 30))
                        })
                    })
            };
            if ready {
                unsafe {
                    MessageBoxW(
                        window,
                        if restore {
                            w!("一時IPを復元しました。helperの権限は終了しています。")
                        } else {
                            w!("Windows側の一時IPを設定しました。Mac側も設定したら探索してください。")
                        },
                        w!("一時固定IP"),
                        MB_OK,
                    );
                }
                return Ok(());
            }
            if matches!(phase, "Failed" | "Deferred") {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(30) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    unsafe {
        MessageBoxW(window,w!("一時IPを設定・復元できませんでした。衝突、既存IPv4／経路、選択NICを確認してください。手動変更を検出した場合は復元を保留します。"),w!("一時固定IP"),MB_OK|MB_ICONINFORMATION);
    }
    Ok(())
}
