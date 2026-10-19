use super::{Result, Runtime};
use lrd_session::{
    channel::Sender,
    wire::{Packet, AUDIO},
};
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::Win32::{
    Media::{
        Audio::*, KernelStreaming::KSDATAFORMAT_SUBTYPE_PCM, MediaFoundation::MFAudioFormat_Float,
    },
    System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER},
};
pub fn start(sender: Sender, mode: Arc<AtomicU8>) {
    std::thread::spawn(move || {
        let Ok(_runtime) = Runtime::initialize() else {
            return;
        };
        while !sender.stopped() {
            if capture(&sender, &mode).is_err() {
                eprintln!("システム音声を再初期化しています");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}
fn capture(sender: &Sender, mode: &AtomicU8) -> Result<()> {
    unsafe {
        let devices: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let selected_device = device_id(&device)?;
        let client: IAudioClient = device.Activate(CLSCTX_INPROC_SERVER, None)?;
        let pointer = client.GetMixFormat()?;
        if pointer.is_null() {
            return Err("audio format unavailable".into());
        }
        let format = pointer.read_unaligned();
        if !matches!(format.wFormatTag, 1 | 3 | 0xfffe)
            || (format.wFormatTag == 0xfffe && format.cbSize < 22)
        {
            CoTaskMemFree(Some(pointer.cast()));
            return Err("unsupported loopback format header".into());
        }
        let mut float = format.wFormatTag == 3;
        if format.wFormatTag == 0xfffe {
            let ext = pointer.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned();
            let subtype = ext.SubFormat;
            if subtype == MFAudioFormat_Float {
                float = true;
            } else if subtype != KSDATAFORMAT_SUBTYPE_PCM {
                CoTaskMemFree(Some(pointer.cast()));
                return Err("unsupported loopback format".into());
            }
        }
        let rate = format.nSamplesPerSec;
        let channels = format.nChannels as usize;
        let bits = format.wBitsPerSample as usize;
        if !(8000..=192000).contains(&rate)
            || !(1..=8).contains(&channels)
            || !matches!(bits, 16 | 24 | 32)
            || (float && bits != 32)
            || format.nBlockAlign as usize != channels * (bits / 8)
        {
            CoTaskMemFree(Some(pointer.cast()));
            return Err("unsupported loopback PCM".into());
        }
        let initialized = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000,
            0,
            pointer,
            None,
        );
        CoTaskMemFree(Some(pointer.cast()));
        initialized?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        struct Stop(IAudioClient);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = unsafe { self.0.Stop() };
            }
        }
        let _stop = Stop(client);
        let mut device_checked = Instant::now();
        while !sender.stopped() {
            if device_checked.elapsed() >= Duration::from_secs(1) {
                let current = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
                if device_id(&current)? != selected_device {
                    return Ok(());
                }
                device_checked = Instant::now();
            }
            while capture.GetNextPacketSize()? > 0 {
                let mut pointer = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                let mut position = 0;
                capture.GetBuffer(
                    &mut pointer,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    None,
                )?;
                if frames > 192000 {
                    let _ = capture.ReleaseBuffer(frames);
                    return Err("audio capture buffer limit".into());
                }
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || pointer.is_null();
                let data = if silent {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(pointer, frames as usize * channels * (bits / 8))
                };
                let batch = (rate / 100).clamp(80, 1920) as usize;
                for first in (0..frames as usize).step_by(batch) {
                    let count = batch.min(frames as usize - first);
                    let mut pcm = Vec::with_capacity(13 + count * 8);
                    pcm.extend(rate.to_be_bytes());
                    pcm.push(2);
                    pcm.extend((position + first as u64).to_be_bytes());
                    for frame in first..first + count {
                        let sample = |c: usize| -> f32 {
                            if silent || c >= channels {
                                return 0.0;
                            }
                            let i = (frame * channels + c) * (bits / 8);
                            let value = if float {
                                f32::from_le_bytes(data[i..i + 4].try_into().unwrap())
                            } else {
                                match bits {
                                    16 => {
                                        i16::from_le_bytes(data[i..i + 2].try_into().unwrap())
                                            as f32
                                            / 32768.0
                                    }
                                    24 => {
                                        ((data[i] as i32)
                                            | ((data[i + 1] as i32) << 8)
                                            | ((data[i + 2] as i32) << 16))
                                            .wrapping_shl(8)
                                            as f32
                                            / 2147483648.0
                                    }
                                    _ => {
                                        i32::from_le_bytes(data[i..i + 4].try_into().unwrap())
                                            as f32
                                            / 2147483648.0
                                    }
                                }
                            };
                            if value.is_finite() {
                                value.clamp(-1.0, 1.0)
                            } else {
                                0.0
                            }
                        };
                        let mut left = sample(0);
                        let mut right = if channels == 1 { left } else { sample(1) };
                        if channels > 2 {
                            left += sample(2) * 0.707;
                            right += sample(2) * 0.707;
                        }
                        if channels > 3 {
                            left += sample(3) * 0.25;
                            right += sample(3) * 0.25;
                        }
                        if channels > 4 {
                            left += sample(4) * 0.5;
                        }
                        if channels > 5 {
                            right += sample(5) * 0.5;
                        }
                        if channels > 6 {
                            left += sample(6) * 0.5;
                        }
                        if channels > 7 {
                            right += sample(7) * 0.5;
                        }
                        pcm.extend(left.clamp(-1.0, 1.0).to_le_bytes());
                        pcm.extend(right.clamp(-1.0, 1.0).to_le_bytes());
                    }
                    if mode.load(Ordering::Acquire) != 0 {
                        let _ = sender.send(Packet {
                            kind: AUDIO,
                            payload: pcm,
                        });
                    }
                }
                capture.ReleaseBuffer(frames)?;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}
fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe {
        let pointer = device.GetId()?;
        if pointer.is_null() {
            return Err("audio endpoint identity unavailable".into());
        }
        let id = pointer.to_string();
        CoTaskMemFree(Some(pointer.0.cast()));
        Ok(id?)
    }
}
