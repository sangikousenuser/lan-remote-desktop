//! Select only advertised Windows modes, bounded by display size and measured
//! decode/render cost. The starting estimate does not guarantee GPU performance.
use crate::wire::DisplayMode;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub physical_width: u32,
    pub physical_height: u32,
    pub logical_width: u32,
    pub logical_height: u32,
    pub current_width: u32,
    pub current_height: u32,
    pub refresh_hz: f64,
    pub decode_ms: f64,
    pub render_ms: f64,
    pub hardware: bool,
    pub modes: Vec<DisplayMode>,
}
pub fn choose(client: &Client) -> Option<DisplayMode> {
    let sizes = [
        client.physical_width,
        client.physical_height,
        client.logical_width,
        client.logical_height,
        client.current_width,
        client.current_height,
    ];
    if sizes.iter().any(|size| !(1..=16384).contains(size))
        || client.modes.len() > 128
        || ![client.refresh_hz, client.decode_ms, client.render_ms]
            .iter()
            .all(|v| v.is_finite() && (0.0..=10000.).contains(v))
    {
        return None;
    }
    let fps = if client.refresh_hz >= 1. {
        client.refresh_hz.clamp(30., 60.)
    } else {
        60.
    };
    let current_pixels = client.current_width as f64 * client.current_height as f64;
    let physical_pixels = client.physical_width as f64 * client.physical_height as f64;
    let measured = current_pixels * (700. / fps) / client.decode_ms.max(client.render_ms).max(0.05);
    let ceiling = if client.hardware {
        3840. * 2160.
    } else {
        (client.logical_width as f64 * client.logical_height as f64).min(1920. * 1080.)
    };
    let pixel_budget = measured.min(physical_pixels).min(ceiling).max(640. * 480.);
    let aspect = client.physical_width as f64 / client.physical_height as f64;
    let width = (pixel_budget * aspect).sqrt();
    let height = (pixel_budget / aspect).sqrt();
    let score = |mode: &DisplayMode| {
        let pixels = mode.width as f64 * mode.height as f64;
        let excess = (pixels / pixel_budget - 1.).max(0.) * 4.;
        let aspect_error = ((mode.width as f64 / mode.height as f64) / aspect)
            .ln()
            .abs();
        let distance =
            (mode.width as f64 / width).ln().abs() + (mode.height as f64 / height).ln().abs();
        excess + 3. * aspect_error + distance + ((fps - mode.hz as f64) / fps).max(0.) * 0.5
    };
    client
        .modes
        .iter()
        .filter(|mode| mode.valid())
        .min_by(|a, b| {
            score(a)
                .total_cmp(&score(b))
                .then_with(|| b.hz.min(60).cmp(&a.hz.min(60)))
        })
        .cloned()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn client() -> Client {
        Client {
            physical_width: 3840,
            physical_height: 2160,
            logical_width: 1920,
            logical_height: 1080,
            current_width: 3840,
            current_height: 2160,
            refresh_hz: 60.,
            decode_ms: 2.,
            render_ms: 2.,
            hardware: true,
            modes: vec![
                DisplayMode {
                    width: 1280,
                    height: 720,
                    hz: 60,
                },
                DisplayMode {
                    width: 1920,
                    height: 1080,
                    hz: 60,
                },
                DisplayMode {
                    width: 3840,
                    height: 2160,
                    hz: 60,
                },
            ],
        }
    }
    #[test]
    fn fast_hardware_uses_physical_retina_and_slow_decode_selects_supported_lower_mode() {
        let mut c = client();
        assert_eq!(choose(&c).unwrap().width, 3840);
        c.decode_ms = 40.;
        assert_eq!(choose(&c).unwrap().width, 1920);
        c.hardware = false;
        c.decode_ms = 2.;
        assert_eq!(choose(&c).unwrap().width, 1920);
        assert!(c.modes.contains(&choose(&c).unwrap()));
    }
    #[test]
    fn respects_aspect_advertised_modes_and_rejects_invalid_capabilities() {
        let mut c = client();
        c.physical_width = 1920;
        c.physical_height = 1200;
        c.modes.push(DisplayMode {
            width: 1920,
            height: 1200,
            hz: 60,
        });
        assert_eq!(choose(&c).unwrap().height, 1200);
        c.decode_ms = f64::NAN;
        assert!(choose(&c).is_none());
        c.decode_ms = 2.;
        c.modes.clear();
        assert!(choose(&c).is_none());
        c.modes.push(DisplayMode {
            width: 99999,
            height: 1080,
            hz: 60,
        });
        assert!(choose(&c).is_none());
    }
}
