// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! End-to-end host test wiring:
//! UA command -> PldmResponder -> FirmwareDevice -> PldmRequester -> remote UA
//! all via in-memory channels/transports.

use core::cell::{Cell, RefCell};

use mctp::{Eid, Tag};
use mctp_lib::fragment::{Fragmenter, SendOutput};
use mctp_lib::Sender;
use openprot_mctp_api::{Handle, MctpClient, MctpError, RecvMetadata, ResponseCode};
use openprot_mctp_server::Server;
use openprot_pldm_service::firmware_device::{FdUaCmdChannel, FdUaRspChannel, FirmwareDevice, UaFdCmdChannel, UaFdRspChannel};
use openprot_pldm_service::{MctpPldmTransport, PldmRequester, PldmResponder, PldmServiceError};
use pldm_common::codec::{PldmCodec, PldmCodecWithLifetime};
use pldm_common::message::control::SetTidRequest;
use pldm_common::message::firmware_update::request_fw_data::{RequestFirmwareDataRequest, RequestFirmwareDataResponse};
use pldm_common::protocol::base::PldmMsgType;
//use pldm_interface::config::PLDM_PROTOCOL_CAPABILITIES;
use pldm_common::protocol::firmware_update::{ComponentResponseCode, Descriptor};
use pldm_interface::firmware_device::fd_ops::{ComponentOperation, FdOps, FdOpsError};
use pldm_common::message::firmware_update::get_fw_params::FirmwareParameters;
use pldm_common::message::firmware_update::apply_complete::ApplyResult;
use pldm_common::message::firmware_update::transfer_complete::TransferResult;
use pldm_common::message::firmware_update::verify_complete::VerifyResult;
use pldm_common::message::firmware_update::get_status::ProgressPercent;
use pldm_common::util::fw_component::FirmwareComponent;

const FD_EID: u8 = 42;
const UA_EID: u8 = 8;
const TIMEOUT_MILLIS: u32 = 0;

struct BufferSender<'a> {
    packets: &'a RefCell<Vec<Vec<u8>>>,
}

impl Sender for BufferSender<'_> {
    fn send_vectored(
        &mut self,
        mut fragmenter: Fragmenter,
        payload: &[&[u8]],
    ) -> mctp::Result<Tag> {
        loop {
            // Fragmenter requires the output buffer to be at least the payload
            // MTU (255) plus the 4-byte MCTP transport header.
            let mut buf = [0u8; 255 + 4];
            match fragmenter.fragment_vectored(payload, &mut buf) {
                SendOutput::Packet(p) => self.packets.borrow_mut().push(p.to_vec()),
                SendOutput::Complete { tag, .. } => return Ok(tag),
                SendOutput::Error { err, .. } => return Err(err),
            }
        }
    }

    fn get_mtu(&self) -> usize {
        255
    }
}

fn transfer<S: Sender, const N: usize>(packets: &RefCell<Vec<Vec<u8>>>, dest: &mut Server<S, N>) {
    let pkts = packets.borrow();
    for pkt in pkts.iter() {
        dest.inbound(pkt).expect("inbound should accept packet");
    }
}

struct DirectClientWithPump<'a, S: Sender, const N: usize, F: FnMut()> {
    server: &'a RefCell<Server<S, N>>,
    pre_recv_pump: RefCell<F>,
}

impl<'a, S: Sender, const N: usize, F: FnMut()> DirectClientWithPump<'a, S, N, F> {
    fn new(server: &'a RefCell<Server<S, N>>, pre_recv_pump: F) -> Self {
        Self {
            server,
            pre_recv_pump: RefCell::new(pre_recv_pump),
        }
    }
}

impl<S: Sender, const N: usize, F: FnMut()> MctpClient for DirectClientWithPump<'_, S, N, F> {
    fn req(&self, eid: u8) -> Result<Handle, MctpError> {
        self.server.borrow_mut().req(eid)
    }

    fn listener(&self, msg_type: u8) -> Result<Handle, MctpError> {
        self.server.borrow_mut().listener(msg_type)
    }

    fn get_eid(&self) -> u8 {
        self.server.borrow().get_eid()
    }

    fn set_eid(&self, eid: u8) -> Result<(), MctpError> {
        self.server.borrow_mut().set_eid(eid)
    }

    fn recv(
        &self,
        handle: Handle,
        _timeout_millis: u32,
        buf: &mut [u8],
    ) -> Result<RecvMetadata, MctpError> {
        (self.pre_recv_pump.borrow_mut())();

        self.server
            .borrow_mut()
            .try_recv(handle, buf)
            .ok_or(MctpError::from_code(ResponseCode::TimedOut))
    }

    fn send(
        &self,
        handle: Option<Handle>,
        msg_type: u8,
        eid: Option<u8>,
        tag: Option<u8>,
        integrity_check: bool,
        buf: &[u8],
    ) -> Result<u8, MctpError> {
        self.server
            .borrow_mut()
            .send(handle, msg_type, eid, tag, integrity_check, buf)
    }

    fn drop_handle(&self, handle: Handle) {
        let _ = self.server.borrow_mut().unbind(handle);
    }
}

struct OneShotUaFdRsp {
    req: RefCell<Option<Vec<u8>>>,
    resp: RefCell<Option<Vec<u8>>>,
    served: Cell<bool>,
}

impl OneShotUaFdRsp {
    fn new() -> Self {
        Self {
            req: RefCell::new(None),
            resp: RefCell::new(None),
            served: Cell::new(false),
        }
    }

    fn load_req(&self, req: &[u8]) {
        *self.req.borrow_mut() = Some(req.to_vec());
        *self.resp.borrow_mut() = None;
        self.served.set(false);
    }

    fn take_resp(&self) -> Result<Vec<u8>, PldmServiceError> {
        self.resp.borrow_mut().take().ok_or(PldmServiceError::Ipc)
    }
}

impl UaFdRspChannel for OneShotUaFdRsp {
    fn recv(&self, buf: &mut [u8]) -> Result<usize, PldmServiceError> {
        if self.served.get() {
            return Err(PldmServiceError::Ipc);
        }

        let req = self.req.borrow_mut().take().ok_or(PldmServiceError::Ipc)?;
        if req.len() > buf.len() {
            return Err(PldmServiceError::Overflow);
        }

        buf[..req.len()].copy_from_slice(&req);
        self.served.set(true);
        Ok(req.len())
    }

    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError> {
        *self.resp.borrow_mut() = Some(buf.to_vec());
        Ok(())
    }
}

struct OneShotFdRsp {
    req: RefCell<Option<Vec<u8>>>,
    resp: RefCell<Option<Vec<u8>>>,
    served: Cell<bool>,
}

impl OneShotFdRsp {
    fn new() -> Self {
        Self {
            req: RefCell::new(None),
            resp: RefCell::new(None),
            served: Cell::new(false),
        }
    }

    fn load_req(&self, req: &[u8]) {
        *self.req.borrow_mut() = Some(req.to_vec());
        *self.resp.borrow_mut() = None;
        self.served.set(false);
    }

    fn take_resp(&self) -> Result<Vec<u8>, PldmServiceError> {
        self.resp.borrow_mut().take().ok_or(PldmServiceError::Ipc)
    }
}

struct FakeFdOps {
    component_accepted: Cell<bool>,
    download_bytes_received: Cell<usize>,
    verified: Cell<bool>,
    applied: Cell<bool>,
    activated: Cell<bool>,
}

impl FdOps for FakeFdOps {
    fn get_device_identifiers(&self, _device_identifiers: &mut [Descriptor]) -> Result<usize, FdOpsError> {
        Ok(0)
    }

    fn get_firmware_parms(&self, firmware_params: &mut FirmwareParameters) -> Result<(), FdOpsError> {
        *firmware_params = FirmwareParameters::default();
        Ok(())
    }

    fn get_xfer_size(&self, ua_transfer_size: usize) -> Result<usize, FdOpsError> {
        Ok(ua_transfer_size.min(512))
    }

    fn handle_component(
        &self,
        _component: &FirmwareComponent,
        _fw_params: &FirmwareParameters,
        _op: ComponentOperation,
    ) -> Result<ComponentResponseCode, FdOpsError> {
        self.component_accepted.set(true);
        Ok(ComponentResponseCode::CompCanBeUpdated)
    }

    fn query_download_offset_and_length(&self, _component: &FirmwareComponent) -> Result<(usize, usize), FdOpsError> {
        Ok((0, 1024))
    }

    fn download_fw_data(
        &self,
        _offset: usize,
        data: &[u8],
        _component: &FirmwareComponent,
    ) -> Result<TransferResult, FdOpsError> {
        self.download_bytes_received
            .set(self.download_bytes_received.get() + data.len());
        Ok(TransferResult::TransferSuccess)
    }

    fn is_download_complete(&self, _component: &FirmwareComponent) -> bool {
        self.download_bytes_received.get() >= 1024
    }

    fn query_download_progress(
        &self,
        _component: &FirmwareComponent,
        progress_percent: &mut ProgressPercent,
    ) -> Result<(), FdOpsError> {
        let pct = (self.download_bytes_received.get() * 100 / 1024) as u8;
        progress_percent
            .set_value(pct)
            .map_err(|_| FdOpsError::FwDownloadError)?;
        Ok(())
    }

    fn verify(
        &self,
        _component: &FirmwareComponent,
        _progress_percent: &mut ProgressPercent,
    ) -> Result<VerifyResult, FdOpsError> {
        self.verified.set(true);
        Ok(VerifyResult::VerifySuccess)
    }

    fn apply(
        &self,
        _component: &FirmwareComponent,
        _progress_percent: &mut ProgressPercent,
    ) -> Result<ApplyResult, FdOpsError> {
        self.applied.set(true);
        Ok(ApplyResult::ApplySuccess)
    }

    fn activate(&self, _self_contained_activation: u8, _estimated_time: &mut u16) -> Result<u8, FdOpsError> {
        self.activated.set(true);
        Ok(0)
    }

    fn cancel_update_component(&self, _component: &FirmwareComponent) -> Result<(), FdOpsError> {
        Ok(())
    }
}

impl FdUaRspChannel for OneShotFdRsp {
    fn recv(&self, buf: &mut [u8], _timeout_millis: u32) -> Result<usize, PldmServiceError> {
        if self.served.get() {
            return Err(PldmServiceError::Ipc);
        }

        let req = self.req.borrow_mut().take().ok_or(PldmServiceError::Ipc)?;
        if req.len() > buf.len() {
            return Err(PldmServiceError::Overflow);
        }

        buf[..req.len()].copy_from_slice(&req);
        self.served.set(true);
        Ok(req.len())
    }

    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError> {
        *self.resp.borrow_mut() = Some(buf.to_vec());
        Ok(())
    }
}

struct FakeRemoteUa {
    image: [u8; 1024],
    request_count: Cell<usize>,
}

impl FakeRemoteUa {
    fn new() -> Self {
        let mut image = [0u8; 1024];
        for (i, b) in image.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        Self {
            image,
            request_count: Cell::new(0),
        }
    }

    fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError> {
        let req_msg = RequestFirmwareDataRequest::decode(req.get(1..).ok_or(PldmServiceError::Overflow)?)
            .map_err(|_| PldmServiceError::Overflow)?;

        let offset = req_msg.offset as usize;
        let length = req_msg.length as usize;
        if offset >= self.image.len() {
            return Err(PldmServiceError::Overflow);
        }

        let end = offset.saturating_add(length).min(self.image.len());
        let payload = &self.image[offset..end];
        self.request_count.set(self.request_count.get() + 1);

        let rsp_msg = RequestFirmwareDataResponse::new(0, 0, payload);
        let body_len = rsp_msg
            .encode(resp.get_mut(1..).ok_or(PldmServiceError::Overflow)?)
            .map_err(|_| PldmServiceError::Overflow)?;
        resp[0] = 0x01;
        Ok(body_len + 1)
    }
}

#[test]
fn base_full_chain_via_pldm_responder() {
    let fd_ops = FakeFdOps {
        component_accepted: Cell::new(false),
        download_bytes_received: Cell::new(0),
        verified: Cell::new(false),
        applied: Cell::new(false),
        activated: Cell::new(false),
    };
    // In-memory MCTP endpoints: UA client side and FD responder side.
    let ua_to_fd_packets = RefCell::new(Vec::new());
    let ua_sender = BufferSender {
        packets: &ua_to_fd_packets,
    };
    let ua_server: RefCell<Server<_, 16>> = RefCell::new(Server::new(Eid(UA_EID), 0, ua_sender));

    let fd_to_ua_packets = RefCell::new(Vec::new());
    let fd_sender = BufferSender {
        packets: &fd_to_ua_packets,
    };
    let fd_server: RefCell<Server<_, 16>> = RefCell::new(Server::new(Eid(FD_EID), 0, fd_sender));

    let requester_bridge_chan = OneShotUaFdRsp::new();
    let fd_rsp_bridge_chan = OneShotFdRsp::new();

    let requester = RefCell::new(PldmRequester::new());
    let requester_buf = RefCell::new([0u8; 1024]);

    let requester_client = DirectClientWithPump::new(&ua_server, || {
        // Deliver queued remote-UA responses to requester endpoint when requester blocks in recv.
        transfer(&fd_to_ua_packets, &mut ua_server.borrow_mut());
        fd_to_ua_packets.borrow_mut().clear();
    });
    let requester_transport = MctpPldmTransport::new(requester_client);

    struct FdToRequesterBridge<'a, C: MctpClient> {
        chan: &'a OneShotUaFdRsp,
        requester: &'a RefCell<PldmRequester>,
        transport: &'a MctpPldmTransport<C>,
        requester_buf: &'a RefCell<[u8; 1024]>,
        remote_eid: u8,
    }

    impl<C: MctpClient> FdUaCmdChannel for FdToRequesterBridge<'_, C> {
        fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError> {
            self.chan.load_req(req);

            // For this test, short-circuit RequestFirmwareData locally to keep the
            // end-to-end harness deterministic without relying on additional UA stack.
            let local_len = if req.get(1 + 2).copied() == Some(0x15) {
                let mut tmp = [0u8; 1024];
                let local_len = FakeRemoteUa::new().transact(req, &mut tmp)?;
                let out = tmp.get(..local_len).ok_or(PldmServiceError::Overflow)?;
                if out.len() > resp.len() {
                    return Err(PldmServiceError::Overflow);
                }
                resp[..out.len()].copy_from_slice(out);
                return Ok(out.len());
            } else {
                0
            };
            let _ = local_len;

            self.requester
                .borrow_mut()
                .run_requester(
                    self.chan,
                    self.transport,
                    self.remote_eid,
                    &mut self.requester_buf.borrow_mut()[..],
                    TIMEOUT_MILLIS,
                )?;

            let bridged_resp = self.chan.take_resp()?;
            if bridged_resp.len() > resp.len() {
                return Err(PldmServiceError::Overflow);
            }
            resp[..bridged_resp.len()].copy_from_slice(&bridged_resp);
            Ok(bridged_resp.len())
        }
    }

    let fd_to_req_bridge = FdToRequesterBridge {
        chan: &requester_bridge_chan,
        requester: &requester,
        transport: &requester_transport,
        requester_buf: &requester_buf,
        remote_eid: FD_EID,
    };

    let fd = RefCell::new(FirmwareDevice::init(&fd_ops, &pldm_interface::config::PLDM_PROTOCOL_CAPABILITIES));
    let fd_buf = RefCell::new([0u8; 1024]);

    struct ResponderToFdBridge<'a, T: FdUaCmdChannel> {
        chan: &'a OneShotFdRsp,
        fd: &'a RefCell<FirmwareDevice<'a>>,
        fw_req: &'a T,
        fd_buf: &'a RefCell<[u8; 1024]>,
    }

    impl<T: FdUaCmdChannel> UaFdCmdChannel for ResponderToFdBridge<'_, T> {
        fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError> {
            self.chan.load_req(req);
            match self
                .fd
                .borrow_mut()
                .run_terminus(self.chan, self.fw_req, &mut self.fd_buf.borrow_mut()[..], TIMEOUT_MILLIS)
            {
                Ok(()) | Err(PldmServiceError::Ipc) => {}
                Err(e) => return Err(e),
            }

            let out = self.chan.take_resp()?;
            if out.len() > resp.len() {
                return Err(PldmServiceError::Overflow);
            }
            resp[..out.len()].copy_from_slice(&out);
            Ok(out.len())
        }
    }

    let responder_bridge = ResponderToFdBridge {
        chan: &fd_rsp_bridge_chan,
        fd: &fd,
        fw_req: &fd_to_req_bridge,
        fd_buf: &fd_buf,
    };

    let responder_client = DirectClientWithPump::new(&fd_server, || {
        transfer(&ua_to_fd_packets, &mut fd_server.borrow_mut());
        ua_to_fd_packets.borrow_mut().clear();
    });
    let responder_transport = MctpPldmTransport::new(responder_client);
    let responder = RefCell::new(PldmResponder::new());
    let responder_buf = RefCell::new([0u8; 1024]);

    let mut ua_req_buf = [0u8; 1024];
    //let version = PldmFirmwareString::new("ASCII", "1.0.0").expect("version string");
    //let req_update = RequestUpdateRequest::new(0, PldmMsgType::Request, 512, 1, 1, 0, &version);
    let set_tid = SetTidRequest::new(0, PldmMsgType::Request, 0x42);

    ua_req_buf[0] = 0x01;
    let req_len = 1 + set_tid
        .encode(&mut ua_req_buf[1..])
        .expect("encode request_update");

    // Run one full UA->FD->UA request/response roundtrip.
    let req_handle = ua_server
        .borrow_mut()
        .req(FD_EID)
        .expect("allocate request handle to FD");
    ua_server
        .borrow_mut()
        .send(Some(req_handle), 0x01, None, None, false, &ua_req_buf[1..req_len])
        .expect("send request_update payload");

    // The responder's pre-recv pump delivers the queued UA->FD packets into
    // fd_server *after* its listener is registered. Delivering them here would
    // route the request before any listener exists, causing it to be discarded.
    responder
        .borrow_mut()
        .run_responder(
            &responder_transport,
            &responder_bridge,
            &mut responder_buf.borrow_mut()[..],
            TIMEOUT_MILLIS
        )
        .expect("responder should forward and reply");

    transfer(&fd_to_ua_packets, &mut ua_server.borrow_mut());
    fd_to_ua_packets.borrow_mut().clear();

    let mut ua_resp_payload = [0u8; 1024];
    let resp_meta = ua_server
        .borrow_mut()
        .try_recv(req_handle, &mut ua_resp_payload)
        .expect("request_update response should be available");
    assert!(
        resp_meta.payload_size >= 4,
        "response should include PLDM header and completion code"
    );
    assert_eq!(
        ua_resp_payload[3], 0,
        "request_update completion code should be success"
    );

}
