use super::*;
use crate::site::{Observation, ReadingQuality};
use crate::{config::resources::candidate::ResourceKind, station_fixture::equipment};
use serde_json::{Value, json};

fn settings(extra: Value) -> crate::config::AlarmSettings {
    let mut fields = json!({"enabled":true,"source":"supply","condition":"above","trip_value":14,"clear_value":13});
    json_patch::merge(&mut fields, &extra);
    equipment(&[
        (ResourceKind::Sensor, "supply", json!({})),
        (ResourceKind::Input, "door", json!({})),
        (ResourceKind::Alarm, "warning", fields),
    ])
    .alarms
    .remove(0)
}
fn sample(value: Option<AlarmValue>) -> Observation<AlarmValue> {
    Observation {
        value,
        observed_at_ms: Some(0),
        quality: if value.is_some() {
            ReadingQuality::Valid
        } else {
            ReadingQuality::Unavailable
        },
    }
}
fn analog(value: f64) -> Observation<AlarmValue> {
    sample(Some(AlarmValue::Number(value)))
}

#[test]
fn site_alarm_transition_and_unknown() {
    for (extra, safe, trip, clear) in [
        (
            json!({}),
            AlarmValue::Number(13.96),
            AlarmValue::Number(14.0),
            AlarmValue::Number(13.0),
        ),
        (
            json!({"condition":"below","trip_value":11,"clear_value":12}),
            AlarmValue::Number(11.1),
            AlarmValue::Number(11.0),
            AlarmValue::Number(12.0),
        ),
        (
            json!({"source":"door","condition":"active"}),
            AlarmValue::Digital(false),
            AlarmValue::Digital(true),
            AlarmValue::Digital(false),
        ),
        (
            json!({"source":"door","condition":"inactive"}),
            AlarmValue::Digital(true),
            AlarmValue::Digital(false),
            AlarmValue::Digital(true),
        ),
    ] {
        let mut state = AlarmState::new(settings({
            let mut fields = extra;
            fields["timing"] = json!({"trip_delay_ms":100,"clear_delay_ms":100});
            fields
        }));
        assert!(state.advance(0, &sample(Some(safe))).is_empty());
        assert!(state.advance(10, &sample(Some(trip))).is_empty());
        assert!(state.advance(50, &sample(None)).is_empty());
        assert!(state.advance(100, &sample(Some(trip))).is_empty());
        assert!(state.advance(199, &sample(Some(trip))).is_empty());
        assert_eq!(
            state.advance(200, &sample(Some(trip))),
            vec![AlarmEvent::Trip]
        );
        assert!(state.advance(300, &sample(None)).is_empty());
        assert!(state.snapshot().active);
        assert!(state.advance(400, &sample(Some(clear))).is_empty());
        assert!(state.advance(450, &sample(None)).is_empty());
        assert!(state.advance(500, &sample(Some(clear))).is_empty());
        assert_eq!(
            state.advance(600, &sample(Some(clear))),
            vec![AlarmEvent::Clear]
        );
    }
    let mut unavailable = AlarmState::new(settings(json!({"condition":"unavailable"})));
    assert_eq!(
        unavailable.advance(0, &sample(None)),
        vec![AlarmEvent::Trip]
    );
    assert_eq!(
        unavailable.advance(1, &analog(12.0)),
        vec![AlarmEvent::Clear]
    );
}

#[test]
fn site_alarm_ack_reset_repeat() {
    let mut state = AlarmState::new(settings(
        json!({"latched":true,"timing":{"repeat_interval_ms":100,"clear_delay_ms":20}}),
    ));
    assert_eq!(state.advance(0, &analog(15.0)), vec![AlarmEvent::Trip]);
    assert!(state.advance(99, &analog(15.0)).is_empty());
    assert_eq!(state.advance(100, &analog(15.0)), vec![AlarmEvent::Repeat]);
    state.acknowledge();
    assert!(state.advance(200, &analog(15.0)).is_empty());
    assert!(state.reset().is_err());
    assert!(state.advance(300, &analog(12.0)).is_empty());
    assert!(state.reset().is_err());
    assert!(state.advance(320, &analog(12.0)).is_empty());
    assert_eq!(state.reset().unwrap(), Some(AlarmEvent::Clear));
    assert!(!state.snapshot().active);
    assert_eq!(state.advance(400, &analog(15.0)), vec![AlarmEvent::Trip]);
    assert!(!state.snapshot().acknowledged);
}

#[test]
fn site_alarm_disabled_or_invalid_observations_cannot_trip_or_clear() {
    let mut disabled = AlarmState::new(settings(json!({"enabled":false})));
    assert!(disabled.advance(0, &analog(100.0)).is_empty());
    assert!(!disabled.snapshot().active);
    assert_eq!(disabled.reset(), Ok(None));
    for (extra, trip) in [
        (json!({}), 15.0),
        (
            json!({"condition":"below","trip_value":11,"clear_value":12}),
            10.0,
        ),
    ] {
        let mut state = AlarmState::new(settings(extra));
        assert_eq!(state.advance(0, &analog(trip)), vec![AlarmEvent::Trip]);
        for invalid in [AlarmValue::Number(f64::NAN), AlarmValue::Digital(false)] {
            assert!(state.advance(1, &sample(Some(invalid))).is_empty());
            assert!(state.snapshot().active);
            assert_eq!(state.snapshot().unsafe_condition, None);
            assert_eq!(state.reset(), Err(SiteError::Unsafe));
        }
    }
}
