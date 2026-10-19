#[cfg(target_os = "macos")]
mod agent;
#[cfg(target_os = "macos")]
mod network;

#[cfg(target_os = "macos")]
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--agent") {
        if let Err(e) = agent::run() {
            eprintln!("Agent: {e}");
            std::process::exit(1);
        }
        return;
    }
    if !std::env::args().any(|a| matches!(a.as_str(), "--help" | "--smoke-test" | "--video"))
        && matches!(
            lrd_session::updates::service::bootstrap(env!("CARGO_PKG_VERSION")),
            Ok(true)
        )
    {
        return;
    }
    use std::ffi::CString;
    unsafe extern "C" {
        fn lrd_client_main(argc: i32, argv: *const *const std::ffi::c_char) -> i32;
    }
    let args: Vec<_> = std::env::args()
        .map(|arg| CString::new(arg).expect("NUL in argument"))
        .collect();
    let pointers: Vec<_> = args.iter().map(|arg| arg.as_ptr()).collect();
    // The native entry runs synchronously on the main thread; all C strings outlive it.
    let code = unsafe { lrd_client_main(pointers.len() as i32, pointers.as_ptr()) };
    std::process::exit(code);
}

#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn lrd_startup_ready() {
    if !std::env::args().any(|a| matches!(a.as_str(), "--smoke-test" | "--video")) {
        lrd_session::updates::service::ready(env!("CARGO_PKG_VERSION"));
    }
}

#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn lrd_resource_statistics_json() -> *mut std::ffi::c_char {
    static METER: std::sync::OnceLock<
        std::sync::Mutex<lrd_session::diagnostics::resources::Meter>,
    > = std::sync::OnceLock::new();
    let resource = METER
        .get_or_init(Default::default)
        .lock()
        .map(|mut m| m.sample())
        .unwrap_or_default();
    let value = serde_json::json!({"cpu_percent":resource.cpu_percent,"resident_bytes":resource.resident_bytes,
        "peak_resident_bytes":resource.peak_resident_bytes,"log_dropped_samples":lrd_session::diagnostics::dropped_samples()});
    std::ffi::CString::new(value.to_string()).map_or(std::ptr::null_mut(), |s| s.into_raw())
}
#[cfg(target_os = "macos")]
#[no_mangle]
pub extern "C" fn lrd_record_render_metrics(
    decode_fps: f64,
    render_fps: f64,
    rtt_ms: f64,
    errors: u64,
    dropped: u64,
    sent: u64,
    received: u64,
    hardware: bool,
) {
    lrd_session::diagnostics::record(lrd_session::diagnostics::Sample {
        decode_fps: Some(decode_fps),
        render_fps: Some(render_fps),
        rtt_ms: (rtt_ms > 0.).then_some(rtt_ms),
        decode_errors: Some(errors),
        dropped_frames: Some(dropped),
        sent_bytes: Some(sent),
        received_bytes: Some(received),
        decoder_hardware: Some(hardware),
        ..Default::default()
    });
}
#[cfg(target_os = "macos")]
#[no_mangle]
/// # Safety
/// `path` must point to a NUL-terminated UTF-8 string that remains readable until
/// this call returns. It comes from the user's native Save panel.
pub unsafe extern "C" fn lrd_export_numeric_logs(path: *const std::ffi::c_char) -> bool {
    if path.is_null() {
        return false;
    }
    let Ok(path) = (unsafe { std::ffi::CStr::from_ptr(path) }).to_str() else {
        return false;
    };
    path.len() <= 32768 && lrd_session::diagnostics::export(std::path::Path::new(path)).is_ok()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS Client requires macOS. Build the Windows Host on Windows.");
    std::process::exit(1);
}
