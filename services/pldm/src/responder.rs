// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! PLDM responder that processes incoming PLDM-over-MCTP messages.
//!
//! ## Buffer layout
//!
//! `CmdInterface` from `pldm-interface` operates on a single flat buffer
//! whose first byte is the MCTP message-type byte (0x01 for PLDM) followed
//! immediately by the PLDM header and payload:
//!
//! ```text
//! buf[0]          : MCTP message-type (0x01)
//! buf[1..]        : PLDM message (header + data)
//! ```
//!
//! The MCTP API's [`MctpListener::recv`] writes only the PLDM bytes (no
//! MCTP framing byte) into the supplied buffer.  [`PldmResponder::run_once`]
//! therefore receives into `buf[1..]` and sets `buf[0]` before handing the
//! whole slice to `CmdInterface`.  The PLDM response (also without the MCTP
//! framing byte) is then extracted from `buf[1..resp_len]` and sent back via
//! the response channel.

use openprot_mctp_api::MctpClient;
use pldm_common::util::mctp_transport::MCTP_PLDM_MSG_TYPE;

use crate::error::PldmServiceError;
use crate::transport::MctpPldmTransport;

/// The MCTP message-type value used for PLDM (0x01).
pub const PLDM_MSG_TYPE: u8 = MCTP_PLDM_MSG_TYPE;

/// PLDM responder service.
pub struct PldmResponder {}

impl PldmResponder {
    /// Create a new PLDM responder.
    pub fn new() -> Self {
        PldmResponder {}
    }

    /// Receive and handle one incoming PLDM message over an MCTP transport.
    ///
    /// Calls [`MctpPldmTransport::recv_and_respond`] once, passing the framed
    /// buffer to `handler`.  `handler` must return the total response length
    /// (including `buf[0]`, the MCTP type byte).
    ///
    /// A `timeout_millis` of `0` blocks indefinitely.
    pub fn run_responder<C: MctpClient, F>(
        &mut self,
        transport: &MctpPldmTransport<C>,
        buf: &mut [u8],
        timeout_millis: u32,
        handler: F,
    ) -> Result<(), PldmServiceError>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, PldmServiceError>,
    {
        transport.recv_and_respond(buf, timeout_millis, handler)
        //CAD TODO: Forward to firmware device using fd_req
    }

}
