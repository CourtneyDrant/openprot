// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! PLDM Firmware Device (FD) service.
//!
//! [`FirmwareDevice`] owns the PLDM firmware-update state machine and the
//! platform-specific flash operations.  It is intended to run as an isolated
//! process that sits between two IPC channels:
//!
//! * **`fd_cmd`** – receives raw MCTP-framed PLDM FW-update command bytes
//!   forwarded by the `pldm_responder` process and sends the response back.
//! * **`fw_req`** – sends raw MCTP-framed PLDM request bytes (e.g.
//!   `RequestFirmwareData`) to the `pldm_requester` process and receives the
//!   UA response.
//!
//! The two IPC ends are abstracted via the [`FdUaRspChannel`] and [`FdUaCmdChannel`]
//! traits so that the service crate remains independent of the Hubris / Pigweed
//! IPC codegen.
//!
//! ## Buffer layout
//!
//! Both channels carry the same flat buffer convention used throughout this
//! crate:
//!
//! ```text
//! buf[0]          : MCTP message-type (0x01)
//! buf[1..]        : PLDM message (header + data)
//! ```
//!
//! ## Main loop
//!
//! Each call to [`FirmwareDevice::run_once`] performs one full UA-command
//! cycle:
//!
//! 1. **Phase 1 – inbound**: receive one FW-update command from `fd_cmd`,
//!    dispatch it through [`CmdInterface::handle_responder_msg`], reply.
//! 2. **Phase 2 – outbound**: repeatedly call
//!    [`CmdInterface::generate_initiator_request`]; while a request is
//!    pending forward it over `fw_req`, receive the UA response, and feed it
//!    back via [`CmdInterface::process_initiator_response`].  Stop when no
//!    more requests are pending.

use pldm_interface::cmd_interface::CmdInterface;
use pldm_interface::firmware_device::fd_ops::FdOps;
use pldm_interface::firmware_device::fd_context::FirmwareDeviceContext;
use pldm_interface::control_context::ProtocolCapability;

use crate::error::PldmServiceError;

/// Maximum PLDM-over-IPC message size (MCTP-type byte + PLDM payload).
pub const FD_IPC_MAX_MSG: usize = 1024;

/// Server-side channel for receiving PLDM firmware-device commands and sending
/// responses back to the caller.
///
/// Implemented by platform-specific IPC glue (e.g. `IpcFdUaRspChannel` in
/// `openprot-pldm-firmware-device-ipc`).
pub trait FdUaRspChannel {
    /// Receive one incoming PLDM message into `buf`.
    ///
    /// Returns the number of bytes written.  `timeout_millis` of `0` blocks
    /// indefinitely; other values are a best-effort hint to the implementation.
    fn recv(&self, buf: &mut [u8], timeout_millis: u32) -> Result<usize, PldmServiceError>;

    /// Send a PLDM response back through the channel.
    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError>;
}

/// Server-side channel used by [`PldmRequester`] to receive raw PLDM requests
/// forwarded by [`FirmwareDevice`] and send the MCTP response back.
///
/// This is the counterpart of [`FdUaCmdChannel`]: `FirmwareDevice` calls
/// `FdUaCmdChannel::transact`; the `pldm_requester` process implements
/// `UaFdRspChannel` on the other end of that same IPC connection.
///
/// Implemented by platform-specific IPC glue (e.g. `IpcUaFdRspChannel` in
/// `openprot-pldm-firmware-device-ipc`).
///
/// [`PldmRequester`]: crate::requester::PldmRequester
pub trait UaFdRspChannel {
    /// Receive one raw PLDM request from [`FirmwareDevice`].
    ///
    /// `buf[0]` will be the MCTP message-type byte (`0x01`); `buf[1..]`
    /// contains the PLDM payload.  Returns the total number of bytes written
    /// (including the framing byte).
    fn recv(&self, buf: &mut [u8]) -> Result<usize, PldmServiceError>;

    /// Send the PLDM response back to [`FirmwareDevice`].
    ///
    /// `buf[0]` must be the MCTP message-type byte; `buf[1..]` must contain
    /// the PLDM response payload.
    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError>;
}

/// Client-side channel for sending PLDM firmware-update requests to the Update
/// Agent and receiving its responses.
///
/// Implemented by platform-specific IPC glue (e.g. `IpcFdUaCmdChannel` in
/// `openprot-pldm-firmware-device-ipc`).
pub trait FdUaCmdChannel {
    /// Perform a synchronous request/response round-trip.
    ///
    /// Sends `req` and blocks until the response arrives, writing it into
    /// `resp`.  Returns the number of response bytes written.
    fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError>;
}

/// Client-side channel for sending PLDM firmware-command requests to the Firmware
/// Device and receiving its responses.
///
/// This is the counterpart of [`FdUaRspChannel`]: `PldmResponder` calls
/// `UaFdCmdChannel::transact`; `FirmwareDevice` implements `FdUaRspChannel` on
/// the other end of that same IPC connection.
///
/// Implemented by platform-specific IPC glue (e.g. `IpcUaFdCmdChannel` in
/// `openprot-pldm-firmware-device-ipc`).
pub trait UaFdCmdChannel {
    /// Perform a synchronous request/response round-trip.
    ///
    /// Sends `req` and blocks until the response arrives, writing it into
    /// `resp`.  Returns the number of response bytes written.
    fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError>;
}

#[allow(missing_docs)]
pub struct FirmwareDevice<'a> {
    cmd_interface: CmdInterface<'a>,
}

impl<'a> FirmwareDevice<'a> {
    /// Create a new [`FirmwareDevice`] with the given protocol capabilities.
    ///
    /// `protocol_capabilities` should advertise at least
    /// [`PldmSupportedType::FwUpdate`] so that the [`CmdInterface`] accepts
    /// and routes firmware-update commands correctly.
    ///
    /// [`PldmSupportedType::FwUpdate`]: pldm_common::protocol::base::PldmSupportedType::FwUpdate
    pub fn init(fdops: &'a dyn FdOps, protocol_capabilities: &'a [ProtocolCapability<'a>]) -> Self {
        FirmwareDevice {
            cmd_interface: CmdInterface::new(protocol_capabilities, FirmwareDeviceContext::new(fdops)),
        }
    }

    #[allow(missing_docs)]
    pub fn run_terminus(
        &mut self,
        fd_rsp: &impl FdUaRspChannel,
        fw_req: &impl FdUaCmdChannel,
        buf: &mut [u8],
        timeout_millis: u32,
    ) -> Result<(), PldmServiceError> {
        // Loop until the inbound command is handled
        loop {
            if self.cmd_interface.is_update_mode() {
                let mut fw_buf  = [0u8; FD_IPC_MAX_MSG];
                let mut fw_resp  = [0u8; FD_IPC_MAX_MSG];
                self.cmd_interface.handle_initiator_msg(&mut fw_buf)
                    .map_err(PldmServiceError::MsgHandler)?;
                // Build a request using pldm-lib
                fw_req.transact(&fw_buf, &mut fw_resp)?;
                self.cmd_interface.handle_initiator_response(&mut fw_resp)
                    .map_err(PldmServiceError::MsgHandler)?;
            } 

            let msg_len = fd_rsp.recv(buf, timeout_millis)?;
            let resp_len = self
                .cmd_interface
                .handle_responder_msg(&mut buf[..msg_len])
                .map_err(PldmServiceError::MsgHandler)?;
            fd_rsp.respond(&buf[..resp_len])?;
        }
    }
}
