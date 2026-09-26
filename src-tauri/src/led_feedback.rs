use crate::audio::target_match::{application_name_matches, ApplicationMatchInfo};
use crate::audio::{AudioBackend, AudioPeakLevels};
use crate::device_target::{parse_device_target, DeviceTargetKind};
use crate::model::{Binding, BindingTarget};

/// Decay applied to the previous LED level so flashes have a short visible tail.
pub const LED_ENVELOPE_DECAY: f32 = 0.78;
/// How quickly the adaptive baseline tracks steady metering (per ~40ms tick).
pub const LED_BASELINE_ALPHA: f32 = 0.08;

#[derive(Debug, Clone, Copy, Default)]
pub struct ReactiveLedState {
    pub level: f32,
    pub baseline: f32,
}

pub fn normalize_intensity(intensity: f32) -> f32 {
    if intensity.is_finite() {
        intensity.clamp(0.0, 2.0)
    } else {
        1.0
    }
}

pub fn normalize_peak(peak: f32) -> f32 {
    if peak.is_finite() {
        peak.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Map sensitivity (0..=2) onto a shaped LED drive for a single peak sample.
///
/// Higher sensitivity:
/// - lifts mid/quiet peaks with a softer gamma curve
/// - amplifies short rises above a slow baseline so steady metering still flashes
pub fn scale_led_level(peak: f32, intensity: f32) -> f32 {
    let peak = normalize_peak(peak);
    let intensity = normalize_intensity(intensity);
    if intensity <= 0.0 || peak <= 0.0 {
        return 0.0;
    }
    // intensity 1 → near-linear; 2 → strong lift of quieter peaks; low → darker.
    let gamma = (1.15 / intensity.max(0.12)).clamp(0.32, 3.8);
    let shaped = peak.powf(gamma);
    let gain = 0.55 + intensity * 0.85;
    (shaped * gain).clamp(0.0, 1.0)
}

/// Adaptive response: absolute shaped level + punch for peaks above the rolling baseline.
pub fn sensitive_led_drive(peak: f32, baseline: f32, intensity: f32) -> (f32, f32) {
    let peak = normalize_peak(peak);
    let intensity = normalize_intensity(intensity);
    let baseline = normalize_peak(baseline);

    let next_baseline = if intensity <= 0.0 {
        0.0
    } else if baseline <= 0.001 {
        peak
    } else {
        baseline + (peak - baseline) * LED_BASELINE_ALPHA
    };

    if intensity <= 0.0 {
        return (0.0, next_baseline);
    }

    let absolute = scale_led_level(peak, intensity);
    // Excess above the recent average — this is what makes steady loud music still pulse.
    let delta = (peak - next_baseline).max(0.0);
    // Punch grows with the slider: ~100% is moderate, ~200% catches small mid-meter bumps.
    let punch_gain = 1.0 + intensity * 2.5 + intensity * intensity * 3.5;
    let punch = (delta * punch_gain).clamp(0.0, 1.0);
    // Blend: higher sensitivity leans more on punch so micro-dynamics dominate.
    let punch_mix = (0.3 + intensity * 0.35).clamp(0.3, 1.0);
    let driven = (absolute * (1.0 - punch_mix * 0.5) + punch * punch_mix).clamp(0.0, 1.0);
    (driven, next_baseline)
}

pub fn envelope_led_level(previous: f32, drive: f32, decay: f32) -> f32 {
    let drive = normalize_peak(drive);
    let previous = normalize_peak(previous);
    let decay = if decay.is_finite() {
        decay.clamp(0.0, 1.0)
    } else {
        LED_ENVELOPE_DECAY
    };
    drive.max(previous * decay)
}

pub fn peak_for_target(target: &BindingTarget, peaks: &AudioPeakLevels) -> f32 {
    match target {
        BindingTarget::Master => peaks.master,
        BindingTarget::Focus => peaks
            .focused_session_id
            .as_ref()
            .and_then(|id| peaks.sessions.get(id).copied())
            .unwrap_or(0.0),
        BindingTarget::Session { session_id } => {
            peaks.sessions.get(session_id).copied().unwrap_or(0.0)
        }
        BindingTarget::Application { name, .. } => {
            let mut peak = 0.0_f32;
            for (info, session_peak) in &peaks.session_matches {
                if application_name_matches(
                    name,
                    ApplicationMatchInfo {
                        process_path: info.process_path.as_deref(),
                        process_name: info.process_name.as_deref(),
                        display_name: Some(info.display_name.as_str()),
                        application_key: info.application_key.as_deref(),
                        friendly_process_label: None,
                        humanized_process_name: None,
                        package_family_name: None,
                        package_full_name: None,
                        application_user_model_id: None,
                    },
                ) {
                    peak = peak.max(*session_peak);
                }
            }
            peak
        }
        BindingTarget::Device { device_id } => {
            let (kind, raw_id) = parse_device_target(device_id);
            match kind {
                DeviceTargetKind::Playback => peaks.devices.get(raw_id).copied().unwrap_or(0.0),
                DeviceTargetKind::Recording => 0.0,
            }
        }
        _ => 0.0,
    }
}

pub fn reactive_led_level_for_binding(
    binding: &Binding,
    peaks: &AudioPeakLevels,
    state: ReactiveLedState,
) -> Option<(f32, ReactiveLedState)> {
    if !binding.uses_audio_reactive_led() {
        return None;
    }
    let mut peak = 0.0_f32;
    for target in binding.normalized_targets_ref() {
        peak = peak.max(peak_for_target(target, peaks));
    }
    let intensity = binding.normalized_led_intensity();
    let (drive, baseline) = sensitive_led_drive(peak, state.baseline, intensity);
    let level = envelope_led_level(state.level, drive, LED_ENVELOPE_DECAY);
    Some((
        level,
        ReactiveLedState {
            level,
            baseline,
        },
    ))
}

pub fn collect_audio_peaks(audio: &dyn AudioBackend) -> AudioPeakLevels {
    audio.peak_levels().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BindingAction, BindingTarget};
    use crate::test_support::binding;
    use std::collections::HashMap;

    #[test]
    fn intensity_lifts_mid_peaks_non_linearly() {
        let linearish = scale_led_level(0.35, 1.0);
        let sensitive = scale_led_level(0.35, 2.0);
        assert!(sensitive > linearish + 0.08);
        assert!((scale_led_level(0.1, 0.0) - 0.0).abs() < 0.0001);
        assert!(scale_led_level(0.8, 2.0) >= 0.95);
    }

    #[test]
    fn sensitivity_amplifies_rises_above_steady_meter() {
        // Steady metering around 0.45, then a small bump — high sensitivity should flash harder.
        let baseline = 0.45;
        let bump = 0.52;
        let (_, next_base_low) = sensitive_led_drive(bump, baseline, 0.5);
        let (low, _) = sensitive_led_drive(bump, baseline, 0.5);
        let (high, next_base_high) = sensitive_led_drive(bump, baseline, 2.0);
        assert!(high > low + 0.2);
        assert!(next_base_low > 0.0);
        assert!(next_base_high > 0.0);
    }

    #[test]
    fn envelope_holds_brief_pulse_tail() {
        let first = envelope_led_level(0.0, 1.0, 0.8);
        assert!((first - 1.0).abs() < 0.0001);
        let second = envelope_led_level(first, 0.0, 0.8);
        assert!((second - 0.8).abs() < 0.0001);
        let third = envelope_led_level(second, 0.1, 0.8);
        assert!((third - 0.64).abs() < 0.0001);
    }

    #[test]
    fn peak_for_master_and_application_targets() {
        let peaks = AudioPeakLevels {
            master: 0.55,
            devices: HashMap::new(),
            sessions: HashMap::from([("chrome".into(), 0.2)]),
            focused_session_id: None,
            session_matches: vec![(
                crate::audio::SessionPeakIdentity {
                    display_name: "Chrome".into(),
                    application_key: Some("chrome".into()),
                    process_name: Some("chrome.exe".into()),
                    process_path: None,
                },
                0.7,
            )],
        };
        assert!((peak_for_target(&BindingTarget::Master, &peaks) - 0.55).abs() < 0.0001);
        assert!(
            (peak_for_target(
                &BindingTarget::Application {
                    name: "chrome".into(),
                    display_name: None,
                    icon_data: None,
                },
                &peaks
            ) - 0.7)
                .abs()
                < 0.0001
        );
    }

    #[test]
    fn reactive_level_respects_mode_and_intensity() {
        let mut binding = binding();
        binding.action = BindingAction::Volume;
        binding.targets = vec![BindingTarget::Master];
        binding.led_feedback_mode = crate::model::LedFeedbackMode::FollowValue;
        let peaks = AudioPeakLevels {
            master: 0.5,
            ..AudioPeakLevels::default()
        };
        assert!(reactive_led_level_for_binding(&binding, &peaks, ReactiveLedState::default()).is_none());

        binding.led_feedback_mode = crate::model::LedFeedbackMode::AudioReactive;
        binding.led_intensity = 2.0;
        let (value, state) =
            reactive_led_level_for_binding(&binding, &peaks, ReactiveLedState::default())
                .expect("reactive");
        assert!(value > 0.4);
        assert!(state.baseline > 0.0);
    }
}
