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

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macOS Client requires macOS. Build the Windows Host on Windows.");
    std::process::exit(1);
}
