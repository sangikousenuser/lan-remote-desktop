use std::time::Instant;
#[derive(Clone, Copy, Default)]
pub struct Raw {
    pub cpu_seconds: Option<f64>,
    pub resident_bytes: Option<u64>,
    pub peak_resident_bytes: Option<u64>,
}
#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct Resources {
    pub cpu_percent: Option<f64>,
    pub resident_bytes: Option<u64>,
    pub peak_resident_bytes: Option<u64>,
}
#[derive(Default)]
pub struct Meter {
    previous: Option<(Instant, f64)>,
}
impl Meter {
    pub fn sample(&mut self) -> Resources {
        let raw = raw();
        let now = Instant::now();
        let cpu = raw.cpu_seconds.and_then(|value| {
            let previous = self.previous.replace((now, value));
            previous.and_then(|(at, cpu)| {
                now.checked_duration_since(at)
                    .filter(|dt| dt.as_secs_f64() > 0.)
                    .map(|dt| ((value - cpu).max(0.) / dt.as_secs_f64()) * 100.)
            })
        });
        Resources {
            cpu_percent: cpu.filter(|n| n.is_finite()),
            resident_bytes: raw.resident_bytes,
            peak_resident_bytes: raw.peak_resident_bytes,
        }
    }
}
pub fn raw() -> Raw {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        let mut raw = Raw::default();
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            let usage = unsafe { usage.assume_init() };
            raw.cpu_seconds = Some(
                usage.ru_utime.tv_sec as f64
                    + usage.ru_utime.tv_usec as f64 / 1e6
                    + usage.ru_stime.tv_sec as f64
                    + usage.ru_stime.tv_usec as f64 / 1e6,
            );
            raw.peak_resident_bytes = Some(
                usage.ru_maxrss.max(0) as u64 * if cfg!(target_os = "macos") { 1 } else { 1024 },
            );
        }
        #[cfg(target_os = "macos")]
        {
            #[repr(C)]
            #[derive(Default)]
            struct Basic {
                virtual_size: u64,
                resident_size: u64,
                resident_max: u64,
                user_seconds: i32,
                user_micros: i32,
                system_seconds: i32,
                system_micros: i32,
                policy: i32,
                suspend: i32,
            }
            unsafe extern "C" {
                static mach_task_self_: u32;
                fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
            }
            let mut info = Basic::default();
            let mut count = (std::mem::size_of::<Basic>() / 4) as u32;
            if unsafe {
                task_info(
                    mach_task_self_,
                    20,
                    (&mut info as *mut Basic).cast(),
                    &mut count,
                )
            } == 0
            {
                raw.resident_bytes = Some(info.resident_size);
                raw.peak_resident_bytes = Some(info.resident_max);
            }
        }
        raw
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::FILETIME,
            System::{
                ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
                Threading::{GetCurrentProcess, GetProcessTimes},
            },
        };
        let mut raw = Raw::default();
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let process = unsafe { GetCurrentProcess() };
        if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            .is_ok()
        {
            let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
            raw.cpu_seconds = Some((ticks(kernel) as f64 + ticks(user) as f64) / 1e7);
        }
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let mut memory = PROCESS_MEMORY_COUNTERS {
            cb: size,
            ..Default::default()
        };
        if unsafe { GetProcessMemoryInfo(process, &mut memory, size) }.is_ok() {
            raw.resident_bytes = Some(memory.WorkingSetSize as u64);
            raw.peak_resident_bytes = Some(memory.PeakWorkingSetSize as u64);
        }
        raw
    }
    #[cfg(not(any(unix, windows)))]
    {
        Raw::default()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_process_resource_counters_are_available_without_privilege() {
        let r = super::raw();
        assert!(r.cpu_seconds.unwrap() >= 0.);
        assert!(r.peak_resident_bytes.unwrap() > 0);
        #[cfg(any(windows, target_os = "macos"))]
        assert!(r.resident_bytes.unwrap() > 0);
    }
}
