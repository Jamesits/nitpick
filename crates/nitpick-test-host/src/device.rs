//! Device calls of the test host: the interface between the controller and
//! the device worker of the host.
//!
//! The controller sends the calls in request order and handles each reply
//! in that order. The implementation runs them on one worker, so a call
//! starts only after the earlier calls end.

use nitpick_core::device::{
    DeviceError, DeviceId, DeviceInfo, DeviceQuery, DisplaySetup, IccProfile, ProfileState,
    ProfileTarget,
};

use crate::controller::LocalFuture;

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceCall {
    /// Restores the records of an earlier process. The controller sends it
    /// before the first other call.
    Recover,
    List,
    Query(DeviceId),
    Setup(DeviceId),
    Targets(DeviceId),
    State(ProfileTarget),
    /// Prepares, saves the record, assigns and reads back. A failed
    /// assignment is restored in the same call.
    Assign {
        target: ProfileTarget,
        profile: IccProfile,
    },
    /// Restores the active change.
    Restore,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeviceReply {
    Recovered(Vec<RecoveredRecord>),
    List(Vec<DeviceInfo>),
    Query(Box<DeviceQuery>),
    Setup(DisplaySetup),
    Targets(Vec<ProfileTarget>),
    State(ProfileState),
    Assigned(Assigned),
    Restored(Restored),
}

/// One record of [`DeviceCall::Recover`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredRecord {
    /// The record file name.
    pub record: String,
    /// The verified original state; the record is removed. An error keeps
    /// the record.
    pub result: Result<ProfileState, DeviceError>,
}

/// A record was saved. An error before the save is the error of the call.
#[derive(Debug, Clone, PartialEq)]
pub struct Assigned {
    pub applied: Result<ProfileState, DeviceError>,
    /// After a failed assignment: its restoration.
    pub restored: Option<Result<Restored, DeviceError>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Restored {
    pub state: ProfileState,
    /// The assignment timed out: its write can still land, so the record
    /// stays until the next recovery.
    pub record_kept: bool,
}

/// The device worker of a host.
pub trait Devices {
    /// Dropping the future does not stop the call.
    fn call(&mut self, call: DeviceCall) -> LocalFuture<Result<DeviceReply, DeviceError>>;
}

#[cfg(any(test, feature = "device"))]
pub mod worker;
