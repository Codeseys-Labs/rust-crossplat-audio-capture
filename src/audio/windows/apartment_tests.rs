use super::wasapi::{
    enumerate_application_audio_sessions, ComInitializer, WindowsDeviceEnumerator,
};
use crate::core::error::AudioError;
use crate::core::interface::{AudioDevice, DeviceEnumerator, DeviceKind};
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Com::{
    CoGetApartmentType, CoInitializeEx, CoUninitialize, APTTYPE, APTTYPEQUALIFIER, APTTYPE_MAINSTA,
    APTTYPE_MTA, APTTYPE_STA, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
};
use windows::Win32::System::Threading::WaitForSingleObject;

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
enum Apartment {
    Plain,
    Mta,
    Sta,
}

use Apartment::{Mta, Plain, Sta};

fn apartment_type() -> windows::core::Result<APTTYPE> {
    let mut ty = APTTYPE::default();
    let mut qualifier = APTTYPEQUALIFIER::default();
    // SAFETY: Both output pointers refer to live, distinct, writable locals.
    unsafe { CoGetApartmentType(&mut ty, &mut qualifier) }?;
    Ok(ty)
}

impl Apartment {
    fn call<T>(self, f: impl FnOnce() -> T) -> T {
        // B2: Compare the exact STA type (including MAINSTA) on either side of this call.
        let before = matches!(self, Sta).then(|| apartment_type().expect("STA before call"));
        let result = f();
        if let Some(before) = before {
            // B2: Losing COM initialization is also an apartment change.
            assert_eq!(apartment_type().expect("STA after call"), before);
        }
        result
    }
}

fn spawn_on<T: Send + 'static>(
    apartment: Apartment,
    f: impl FnOnce() -> T + Send + 'static,
) -> JoinHandle<T> {
    thread::spawn(move || {
        let model = match apartment {
            // B1: A plain thread makes no CoInitializeEx call, even if an implicit MTA exists.
            Plain => return f(),
            Mta => COINIT_MULTITHREADED,
            Sta => COINIT_APARTMENTTHREADED,
        };
        struct Uninitialize;
        impl Drop for Uninitialize {
            fn drop(&mut self) {
                // SAFETY: This local guard balances one successful CoInitializeEx on this thread.
                unsafe { CoUninitialize() };
            }
        }
        // SAFETY: The reserved argument is null and model is exactly one of the two COM models.
        let initialized = unsafe { CoInitializeEx(None, model) };
        // B1/B2: Never exercise the caller on a thread whose setup failed.
        initialized.ok().expect("initialize fresh COM test thread");
        let _guard = Uninitialize;
        // B1/B2: Verify the fixture independently of the backend, before invoking it.
        let actual = apartment_type().expect("read initialized apartment");
        match apartment {
            Mta => assert_eq!(actual, APTTYPE_MTA),
            Sta => assert!(matches!(actual, APTTYPE_STA | APTTYPE_MAINSTA)),
            Plain => unreachable!(),
        }
        f()
    })
}

#[track_caller]
fn finish<T>(worker: JoinHandle<T>) -> T {
    // SAFETY: The owned JoinHandle keeps its native thread handle valid throughout the wait.
    let status =
        unsafe { WaitForSingleObject(HANDLE(worker.as_raw_handle()), TIMEOUT.as_millis() as u32) };
    // B1–B11: Bound the observer's wait, including COM teardown and thread exit, not just f().
    assert_eq!(
        status, WAIT_OBJECT_0,
        "worker did not exit within five seconds"
    );
    // B3/B4: A signaled Windows thread has terminated; join retrieves its result or original panic.
    worker
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[track_caller]
fn on<T: Send + 'static>(apartment: Apartment, f: impl FnOnce() -> T + Send + 'static) -> T {
    finish(spawn_on(apartment, f))
}

fn new_enumerator(apartment: Apartment) -> WindowsDeviceEnumerator {
    // B1/B2: Construction succeeds without changing the calling apartment.
    apartment
        .call(WindowsDeviceEnumerator::new)
        .expect("construct enumerator")
}

#[test]
fn construction_accepts_all_apartments() {
    for apartment in [Plain, Mta, Sta] {
        on(apartment, move || drop(new_enumerator(apartment)));
    }
}

#[test]
fn strict_initializer_rejects_sta() {
    on(Sta, || match Sta.call(ComInitializer::new) {
        // B2/B11: The strict initializer still rejects this actual STA with its original operation.
        Err(AudioError::BackendError { operation, .. }) => assert_eq!(operation, "com_init"),
        Err(other) => panic!("expected a com_init backend error, got {other:?}"),
        Ok(_) => panic!("strict initializer accepted an STA"),
    });
}

#[test]
fn enumerate_and_default_device_after_creator_exit() {
    for creator in [Plain, Mta, Sta] {
        let enumerator = Arc::new(on(creator, move || new_enumerator(creator)));
        for caller in [Plain, Mta, Sta] {
            let enumerator = Arc::clone(&enumerator);
            on(caller, move || {
                // B2/B3: Both calls work after the creator exited, even from a different apartment.
                caller
                    .call(|| enumerator.enumerate_devices())
                    .expect("enumeration after creator exit");
                let default = caller
                    .call(|| enumerator.default_device())
                    .expect("default after creator exit");
                // B2/B4: A metadata getter must also leave the calling STA unchanged.
                caller.call(|| default.id());
            });
        }
    }
}

fn read_kind(apartment: Apartment, device: &dyn AudioDevice) -> Option<DeviceKind> {
    match apartment.call(|| device.kind()) {
        Ok(kind) => Some(kind),
        // B4/B11: Unknown direction is an explicit backend error, never a guessed device kind.
        Err(AudioError::BackendError { operation, .. }) => {
            assert_eq!(operation, "get_device_kind");
            None
        }
        Err(other) => panic!("unexpected endpoint kind error: {other:?}"),
    }
}

#[test]
fn enumerate_on_sta_then_use_devices_after_its_exit() {
    let enumerator = on(Plain, || new_enumerator(Plain));
    let mut devices = on(Sta, move || {
        // B3/B4: Keep the actual device handles, not just their snapshots, when the STA exits.
        let devices = Sta
            .call(|| enumerator.enumerate_devices())
            .expect("enumeration on STA");
        // B4: This hardware test needs at least one device to transfer.
        assert!(!devices.is_empty(), "an audio endpoint is required");
        devices
            .into_iter()
            .map(|device| {
                let id = Sta.call(|| device.id());
                let name = Sta.call(|| device.name());
                let kind = read_kind(Sta, &*device);
                (device, id, name, kind)
            })
            .collect::<Vec<_>>()
    });
    for caller in [Plain, Mta, Sta] {
        devices = on(caller, move || {
            for (device, id, name, kind) in &devices {
                // B4: Compare with values read on the exited STA, not just two local getter calls.
                assert_eq!(&caller.call(|| device.id()), id);
                assert_eq!(&caller.call(|| device.name()), name);
                assert_eq!(&read_kind(caller, &**device), kind);
                // B4/B5: Formats are live; the contract does not freeze a mix format at enumeration.
                caller.call(|| device.supported_formats());
                caller.call(|| device.is_default());
                let info = caller.call(|| device.describe());
                // B4: The snapshot still identifies this endpoint.
                assert_eq!(&info.id, id);
                assert_eq!(&info.name, name);
                // B4/B6: Construct a stream without starting capture on the device.
                let stream = caller
                    .call(|| device.create_stream(&Default::default()))
                    .expect("construct stream");
                drop(stream);
            }
            devices
        });
    }
    // B10: The last owner drops all devices on another STA before the bounded wait returns.
    on(Sta, move || Sta.call(|| drop(devices)));
}

#[test]
fn enumerator_drops_on_sta() {
    let enumerator = on(Plain, || new_enumerator(Plain));
    // B2/B10: Move the sole owner, not a redundant Arc, and wait for destruction on the STA.
    on(Sta, move || Sta.call(|| drop(enumerator)));
}

#[test]
fn enumerate_sessions_on_sta() {
    on(Sta, || {
        // B2/B9: No applications need to be playing; a successful empty list is valid.
        Sta.call(enumerate_application_audio_sessions)
            .expect("list application sessions on STA");
    });
}

fn subscription_lifetime(register_on: Apartment, drop_on: Apartment) {
    let enumerator = on(Plain, || new_enumerator(Plain));
    let (delivery, events) = mpsc::channel();
    let returned = Arc::new(AtomicBool::new(false));
    let returned_in_handler = Arc::clone(&returned);
    let watcher = on(register_on, move || {
        // B7: The handler is the sole owner of the sender, including on a machine with no endpoints.
        let handler = Box::new(move |_| {
            let late = returned_in_handler.load(Ordering::SeqCst);
            let named_helper = thread::current().name() == Some("rsac-wasapi-device-watch");
            let mta = matches!(apartment_type(), Ok(APTTYPE_MTA));
            let _ = delivery.send((late, named_helper, mta));
        });
        // B2/B7: Registration succeeds before watch returns on the requested caller.
        register_on
            .call(|| enumerator.watch(handler))
            .expect("register device notifications")
    });

    let handler_state = move || loop {
        match events.try_recv() {
            Ok((late, named_helper, mta)) => {
                // B7: Check every observed invocation, including those queued during teardown.
                assert!(!late, "handler invoked after drop returned");
                assert!(named_helper, "event delivered outside the watcher thread");
                assert!(mta, "delivery thread is not in an MTA");
            }
            Err(state) => break state,
        }
    };
    on(drop_on, move || {
        // B7: A successful registration retains the handler until teardown.
        assert_eq!(handler_state(), mpsc::TryRecvError::Empty);
        drop_on.call(|| {
            drop(watcher);
            returned.store(true, Ordering::SeqCst);
        });
        // B7: Disconnection proves no future invocation is possible, even with silent hardware.
        // Check on the dropping thread, before its apartment or thread is torn down.
        assert_eq!(handler_state(), mpsc::TryRecvError::Disconnected);
    });
}

#[test]
fn sta_subscription_drops_on_plain() {
    subscription_lifetime(Sta, Plain);
}

#[test]
fn mta_subscription_drops_on_sta() {
    subscription_lifetime(Mta, Sta);
}

#[test]
fn plain_subscription_drops_on_mta() {
    subscription_lifetime(Plain, Mta);
}

#[test]
fn enumerate_concurrently_on_two_plain_and_two_sta_threads() {
    let enumerator = Arc::new(on(Plain, || new_enumerator(Plain)));
    let (ready, arrivals) = mpsc::channel();
    let workers = [Plain, Plain, Sta, Sta].map(|caller| {
        let enumerator = Arc::clone(&enumerator);
        let ready = ready.clone();
        let (start, gate) = mpsc::channel();
        let worker = spawn_on(caller, move || {
            // B3: All four callers must have established their apartments before any call starts.
            ready.send(()).expect("signal caller ready");
            gate.recv_timeout(TIMEOUT)
                .expect("start signal missing after five seconds");
            // B2/B3: Borrow the very same enumerator concurrently and traverse real endpoints.
            let devices = caller
                .call(|| enumerator.enumerate_devices())
                .expect("concurrent enumeration");
            assert!(!devices.is_empty(), "an audio endpoint is required");
        });
        (start, worker)
    });
    drop(ready);
    for _ in &workers {
        // B3: Bound the observer's wait even if a caller never reaches the start gate.
        arrivals
            .recv_timeout(TIMEOUT)
            .expect("caller not ready within five seconds");
    }
    for (start, _) in &workers {
        // B3: Release every caller before waiting for any enumeration result.
        start.send(()).expect("caller exited before start");
    }
    for (_, worker) in workers {
        finish(worker);
    }
}
