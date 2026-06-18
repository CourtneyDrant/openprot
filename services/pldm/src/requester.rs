// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! PLDM requester that sends queued PLDM messages over MCTP and processes
//! responses.
//!
//! [`PldmRequester`] acts as a PLDM *initiator*: it takes a queued command,
//! sends it to a remote endpoint over MCTP, and validates the response. It
//! complements the responder-side [`PldmResponder`], which handles inbound
//! requests.
//!
//! ## Buffer layout
//!
//! The buffer passed to [`PldmRequester::run_once`] uses the same layout as
//! [`PldmResponder`]:
//!
//! ```text
//! buf[0]          : MCTP message-type (0x01) – written by send_request
//! buf[1..]        : PLDM message (header + data)
//! ```
//!
//! [`PldmResponder`]: crate::responder::PldmResponder

use openprot_mctp_api::MctpClient;

use crate::error::PldmServiceError;
use crate::firmware_device::FdReqChannel;
use crate::transport::MctpPldmTransport;

/// One outbound requester command to send on the next [`PldmRequester::run_once`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PldmRequesterCommand {
    /// PLDM Base `GetTID` request.
    GetTid,
}

/// PLDM requester service (initiator mode).
///
/// Sends a PLDM request over an MCTP transport provided by a
/// [`MctpPldmTransport`] and validates the response.  Use this alongside a
/// [`PldmResponder`] to exercise a complete request/response exchange.
///
/// # Example
///
/// ```rust,ignore
/// use openprot_pldm_service::requester::PldmRequester;
/// use openprot_pldm_service::transport::MctpPldmTransport;
/// use pldm_interface::control_context::ProtocolCapability;
/// use pldm_common::protocol::base::{PldmControlCmd, PldmSupportedType};
///
/// const CTRL_CMDS: [u8; 5] = [
///     PldmControlCmd::SetTid as u8,
///     PldmControlCmd::GetTid as u8,
///     PldmControlCmd::GetPldmCommands as u8,
///     PldmControlCmd::GetPldmVersion as u8,
///     PldmControlCmd::GetPldmTypes as u8,
/// ];
/// static CAPS: [ProtocolCapability<'static>; 1] = [ProtocolCapability {
///     pldm_type: PldmSupportedType::Base,
///     protocol_version: 0xF1F1F000,
///     supported_commands: &CTRL_CMDS,
/// }];
///
/// let transport = MctpPldmTransport::new(client);
/// let mut requester = PldmRequester::new(&CAPS);
/// let mut buf = [0u8; 1024];
/// requester.queue_get_tid();
/// requester.run_once(&transport, REMOTE_EID, &mut buf, 0).unwrap();
/// ```
///
/// [`PldmResponder`]: crate::responder::PldmResponder
#[allow(dead_code)]
pub struct PldmRequester {
    /// Instance ID stamped into the next outgoing request header.  Incremented
    /// (with wraparound) after each completed exchange so successive requests
    /// carry distinct instance IDs, as required by the PLDM base spec.
    instance_id: u8,
    /// Pending command to send on the next [`run_once`](Self::run_once).
    pending_command: Option<PldmRequesterCommand>,
}

impl PldmRequester {
    /// Create a new PLDM requester.
    ///
    /// `protocol_capabilities` describes the PLDM types, versions, and commands
    /// the local endpoint advertises. It is accepted for symmetry with
    /// [`PldmResponder::new`] and future expansion.
    ///
    /// [`PldmResponder::new`]: crate::responder::PldmResponder::new
    pub fn new() -> Self {
        PldmRequester {
            instance_id: 0,
            pending_command: None,
        }
    }
    
    /// Run a blocking loop that forwards raw PLDM requests from
    /// [`FirmwareDevice`] over MCTP and returns the responses.
    ///
    /// On each iteration:
    /// 1. Receives a framed PLDM request from `fd_req` (`buf[0]` = MCTP type,
    ///    `buf[1..]` = PLDM bytes).
    /// 2. Forwards it to `remote_eid` via `transport` and receives the
    ///    response into `buf[1..]`.
    /// 3. Responds to `fd_req` with `buf[0..1+pldm_resp_len]`.
    ///
    /// A `timeout_millis` of `0` blocks indefinitely on each MCTP exchange.
    ///
    /// [`FirmwareDevice`]: crate::firmware_device::FirmwareDevice
    pub fn run_requester<C: MctpClient>(
        &mut self,
        fd_req: &impl FdReqChannel,
        transport: &MctpPldmTransport<C>,
        remote_eid: u8,
        buf: &mut [u8],
        timeout_millis: u32,
    ) -> Result<(), PldmServiceError> {
        loop {
            // Receive raw PLDM request from FirmwareDevice.
            // buf[0] = MCTP framing byte (0x01), buf[1..msg_len] = PLDM bytes.
            let msg_len = fd_req.recv(buf)?;
            let pldm_len = msg_len
                .checked_sub(1)
                .ok_or(PldmServiceError::Overflow)?;

            // Forward over MCTP; response lands in buf[1..1+pldm_resp_len].
            let pldm_resp_len =
                transport.send_request(remote_eid, pldm_len, buf, timeout_millis)?;
            let resp_total_len = pldm_resp_len
                .checked_add(1)
                .ok_or(PldmServiceError::Overflow)?;

            // Return the framed response to FirmwareDevice.
            fd_req.respond(buf.get(..resp_total_len).ok_or(PldmServiceError::Overflow)?)?;
        }
    }
}
