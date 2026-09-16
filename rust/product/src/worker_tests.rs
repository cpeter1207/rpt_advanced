use super::*;
use crate::{abi, services::HostServices};
use rpt_advanced_core::{
    config::ConfigDocument,
    link::LinkAudio,
    media::{FileRequest, MediaError, PreparedAudio, SpeechRequest},
    runtime::{
        DeviceHandoff, GenerationSettings, NativeFilePreparer, NativeSpeechPreparer,
        PreparedAdapter, Runtime, RuntimeClock, RuntimeError,
    },
    schedule::{CivilTime, Weekday},
};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

struct Media;
impl NativeFilePreparer for Media {
    fn file(&self, _: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}
impl NativeSpeechPreparer for Media {
    fn speech(&self, _: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Err(MediaError::Unavailable)
    }
}

struct Device;
impl DeviceHandoff for Device {
    fn quiesce(&mut self) -> bool {
        true
    }
    fn close(&mut self) {}
    fn open(&mut self, _: &GenerationSettings) -> bool {
        true
    }
}

static READY_CALLED: AtomicBool = AtomicBool::new(false);
static EXCHANGE_CALLED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn ready_hangup(_: *mut c_void, _: *mut c_void) -> i32 {
    READY_CALLED.store(true, Ordering::Release);
    -1
}

unsafe extern "C" fn ready_exchange(_: *mut c_void, _: *mut c_void) -> i32 {
    1
}

unsafe extern "C" fn exchange_hangup(
    _: *mut c_void,
    _: *mut c_void,
    _: u64,
    _: abi::rptadv_radio_render_v1,
    _: *mut c_void,
) -> i32 {
    EXCHANGE_CALLED.store(true, Ordering::Release);
    -1
}

fn clock() -> RuntimeClock {
    RuntimeClock {
        now_ms: 0,
        wall_seconds: 0,
        civil: Some((
            CivilTime::new(2030, 1, 1, Weekday::Tuesday, 9, 7).unwrap(),
            0,
        )),
    }
}

fn audio_owners() -> (Runtime<Audio>, AudioOwners) {
    let mut runtime = Runtime::start(
        ConfigDocument::parse("[1000]\nradio_channel=usb\nduplex=full\n").unwrap(),
        &Media,
        |_, _| {
            let (state, _) = LinkAudio::<crate::link::ring::InboundConsumer>::new(Vec::new(), 8)
                .map_err(|_| RuntimeError::Preparation)?;
            Ok(PreparedAdapter {
                state,
                control: (),
                receive_maximum: 8,
                transmit_maximum: 8,
            })
        },
        |_, _| Ok(Box::new(Device) as Box<dyn DeviceHandoff>),
        clock(),
    )
    .unwrap();
    let owners = runtime.node("1000").unwrap().register_audio().unwrap();
    (runtime, owners)
}

fn radio(
    ready: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32>,
    exchange: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            u64,
            abi::rptadv_radio_render_v1,
            *mut c_void,
        ) -> i32,
    >,
) -> Radio {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    if ready.is_some() {
        table.radio_ready = ready;
    }
    if exchange.is_some() {
        table.radio_exchange = exchange;
    }
    let services = unsafe { HostServices::open(Box::leak(Box::new(table))) }.unwrap();
    services.radio("usb", 8).unwrap()
}

fn wait_for(flag: &AtomicBool) {
    for _ in 0..100 {
        if flag.load(Ordering::Acquire) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("worker did not reach the requested callback");
}

#[test]
fn radio_status_preserves_the_original_edge_time() {
    let status = RadioStatus::default();
    assert_eq!(status.snapshot(), (false, 0));

    status.update(false, 1);
    assert_eq!(status.snapshot(), (false, 0));
    status.update(true, 2);
    status.update(true, 3);
    assert_eq!(status.snapshot(), (true, 2));
    status.update(false, u64::MAX);
    assert_eq!(status.snapshot(), (false, u64::MAX >> 1));
}

#[test]
fn worker_test_failures_preserve_or_release_the_reserved_radio() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::fixture::RADIO_DROPS.store(0, Ordering::Relaxed);
    let services =
        unsafe { crate::services::HostServices::open(crate::fixture::host_descriptor()) }.unwrap();

    let radio = services.radio("usb", 8).unwrap();
    RadioWorker::fail_next_for_test(TestFailure::Prepare);
    let Err((Error::Thread, radio)) =
        RadioWorker::prepare(radio, Instant::now(), RadioStatus::default())
    else {
        panic!("injected preparation failure must preserve the radio");
    };
    drop(radio);
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Relaxed), 1);

    let radio = services.radio("usb", 8).unwrap();
    let worker = RadioWorker::terminated_for_test(radio);
    assert!(matches!(worker.stop(), Err(Error::Thread)));
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Relaxed), 2);
}

#[test]
fn render_silences_mismatched_or_invalid_generation_owners() {
    let (mut runtime, mut owners) = audio_owners();
    let mut samples = [0.25_f32; 8];
    assert!(render(&mut owners, true, &mut samples, 0));
    let mut oversized = [0.25_f32; 9];
    assert!(!render(&mut owners, true, &mut oversized, 0));
    assert_eq!(oversized, [0.0; 9]);
    drop(owners);
    assert!(runtime.stop(0));

    let (mut first, (receive, transmit)) = audio_owners();
    let (mut second, (other_receive, other_transmit)) = audio_owners();
    drop(transmit);
    drop(other_receive);
    let mut mismatched = (receive, other_transmit);
    let mut samples = [0.25_f32; 8];
    assert!(!render(&mut mismatched, true, &mut samples, 0));
    assert_eq!(samples, [0.0; 8]);
    drop(mismatched);
    assert!(first.stop(0));
    assert!(second.stop(0));
}

#[test]
fn worker_stops_after_radio_ready_or_exchange_failure() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    READY_CALLED.store(false, Ordering::Release);
    let Ok(mut worker) = RadioWorker::prepare(
        radio(Some(ready_hangup), None),
        Instant::now(),
        RadioStatus::default(),
    ) else {
        panic!("worker preparation must succeed");
    };
    let (mut runtime, owners) = audio_owners();
    assert!(worker.attach(owners).is_ok());
    wait_for(&READY_CALLED);
    let (_, owners) = worker.stop().unwrap();
    drop(owners);
    assert!(runtime.stop(0));

    EXCHANGE_CALLED.store(false, Ordering::Release);
    let Ok(mut worker) = RadioWorker::prepare(
        radio(Some(ready_exchange), Some(exchange_hangup)),
        Instant::now(),
        RadioStatus::default(),
    ) else {
        panic!("worker preparation must succeed");
    };
    let (mut runtime, owners) = audio_owners();
    assert!(worker.attach(owners).is_ok());
    wait_for(&EXCHANGE_CALLED);
    let (_, owners) = worker.stop().unwrap();
    drop(owners);
    assert!(runtime.stop(0));
}

#[test]
fn worker_attach_failure_and_drop_paths_return_or_release_owners() {
    let _serial = crate::fixture::LIFECYCLE
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    let (mut runtime, owners) = audio_owners();
    let mut worker = RadioWorker::disconnected_for_test(radio(None, None));
    let owners = worker.attach(owners).unwrap_err();
    let (_, returned) = worker.stop().unwrap();
    assert!(returned.is_none());
    drop(owners);
    assert!(runtime.stop(0));

    crate::fixture::RADIO_DROPS.store(0, Ordering::Relaxed);
    let Ok(worker) =
        RadioWorker::prepare(radio(None, None), Instant::now(), RadioStatus::default())
    else {
        panic!("worker preparation must succeed");
    };
    drop(worker);
    assert_eq!(crate::fixture::RADIO_DROPS.load(Ordering::Relaxed), 1);
}
