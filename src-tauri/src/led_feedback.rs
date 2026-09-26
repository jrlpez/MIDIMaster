use crate::audio::target_match::{application_name_matches, ApplicationMatchInfo};
use crate::audio::{AudioPeakLevels, AudioBackend};
use crate::device_target::{parse_device_target, DeviceTargetKind};
use crate::model::{Binding, BindingTarget};

pub const LED_ENVELOPE_DECAY: f32 = 0.82;

pub fn scale_led_level(peak: f32, intensity: f32) -> f32 {
    let peak = if peak.is_finite() {
        peak.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let intensity = if intensity.is_finite() {
        intensity.clamp(0.0, 2.0)
    } else {
        1.0
    };
    (peak * intensity).clamp(0.0, 1.0)
}

pub fn envelope_led_level(previous: f32, peak: f32, intensity: f32, decay: f32) -> f32 {
    let next = scale_led_level(peak, intensity);
    let previous = if previous.is_finite() {
        previous.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let decay = if decay.is_finite() {
        decay.clamp(0.0, 1.0)
    } else {
        LED_ENVELOPE_DECAY
    };
    next.max(previous * decay)
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
        BindingTarget::Application { name, .. } => peaks
            .session_matches
            .iter()
            .filter(|(info, _)| {
                application_name_matches(
                    name,
                    ApplicationMatchInfo {
                        process_path: info.process_path.as_deref(),
                        process_name: info.process_name.as_deref(),
                        display_name: Some(info.display_name.as_str()),
                        application_key: info.application_key.as_deref(),
                        ..Default::default()
                    },
                )
            })
            .map(|(_, peak)| *peak)
            .fold(0.0_f32, f32::max),
        BindingTarget::Device { device_id } => {
            let (kind, raw_id) = parse_device_target(device_id);
            match kind {
                DeviceTargetKind::Playback => peaks.devices.get(&raw_id).copied().unwrap_or(0.0),
                DeviceTargetKind::Recording => 0.0,
            }
        }
        _ => 0.0,
    }
}

pub fn reactive_led_level_for_binding(
    binding: &Binding,
    peaks: &AudioPeakLevels,
    previous: f32,
) -> Option<f32> {
    if !binding.uses_audio_reactive_led() {
        return None;
    }
    let mut peak = 0.0_f32;
    for target in binding.normalized_targets_ref() {
        peak = peak.max(peak_for_target(target, peaks));
    }
    Some(envelope_led_level(
        previous,
        peak,
        binding.normalized_led_intensity(),
        LED_ENVELOPE_DECAY,
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
    fn intensity_scales_and_clamps_peak() {
        assert!((scale_led_level(0.4, 1.0) - 0.4).abs() < 0.0001);
        assert!((scale_led_level(0.4, 2.0) - 0.8).abs() < 0.0001);
        assert!((scale_led_level(0.8, 2.0) - 1.0).abs() < 0.0001);
        assert!((scale_led_level(0.1, 0.0) - 0.0).abs() < 0.0001);
    }

    #[test]
    fn envelope_holds_brief_pulse_tail() {
        let first = envelope_led_level(0.0, 1.0, 1.0, 0.8);
        assert!((first - 1.0).abs() < 0.0001);
        let second = envelope_led_level(first, 0.0, 1.0, 0.8);
        assert!((second - 0.8).abs() < 0.0001);
        let third = envelope_led_level(second, 0.1, 1.0, 0.8);
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
        assert!(reactive_led_level_for_binding(&binding, &peaks, 0.0).is_none());

        binding.led_feedback_mode = crate::model::LedFeedbackMode::AudioReactive;
        binding.led_intensity = 2.0;
        let value = reactive_led_level_for_binding(&binding, &peaks, 0.0).expect("reactive");
        assert!((value - 1.0).abs() < 0.0001);
    }
}
