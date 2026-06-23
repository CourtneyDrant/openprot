// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Pigweed IPC channel implementations for [`FirmwareDevice`].
//!
//! Provides:
//! * [`IpcFdUaRspChannel`] – server-side channel that receives firmware-device
//!   commands via `channel_read` and responds via `channel_respond`.
//! * [`IpcFdUaCmdChannel`] – client-side channel that performs a synchronous
//!   firmware-update request/response round-trip via `channel_transact`.
//! * [`IpcUaFdRspChannel`] – server-side channel used by `PldmRequester` to
//!   receive forwarded PLDM requests from `FirmwareDevice` and respond with
//!   the MCTP result.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use openprot_pldm_firmware_device_ipc::{IpcFdUaRspChannel, IpcFdUaCmdChannel};
//! use openprot_pldm_service::firmware_device::FirmwareDevice;
//!
//! let fd_channel = IpcFdUaRspChannel::new(handle::FD_CMD);
//! let fw_channel = IpcFdUaCmdChannel::new(handle::FW_REQ);
//! let mut fd = FirmwareDevice::new(&PROTOCOL_CAPS);
//! let mut buf = [0u8; openprot_pldm_service::firmware_device::FD_IPC_MAX_MSG];
//! loop {
//!     let _ = fd.run_terminus(&fd_channel, &fw_channel, &mut buf, 0);
//! }
//! ```
//!
//! [`FirmwareDevice`]: openprot_pldm_service::firmware_device::FirmwareDevice

#![no_std]
#![warn(missing_docs)]

use openprot_pldm_service::error::PldmServiceError;
use openprot_pldm_service::firmware_device::{UaFdRspChannel, FdUaRspChannel, FdUaCmdChannel};
use userspace::time::Instant;

/// IPC server-side channel for receiving PLDM firmware-device commands.
///
/// Wraps a Pigweed IPC channel handle.  Each call to [`FdUaRspChannel::recv`]
/// reads one incoming request with `channel_read`; [`FdUaRspChannel::respond`]
/// sends the response with `channel_respond`.
///
/// The handle comes from the application's generated `handle` module
/// (e.g. `handle::FD_CMD`).
pub struct IpcFdUaRspChannel {
    handle: u32,
}

impl IpcFdUaRspChannel {
    /// Create a new channel bound to `handle`.
    pub fn new(handle: u32) -> Self {
        Self { handle }
    }

    /// Return the underlying IPC channel handle.
    pub fn channel_handle(&self) -> u32 {
        self.handle
    }
}

impl FdUaRspChannel for IpcFdUaRspChannel {
    fn recv(&self, buf: &mut [u8], _timeout_millis: u32) -> Result<usize, PldmServiceError> {
        userspace::syscall::channel_read(self.handle, 0, buf)
            .map_err(|_| PldmServiceError::Ipc)
    }

    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError> {
        userspace::syscall::channel_respond(self.handle, buf)
            .map_err(|_| PldmServiceError::Ipc)
    }
}

/// IPC client-side channel for sending PLDM firmware-update requests.
///
/// Wraps a Pigweed IPC channel handle.  Each call to [`FdUaCmdChannel::transact`]
/// performs one synchronous `channel_transact`, blocking until the response
/// arrives.
///
/// The handle comes from the application's generated `handle` module
/// (e.g. `handle::FW_REQ`).
pub struct IpcFdUaCmdChannel {
    handle: u32,
}

impl IpcFdUaCmdChannel {
    /// Create a new channel bound to `handle`.
    pub fn new(handle: u32) -> Self {
        Self { handle }
    }

    /// Return the underlying IPC channel handle.
    pub fn channel_handle(&self) -> u32 {
        self.handle
    }
}

impl FdUaCmdChannel for IpcFdUaCmdChannel {
    fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError> {
        userspace::syscall::channel_transact(self.handle, req, resp, Instant::MAX)
            .map_err(|_| PldmServiceError::Ipc)
    }
}

/// IPC client-side channel for sending PLDM firmware-command requests.
///
/// Wraps a Pigweed IPC channel handle.  Each call to [`UaFdRspChannel::transact`]
/// performs one synchronous `channel_transact`, blocking until the response
/// arrives.
///
/// The handle comes from the application's generated `handle` module
/// (e.g. `handle::FW_REQ`).
pub struct IpcUaFdRspChannel {
    handle: u32,
}

impl IpcUaFdRspChannel {
    /// Create a new channel bound to `handle`.
    pub fn new(handle: u32) -> Self {
        Self { handle }
    }

    /// Return the underlying IPC channel handle.
    pub fn channel_handle(&self) -> u32 {
        self.handle
    }
}

impl UaFdRspChannel for IpcUaFdRspChannel {
    fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError> {
        userspace::syscall::channel_transact(self.handle, req, resp, Instant::MAX)
            .map_err(|_| PldmServiceError::Ipc)
    }
}

/// IPC server-side channel used by [`PldmRequester`] to receive forwarded
/// PLDM requests from [`FirmwareDevice`] and respond with the MCTP result.
///
/// Wraps a Pigweed IPC channel handle.  Each call to [`UaFdRspChannel::recv`]
/// reads one incoming request with `channel_read`; [`UaFdRspChannel::respond`]
/// sends the response with `channel_respond`.
///
/// The handle comes from the application's generated `handle` module
/// (e.g. `handle::FW_REQ`).
///
/// [`PldmRequester`]: openprot_pldm_service::requester::PldmRequester
/// [`FirmwareDevice`]: openprot_pldm_service::firmware_device::FirmwareDevice
pub struct IpcUaFdRspChannel {
    handle: u32,
}

impl IpcUaFdRspChannel {
    /// Create a new channel bound to `handle`.
    pub fn new(handle: u32) -> Self {
        Self { handle }
    }

    /// Return the underlying IPC channel handle.
    pub fn channel_handle(&self) -> u32 {
        self.handle
    }
}

impl UaFdRspChannel for IpcUaFdRspChannel {
    fn recv(&self, buf: &mut [u8]) -> Result<usize, PldmServiceError> {
        userspace::syscall::channel_read(self.handle, 0, buf)
            .map_err(|_| PldmServiceError::Ipc)
    }

    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError> {
        userspace::syscall::channel_respond(self.handle, buf)
            .map_err(|_| PldmServiceError::Ipc)
    }
}
