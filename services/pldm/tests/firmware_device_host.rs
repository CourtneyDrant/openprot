
struct FakeFdOps {
    component_accepted: Cell<bool>,
    download_bytes_received: Cell<usize>,
    verified: Cell<bool>,
    applied: Cell<bool>,
}

impl FdOps for FakeFdOps {
    fn get_device_identifiers(&self, device_identifiers: &mut [Descriptor]) -> Result<usize, FdOpsError> {
        // Return fixed device ID
        Ok(0)  // No descriptors needed for basic test
    }

    fn get_firmware_parms(&self, firmware_params: &mut FirmwareParameters) -> Result<(), FdOpsError> {
        // Return fixed parameters (active component count, etc.)
        Ok(())
    }

    fn get_xfer_size(&self, ua_transfer_size: usize) -> Result<usize, FdOpsError> {
        // Accept UA's suggested transfer size (e.g., 512 bytes)
        Ok(ua_transfer_size.min(512))
    }

    fn handle_component(&self, component: &FirmwareComponent, _fw_params: &FirmwareParameters, op: ComponentOperation) 
        -> Result<ComponentResponseCode, FdOpsError> {
        // Accept all components
        self.component_accepted.set(true);
        Ok(ComponentResponseCode::CompCanBeUpdated)
    }

    fn query_download_offset_and_length(&self, _component: &FirmwareComponent) -> Result<(usize, usize), FdOpsError> {
        // Image is 1024 bytes starting at offset 0
        Ok((0, 1024))
    }

    fn download_fw_data(&self, offset: usize, data: &[u8], _component: &FirmwareComponent) 
        -> Result<TransferResult, FdOpsError> {
        self.download_bytes_received.set(self.download_bytes_received.get() + data.len());
        Ok(TransferResult::TransferSuccessfulReceived)
    }

    fn is_download_complete(&self, _component: &FirmwareComponent) -> bool {
        self.download_bytes_received.get() >= 1024
    }

    fn verify(&self, _component: &FirmwareComponent, _progress: &mut ProgressPercent) 
        -> Result<VerifyResult, FdOpsError> {
        self.verified.set(true);
        Ok(VerifyResult::VerifySuccess)
    }

    fn apply(&self, _component: &FirmwareComponent, _progress: &mut ProgressPercent) 
        -> Result<ApplyResult, FdOpsError> {
        self.applied.set(true);
        Ok(ApplyResult::ApplySuccess)
    }

    fn activate(&self, _self_contained: u8, _estimated_time: &mut u16) -> Result<u8, FdOpsError> {
        Ok(0)  // Success
    }

    // Other methods: return defaults or error as needed
    fn cancel_update_component(&self, _component: &FirmwareComponent) -> Result<(), FdOpsError> {
        Ok(())
    }

    fn query_download_progress(&self, _component: &FirmwareComponent, progress: &mut ProgressPercent) 
        -> Result<(), FdOpsError> {
        *progress = (self.download_bytes_received.get() * 100 / 1024) as u8;
        Ok(())
    }

    // ... other trait methods with sensible defaults
}

#### Fake FdUaRspChannel (scripted command queue)

struct ScriptedFdRsp {
    /// Queue of outbound (UA→FD) commands
    commands: RefCell<Vec<Vec<u8>>>,
    /// Captured responses from FD
    responses: RefCell<Vec<Vec<u8>>>,
    /// Current command index
    cmd_index: Cell<usize>,
}

impl ScriptedFdRsp {
    fn new() -> Self {
        Self {
            commands: RefCell::new(Vec::new()),
            responses: RefCell::new(Vec::new()),
            cmd_index: Cell::new(0),
        }
    }

    fn queue_command(&self, cmd: &[u8]) {
        self.commands.borrow_mut().push(cmd.to_vec());
    }
}

impl FdUaRspChannel for ScriptedFdRsp {
    fn recv(&self, buf: &mut [u8], _timeout_millis: u32) -> Result<usize, PldmServiceError> {
        let cmds = self.commands.borrow();
        let idx = self.cmd_index.get();
        
        if idx >= cmds.len() {
            // No more commands, signal end of test
            return Err(PldmServiceError::Ipc);  // or appropriate error
        }
        
        let cmd = &cmds[idx];
        if buf.len() < cmd.len() {
            return Err(PldmServiceError::Overflow);
        }
        
        buf[..cmd.len()].copy_from_slice(cmd);
        self.cmd_index.set(idx + 1);
        Ok(cmd.len())
    }

    fn respond(&self, buf: &[u8]) -> Result<(), PldmServiceError> {
        self.responses.borrow_mut().push(buf.to_vec());
        Ok(())
    }
}

#### Fake FdUaCmdChannel (pseudo Update Agent)

struct PseudoUa {
    /// Mock firmware image (e.g., 1024 bytes)
    image: [u8; 1024],
    /// Track request count
    request_count: Cell<usize>,
}

impl PseudoUa {
    fn new() -> Self {
        let mut image = [0u8; 1024];
        // Fill with deterministic pattern
        for (i, byte) in image.iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }
        Self {
            image,
            request_count: Cell::new(0),
        }
    }
}

impl FdUaCmdChannel for PseudoUa {
    fn transact(&self, req: &[u8], resp: &mut [u8]) -> Result<usize, PldmServiceError> {
        // Decode RequestFirmwareData request
        // buf[0] = MCTP type, buf[1..] = PLDM payload
        
        if req.len() < 2 {
            return Err(PldmServiceError::InvalidLength);
        }

        let pldm_req = &req[1..];  // Skip MCTP byte
        
        // Parse offset and length from request
        // RequestFirmwareDataRequest: [hdr(8 bytes)][offset(4)][length(4)]
        if pldm_req.len() < 16 {
            return Err(PldmServiceError::InvalidLength);
        }

        let offset = u32::from_le_bytes([pldm_req[8], pldm_req[9], pldm_req[10], pldm_req[11]]) as usize;
        let length = u32::from_le_bytes([pldm_req[12], pldm_req[13], pldm_req[14], pldm_req[15]]) as usize;

        self.request_count.set(self.request_count.get() + 1);

        // Build response: success code + requested image data
        resp[0] = 0x01;  // MCTP type
        // PLDM response header (8 bytes) + completion code (1) + image data
        // For simplicity, copy header from request and flip response bit
        resp[1..9].copy_from_slice(&pldm_req[..8]);
        resp[9] = 0;  // Completion code: success
        
        let slice_len = (length).min(self.image.len() - offset);
        resp[10..10 + slice_len].copy_from_slice(&self.image[offset..offset + slice_len]);
        
        Ok(10 + slice_len)
    }
}


## Script the FD Command Sequence

#[test]
fn firmware_device_update_flow() {
    let fake_ops = FakeFdOps {
        component_accepted: Cell::new(false),
        download_bytes_received: Cell::new(0),
        verified: Cell::new(false),
        applied: Cell::new(false),
    };

    let scripted_rsp = ScriptedFdRsp::new();
    let pseudo_ua = PseudoUa::new();
    
    let mut fd = FirmwareDevice::init(&fake_ops, &FW_UPDATE_CAPS);
    let mut buf = [0u8; 1024];

    // 1. RequestUpdate
    let req_update = RequestUpdateRequest::new(
        0,
        PldmMsgType::Request,
        512,          // max_transfer_size
        1,            // num_of_comp
        1,            // max_outstanding_transfer_req
        0,            // pkg_data_len
        &PldmFirmwareString::new("ASCII", "1.0.0").unwrap(),
    );
    let mut cmd_buf = [0u8; 1024];
    cmd_buf[0] = 0x01;  // MCTP type
    let cmd_len = 1 + req_update.encode(&mut cmd_buf[1..]).unwrap();
    scripted_rsp.queue_command(&cmd_buf[..cmd_len]);

    // 2. PassComponentTable
    let pass_comp = PassComponentTableRequest::new(
        0,
        PldmMsgType::Request,
        TransferRespFlag::StartAndEnd,
        ComponentClassification::Firmware,
        1,
        0,
        0,
        &PldmFirmwareString::new("ASCII", "app-1.0.0").unwrap(),
    );
    // ... encode and queue ...

    // 3. UpdateComponent
    let update_comp = UpdateComponentRequest::new(
        0,
        PldmMsgType::Request,
        ComponentClassification::Firmware,
        1,
        0,
        0,
        1024,  // comp_image_size
        UpdateOptionFlags(0),
        &PldmFirmwareString::new("ASCII", "app-1.0.1").unwrap(),
    );
    // ... encode and queue ...

    // 4. RequestFirmwareData will be handled via PseudoUa.transact()
    // (FD generates this internally when in download mode)

    // 5. TransferComplete (after FD has downloaded all bytes)
    let transfer_complete = TransferCompleteRequest::new(...);
    // ... encode and queue ...

    // 6. VerifyComplete
    let verify_complete = VerifyCompleteRequest::new(...);
    // ... encode and queue ...

    // 7. ApplyComplete
    let apply_complete = ApplyCompleteRequest::new(...);
    // ... encode and queue ...

    // 8. ActivateFirmware
    let activate = ActivateFirmwareRequest::new(...);
    // ... encode and queue ...

    // Run FD loop until scripted commands are exhausted
    loop {
        match fd.run_terminus(&scripted_rsp, &pseudo_ua, &mut buf, 0) {
            Ok(()) => {},
            Err(PldmServiceError::Ipc) => break,  // End of script
            Err(e) => panic!("FD error: {:?}", e),
        }
    }

    // Assert test expectations
    assert!(fake_ops.component_accepted.get(), "component should be accepted");
    assert_eq!(fake_ops.download_bytes_received.get(), 1024, "should receive full image");
    assert!(fake_ops.verified.get(), "verification should succeed");
    assert!(fake_ops.applied.get(), "application should succeed");
    assert_eq!(pseudo_ua.request_count.get(), 2, "should request firmware data twice");
    
    // Check response sequence
    let responses = scripted_rsp.responses.borrow();
    assert!(responses.len() >= 8, "should produce responses for each command");
}