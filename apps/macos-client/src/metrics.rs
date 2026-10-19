use lrd_session::{
    diagnostics::{self, Sample},
    quality::Feedback,
};
#[no_mangle]
/// # Safety
/// The caller supplies `length` readable JSON bytes. Free the owned result
/// using `lrd_free_string`.
pub unsafe extern "C" fn lrd_choose_display_json(
    bytes: *const u8,
    length: usize,
) -> *mut std::ffi::c_char {
    if bytes.is_null() || length > 32768 {
        return std::ptr::null_mut();
    }
    let result = serde_json::from_slice::<lrd_session::display_choice::Client>(unsafe {
        std::slice::from_raw_parts(bytes, length)
    })
    .ok()
    .and_then(|client| lrd_session::display_choice::choose(&client));
    let Some(mode) = result else {
        return std::ptr::null_mut();
    };
    serde_json::to_string(&mode)
        .ok()
        .and_then(|s| std::ffi::CString::new(s).ok())
        .map_or(std::ptr::null_mut(), |s| s.into_raw())
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
    interval_ms: u32,
    decoded: u32,
    presented: u32,
    decode_ms: f64,
    render_ms: f64,
    render_gpu_ms: Option<f64>,
    errors: u64,
    dropped: u64,
    errors_delta: u32,
    dropped_delta: u32,
    received_delta: u64,
    sent: u64,
    received: u64,
    hardware: bool,
    visible: bool,
    rtt_ms: f64,
}
#[no_mangle]
/// # Safety
/// The native caller provides `length` readable bytes until this call returns.
pub unsafe extern "C" fn lrd_record_metrics_json(bytes: *const u8, length: usize) {
    if bytes.is_null() || length > 4096 {
        return;
    }
    let Ok(window) =
        serde_json::from_slice::<Window>(unsafe { std::slice::from_raw_parts(bytes, length) })
    else {
        return;
    };
    let feedback = Feedback {
        epoch: 1,
        interval_ms: window.interval_ms,
        decoded: window.decoded,
        presented: window.presented,
        decode_ms: window.decode_ms,
        render_ms: window.render_ms,
        errors: window.errors_delta,
        dropped: window.dropped_delta,
        visible: window.visible,
    };
    if !feedback.valid() {
        return;
    }
    static FILES: std::sync::OnceLock<std::sync::Mutex<diagnostics::traffic::Meter>> =
        std::sync::OnceLock::new();
    let (file_send, file_receive) = FILES
        .get_or_init(Default::default)
        .lock()
        .map(|mut m| m.sample())
        .unwrap_or_default();
    let seconds = window.interval_ms as f64 / 1000.;
    let sample = Sample {
        decode_fps: Some(window.decoded as f64 / seconds),
        render_fps: Some(window.presented as f64 / seconds),
        rtt_ms: (window.rtt_ms > 0.).then_some(window.rtt_ms),
        decode_ms: Some(window.decode_ms),
        render_ms: Some(window.render_ms),
        render_gpu_ms: window.render_gpu_ms,
        decode_errors: Some(window.errors),
        dropped_frames: Some(window.dropped),
        transport_bitrate_bps: Some((window.received_delta as f64 * 8. / seconds) as u64),
        sent_bytes: Some(window.sent),
        received_bytes: Some(window.received),
        decoder_hardware: Some(window.hardware),
        file_send_bytes_per_second: Some(file_send),
        file_receive_bytes_per_second: Some(file_receive),
        reconnects: Some(crate::network::reconnects()),
        ..Default::default()
    };
    if sample.validate().is_ok() {
        diagnostics::record(sample);
        crate::network::feedback(feedback);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_window_requires_integer_interval_and_rejects_non_numeric_fields() {
        let mut value = serde_json::json!({"interval_ms":500,"decoded":30,"presented":30,
            "decode_ms":2.,"render_ms":1.,"render_gpu_ms":null,"errors":0,"dropped":0,
            "errors_delta":0,"dropped_delta":0,"received_delta":100000,"sent":42,"received":100000,
            "hardware":true,"visible":true,"rtt_ms":1.});
        assert!(serde_json::from_value::<Window>(value.clone()).is_ok());
        value["interval_ms"] = serde_json::json!(500.5);
        assert!(serde_json::from_value::<Window>(value.clone()).is_err());
        value["interval_ms"] = serde_json::json!(500);
        value["keyboard"] = serde_json::json!("must not log");
        assert!(serde_json::from_value::<Window>(value).is_err());
    }
}
