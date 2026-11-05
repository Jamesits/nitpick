//! Core Graphics, `ColorSync` and `AppKit` calls.
//!
//! Core Graphics display queries and `ColorSync` device queries run on the
//! calling worker. `NSScreen` is main-thread only: a call on another thread
//! is sent to the main dispatch queue and waits a bounded time, because a
//! host without a main run loop never drains that queue.

use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use dispatch2::DispatchQueue;
use nitpick_core::device::DeviceError;
use objc2::MainThreadMarker;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSDisplayGamut, NSScreen};
use objc2_color_sync::{
    ColorSyncDeviceCopyDeviceInfo, kColorSyncDeviceDefaultProfileID, kColorSyncDeviceDescription,
    kColorSyncDisplayDeviceClass, kColorSyncFactoryProfiles,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFString, CFType, CFUUID};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayCopyAllDisplayModes, CGDisplayIsBuiltin, CGDisplayMode,
    CGDisplayModelNumber, CGDisplaySerialNumber, CGDisplayVendorNumber, CGError,
    CGGetActiveDisplayList, kCGDisplayShowDuplicateLowResolutionModes,
};
use objc2_foundation::{NSNumber, ns_string};

use super::{ColorSyncDevice, DisplayReport, MacNative, ModeReport, ScreenReport};
use crate::common::{Field, NativeFailure};

/// Bound on the wait for the main thread, for a busy GUI thread. Not
/// measured.
const MAIN_THREAD_WAIT: Duration = Duration::from_secs(2);

#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C-unwind" {
    // Declared here: the binding in `objc2-color-sync` panics on NULL.
    fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> Option<NonNull<CFUUID>>;
}

pub struct System;

fn cg(api: &'static str, e: CGError) -> NativeFailure {
    NativeFailure {
        api,
        code: i64::from(e.0),
    }
}

fn active_displays() -> Result<Vec<CGDirectDisplayID>, DeviceError> {
    let mut count = 0u32;
    // SAFETY: a null list with 0 entries returns the count only.
    let e = unsafe { CGGetActiveDisplayList(0, std::ptr::null_mut(), &raw mut count) };
    if e != CGError::Success {
        return Err(cg("CGGetActiveDisplayList", e).into());
    }
    let mut ids = vec![0; count as usize];
    // SAFETY: the list has `count` entries. A display added after the first
    // call is left out until the next query.
    let e = unsafe { CGGetActiveDisplayList(count, ids.as_mut_ptr(), &raw mut count) };
    if e != CGError::Success {
        return Err(cg("CGGetActiveDisplayList", e).into());
    }
    ids.truncate(count as usize);
    Ok(ids)
}

fn uuid(display: CGDirectDisplayID) -> Field<objc2_core_foundation::CFRetained<CFUUID>> {
    // SAFETY: any display ID is valid input; an unknown ID gives NULL.
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(display) };
    uuid.map(|p| {
        // SAFETY: a "Create" function returns a +1 reference.
        unsafe { objc2_core_foundation::CFRetained::from_raw(p) }
    })
    .ok_or(NativeFailure {
        api: "CGDisplayCreateUUIDFromDisplayID",
        code: 0,
    })
}

fn modes(display: CGDirectDisplayID) -> Field<Vec<ModeReport>> {
    let key: &CFString = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
    // SAFETY: the option value is a CFBoolean, as documented.
    let list = unsafe { CGDisplayCopyAllDisplayModes(display, Some(options.as_opaque())) }.ok_or(
        NativeFailure {
            api: "CGDisplayCopyAllDisplayModes",
            code: 0,
        },
    )?;
    // SAFETY: the array holds `CGDisplayMode` values.
    let list = unsafe { list.cast_unchecked::<CGDisplayMode>() };
    Ok(list
        .iter()
        .map(|m| ModeReport {
            pixel_width: CGDisplayMode::pixel_width(Some(&m)),
            pixel_height: CGDisplayMode::pixel_height(Some(&m)),
            io_flags: CGDisplayMode::io_flags(Some(&m)),
        })
        .collect())
}

fn text(value: &CFType) -> Option<String> {
    if let Some(s) = value.downcast_ref::<CFString>() {
        return Some(s.to_string());
    }
    value
        .downcast_ref::<CFNumber>()
        .and_then(CFNumber::as_i64)
        .map(|n| n.to_string())
}

fn colorsync(uuid: &CFUUID) -> Option<ColorSyncDevice> {
    // SAFETY: the class is a ColorSync constant and the UUID is valid.
    let info = unsafe { ColorSyncDeviceCopyDeviceInfo(kColorSyncDisplayDeviceClass, uuid) }?;
    // SAFETY: device info has string keys and property-list values.
    let info = unsafe { info.cast_unchecked::<CFString, CFType>() };
    // SAFETY: ColorSync key constants.
    let (description, factory, default_id) = unsafe {
        (
            kColorSyncDeviceDescription,
            kColorSyncFactoryProfiles,
            kColorSyncDeviceDefaultProfileID,
        )
    };
    // The default profile ID is inside the factory profile dictionary.
    let default_profile_id = info
        .get(factory)
        .and_then(|f| f.downcast::<CFDictionary>().ok())
        .and_then(|f| {
            // SAFETY: as above.
            let f = unsafe { f.cast_unchecked::<CFString, CFType>() };
            f.get(default_id).as_deref().and_then(text)
        });
    Some(ColorSyncDevice {
        description: info.get(description).as_deref().and_then(text),
        default_profile_id,
    })
}

fn display_report(display: CGDirectDisplayID) -> DisplayReport {
    let uuid = uuid(display);
    let colorsync = uuid.as_ref().ok().and_then(|u| colorsync(u));
    DisplayReport {
        display_id: display,
        uuid: uuid.map(|u| {
            CFUUID::new_string(None, Some(&u))
                .map(|s| s.to_string())
                .unwrap_or_default()
        }),
        vendor: CGDisplayVendorNumber(display),
        model: CGDisplayModelNumber(display),
        serial: CGDisplaySerialNumber(display),
        builtin: CGDisplayIsBuiltin(display),
        modes: modes(display),
        colorsync,
    }
}

fn read_screens(mtm: MainThreadMarker) -> Vec<ScreenReport> {
    autoreleasepool(|_| {
        NSScreen::screens(mtm)
            .to_vec()
            .into_iter()
            .filter_map(|s| {
                let number = s
                    .deviceDescription()
                    .objectForKey(ns_string!("NSScreenNumber"))?
                    .downcast::<NSNumber>()
                    .ok()?;
                Some(ScreenReport {
                    display_id: number.unsignedIntValue(),
                    name: s.localizedName().to_string(),
                    edr_current: s.maximumExtendedDynamicRangeColorComponentValue(),
                    edr_potential: s.maximumPotentialExtendedDynamicRangeColorComponentValue(),
                    edr_reference: s.maximumReferenceExtendedDynamicRangeColorComponentValue(),
                    p3: s.canRepresentDisplayGamut(NSDisplayGamut::P3),
                })
            })
            .collect()
    })
}

impl MacNative for System {
    fn displays(&mut self) -> Result<Vec<DisplayReport>, DeviceError> {
        Ok(active_displays()?
            .into_iter()
            .map(|d| autoreleasepool(|_| display_report(d)))
            .collect())
    }

    fn screens(&mut self) -> Result<Vec<ScreenReport>, String> {
        if let Some(mtm) = MainThreadMarker::new() {
            return Ok(read_screens(mtm));
        }
        // The block owns the sender, so a late block after a timeout sends
        // to a closed channel and has no other effect.
        let (tx, rx) = mpsc::sync_channel(1);
        DispatchQueue::main().exec_async(move || {
            // After `dispatch_main` the main queue runs on another thread.
            let screens = MainThreadMarker::new()
                .map(read_screens)
                .ok_or_else(|| "the main dispatch queue is not on the main thread".to_owned());
            let _ = tx.send(screens);
        });
        rx.recv_timeout(MAIN_THREAD_WAIT).unwrap_or_else(|_| {
            Err(format!(
                "the main thread did not run the NSScreen query within {} s",
                MAIN_THREAD_WAIT.as_secs()
            ))
        })
    }
}
