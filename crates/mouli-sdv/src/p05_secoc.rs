//! Part 5: SECOC, DOIP (ISO 13400), NETWORK MANAGEMENT (NM) & ECU STATE MANAGER (ECUM/BSWM)

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 5 - SECOC, DOIP (ISO 13400), NETWORK MANAGEMENT (NM) & ECU STATE MANAGER (ECUM/BSWM)
// CRITICALITY: ISO 26262 ASIL-D / ISO 21434 Cybersecurity / AUTOSAR Adaptive R22-11
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocation, Deterministic State Machines
// =================================================================================================

use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS: CRYPTOGRAPHIC, DOIP & NETWORK MANAGEMENT SPECS
// =================================================================================================

/// Maximum payload capacity for secured PDUs in SecOC
pub const MAX_SECOC_PDU_PAYLOAD: usize = 128;

/// AUTOSAR SecOC Authenticator (Truncated MAC) length in bytes (standard 24 to 64-bit truncation)
pub const SECOC_MAC_LEN_BYTES: usize = 8; // 64-bit truncated MAC

/// Maximum size of DoIP transport frames over vehicle Ethernet (MTU friendly)
pub const MAX_DOIP_PAYLOAD_SIZE: usize = 1440;

/// DoIP Protocol Version compliant with ISO 13400-2:2019
pub const DOIP_PROTOCOL_VERSION: u8 = 0x02;
pub const DOIP_INVERSE_PROTOCOL_VERSION: u8 = 0xFD;

/// Maximum number of active DoIP TCP/UDP client channels
pub const MAX_DOIP_CHANNELS: usize = 4;

/// AUTOSAR Network Management PDU length
pub const NM_PDU_SIZE: usize = 8;

// =================================================================================================
// 2. ERROR TAXONOMY & SECURITY STATUS ENUMS
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecOcError {
    Ok = 0,
    AuthenticationFailure = 1,
    FreshnessValueDesync = 2,
    FreshnessCounterExhausted = 3,
    PayloadTruncationError = 4,
    CryptoHardwareBusy = 5,
    KeySlotInvalid = 6,
    PduBufferExceeded = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoIpError {
    Ok = 0,
    IncorrectPatternFormat = 1,
    UnknownPayloadType = 2,
    MessageTooLarge = 3,
    OutOfMemory = 4,
    RoutingActivationDenied = 5,
    UnknownSourceAddress = 6,
    DiagnosticTargetUnreachable = 7,
    ChannelNotActive = 8,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcuPowerState {
    Off = 0,
    Startup = 1,
    Run = 2,
    PostRun = 3,
    Standby = 4,
    Sleep = 5,
    EmergencySafeShutdown = 6,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NmState {
    BusSleep = 0,
    PrepareBusSleep = 1,
    RepeatMessage = 2,
    NormalOperation = 3,
    ReadySleep = 4,
}

// =================================================================================================
// 3. HARDWARE SECURITY MODULE (HSM) ABSTRACTION & CRYPTO CIPHER
// =================================================================================================

/// AES-128-CMAC (RFC 4493) message authentication, implemented in software
pub struct HardwareSecurityModule;

impl HardwareSecurityModule {
    /// Bitwise left-shift on a 128-bit big-endian state
    fn shift_left_128(input: &[u8; 16], output: &mut [u8; 16]) -> u8 {
        let mut overflow = 0u8;
        for i in (0..16).rev() {
            let next_overflow = (input[i] & 0x80) >> 7;
            output[i] = (input[i] << 1) | overflow;
            overflow = next_overflow;
        }
        overflow
    }

    /// Constant-time memory comparison to protect against side-channel timing attacks
    #[inline(always)]
    pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for i in 0..a.len() {
            diff |= a[i] ^ b[i];
        }
        diff == 0
    }

    /// AES-128-CMAC (RFC 4493) of `message` under `key`.
    ///
    /// This replaces an earlier routine that contained no AES at all (it mixed bytes with
    /// add-and-rotate steps, so its "MAC" could be forged).
    pub fn compute_cmac_aes128(key: &[u8; 16], message: &[u8], out_mac: &mut [u8; 16]) {
        let aes = Aes128::new(key);

        // Subkeys: K1 = L*x, K2 = K1*x in GF(2^128), where L = AES_K(0).
        let l = aes.encrypt_block(&[0u8; 16]);
        let mut k1 = [0u8; 16];
        if Self::shift_left_128(&l, &mut k1) != 0 {
            k1[15] ^= 0x87;
        }
        let mut k2 = [0u8; 16];
        if Self::shift_left_128(&k1, &mut k2) != 0 {
            k2[15] ^= 0x87;
        }

        // CBC-MAC over all but the last block; the last block is masked with K1 (complete) or
        // padded with 10* and masked with K2 (incomplete or empty message).
        let blocks = message.len().div_ceil(16).max(1);
        let mut x = [0u8; 16];
        for block in message.chunks(16).take(blocks - 1) {
            for (xi, mi) in x.iter_mut().zip(block) {
                *xi ^= mi;
            }
            x = aes.encrypt_block(&x);
        }
        let rest = message.get(16 * (blocks - 1)..).unwrap_or(&[]);
        let mut last = [0u8; 16];
        if rest.len() == 16 {
            for i in 0..16 {
                last[i] = rest[i] ^ k1[i];
            }
        } else {
            last[..rest.len()].copy_from_slice(rest);
            last[rest.len()] = 0x80;
            for i in 0..16 {
                last[i] ^= k2[i];
            }
        }
        for (xi, li) in x.iter_mut().zip(&last) {
            *xi ^= li;
        }
        *out_mac = aes.encrypt_block(&x);
    }
}

/// Multiplication by x in GF(2^8) modulo x^8 + x^4 + x^3 + x + 1, without branches.
#[inline(always)]
const fn xtime(a: u8) -> u8 {
    (a << 1) ^ (0x1B & 0u8.wrapping_sub(a >> 7))
}

/// Multiplication in GF(2^8), constant time (fixed loop, mask instead of branch).
const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0u8;
    let mut i = 0;
    while i < 8 {
        product ^= a & 0u8.wrapping_sub(b & 1);
        a = xtime(a);
        b >>= 1;
        i += 1;
    }
    product
}

/// The AES S-box computed algebraically (inverse as x^254, then the affine map) rather than
/// looked up by a secret-dependent index, so it has no cache-timing side channel.
const fn sbox(x: u8) -> u8 {
    let mut result = 1u8;
    let mut base = x;
    let mut bit = 0;
    while bit < 8 {
        // 254 = 0b1111_1110: multiply in `base` for every set bit, selecting with a mask.
        let take = 0u8.wrapping_sub((254u8 >> bit) & 1);
        let product = gmul(result, base);
        result = (product & take) | (result & !take);
        base = gmul(base, base);
        bit += 1;
    }
    result
        ^ result.rotate_left(1)
        ^ result.rotate_left(2)
        ^ result.rotate_left(3)
        ^ result.rotate_left(4)
        ^ 0x63
}

/// AES-128 block encryption (FIPS 197). Encryption only, which is all CMAC needs.
pub struct Aes128 {
    round_keys: [[u8; 16]; 11],
}

impl Aes128 {
    /// Expands `key` into the 11 round keys.
    pub fn new(key: &[u8; 16]) -> Self {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [sbox(t[1]) ^ rcon, sbox(t[2]), sbox(t[3]), sbox(t[0])];
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut round_keys = [[0u8; 16]; 11];
        for (r, rk) in round_keys.iter_mut().enumerate() {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Self { round_keys }
    }

    fn add_round_key(state: &mut [u8; 16], rk: &[u8; 16]) {
        for (s, k) in state.iter_mut().zip(rk) {
            *s ^= k;
        }
    }

    fn sub_bytes(state: &mut [u8; 16]) {
        for s in state.iter_mut() {
            *s = sbox(*s);
        }
    }

    fn shift_rows(state: &mut [u8; 16]) {
        let old = *state;
        for c in 0..4 {
            for r in 0..4 {
                state[r + 4 * c] = old[r + 4 * ((c + r) % 4)];
            }
        }
    }

    fn mix_columns(state: &mut [u8; 16]) {
        for c in 0..4 {
            let col = [
                state[4 * c],
                state[4 * c + 1],
                state[4 * c + 2],
                state[4 * c + 3],
            ];
            let all = col[0] ^ col[1] ^ col[2] ^ col[3];
            for r in 0..4 {
                state[4 * c + r] = col[r] ^ all ^ xtime(col[r] ^ col[(r + 1) % 4]);
            }
        }
    }

    /// Encrypts one 16-byte block.
    pub fn encrypt_block(&self, block: &[u8; 16]) -> [u8; 16] {
        let mut state = *block;
        Self::add_round_key(&mut state, &self.round_keys[0]);
        for round in 1..10 {
            Self::sub_bytes(&mut state);
            Self::shift_rows(&mut state);
            Self::mix_columns(&mut state);
            Self::add_round_key(&mut state, &self.round_keys[round]);
        }
        Self::sub_bytes(&mut state);
        Self::shift_rows(&mut state);
        Self::add_round_key(&mut state, &self.round_keys[10]);
        state
    }
}

// =================================================================================================
// 4. AUTOSAR SECOC: SECURE ONBOARD COMMUNICATION & FRESHNESS VALUE MANAGEMENT (FVM)
// =================================================================================================

/// Struct managing 64-bit Freshness Value Counter (Trip Count + Reset Count + Message Counter)
pub struct FreshnessValueManager {
    trip_counter: AtomicU32,
    reset_counter: AtomicU16,
    message_counter: AtomicU16,
    lock: AtomicBool,
}

impl FreshnessValueManager {
    pub const fn new() -> Self {
        Self {
            trip_counter: AtomicU32::new(1),
            reset_counter: AtomicU16::new(0),
            message_counter: AtomicU16::new(0),
            lock: AtomicBool::new(false),
        }
    }

    #[inline(always)]
    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
    }

    #[inline(always)]
    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }

    /// Retrieves current monotonic 64-bit freshness value
    pub fn get_freshness_value(&self) -> u64 {
        let trip = self.trip_counter.load(Ordering::Relaxed) as u64;
        let reset = self.reset_counter.load(Ordering::Relaxed) as u64;
        let msg = self.message_counter.load(Ordering::Relaxed) as u64;

        (trip << 32) | (reset << 16) | msg
    }

    /// Increments message counter, with rollover handling into reset counter
    pub fn increment_counter(&self) -> Result<u64, SecOcError> {
        self.acquire();
        let mut msg = self.message_counter.load(Ordering::Relaxed);
        let mut reset = self.reset_counter.load(Ordering::Relaxed);

        if msg == 0xFFFF {
            msg = 0;
            if reset == 0xFFFF {
                self.release();
                return Err(SecOcError::FreshnessCounterExhausted);
            }
            reset += 1;
            self.reset_counter.store(reset, Ordering::Relaxed);
        } else {
            msg += 1;
        }

        self.message_counter.store(msg, Ordering::Relaxed);
        let trip = self.trip_counter.load(Ordering::Relaxed) as u64;
        self.release();

        Ok((trip << 32) | ((reset as u64) << 16) | (msg as u64))
    }

    pub fn increment_trip(&self) {
        self.acquire();
        self.trip_counter.fetch_add(1, Ordering::SeqCst);
        self.reset_counter.store(0, Ordering::Relaxed);
        self.message_counter.store(0, Ordering::Relaxed);
        self.release();
    }
}

impl Default for FreshnessValueManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Fully Authentic SecOC PDU Container
#[derive(Clone, Copy)]
pub struct SecOcPdu {
    pub pdu_id: u16,
    pub payload_len: usize,
    pub payload: [u8; MAX_SECOC_PDU_PAYLOAD],
    pub freshness_value: u64,
    pub authenticator_mac: [u8; SECOC_MAC_LEN_BYTES],
}

pub struct SecOcEngine {
    secret_key: [u8; 16],
    pub fvm: FreshnessValueManager,
}

impl SecOcEngine {
    pub const fn new(symmetric_key: [u8; 16]) -> Self {
        Self {
            secret_key: symmetric_key,
            fvm: FreshnessValueManager::new(),
        }
    }

    /// Encapsulates authentic payload with freshness and computed CMAC authenticator
    pub fn transmit_secured_pdu(
        &self,
        pdu_id: u16,
        raw_payload: &[u8],
        out_pdu: &mut SecOcPdu,
    ) -> Result<(), SecOcError> {
        if raw_payload.len() > MAX_SECOC_PDU_PAYLOAD {
            return Err(SecOcError::PduBufferExceeded);
        }

        let freshness = self.fvm.increment_counter()?;
        out_pdu.pdu_id = pdu_id;
        out_pdu.payload_len = raw_payload.len();
        out_pdu.payload[..raw_payload.len()].copy_from_slice(raw_payload);
        out_pdu.freshness_value = freshness;

        // Assembly buffer for authentication: Data || FreshnessValue (64-bit BE)
        let mut auth_buffer = [0u8; MAX_SECOC_PDU_PAYLOAD + 8];
        auth_buffer[..raw_payload.len()].copy_from_slice(raw_payload);
        auth_buffer[raw_payload.len()..raw_payload.len() + 8]
            .copy_from_slice(&freshness.to_be_bytes());

        let mut full_mac = [0u8; 16];
        HardwareSecurityModule::compute_cmac_aes128(
            &self.secret_key,
            &auth_buffer[..raw_payload.len() + 8],
            &mut full_mac,
        );

        // Truncate MAC to the configured 64-bit authenticator
        out_pdu
            .authenticator_mac
            .copy_from_slice(&full_mac[..SECOC_MAC_LEN_BYTES]);
        Ok(())
    }

    /// Validates an incoming secured PDU against freshness rules and cryptographic MAC
    pub fn receive_verify_pdu(&self, secured_pdu: &SecOcPdu) -> Result<(), SecOcError> {
        let current_freshness = self.fvm.get_freshness_value();
        // Monotonic check: Incoming freshness must not be older than recent known frame
        if secured_pdu.freshness_value < current_freshness.saturating_sub(64) {
            return Err(SecOcError::FreshnessValueDesync);
        }

        let mut auth_buffer = [0u8; MAX_SECOC_PDU_PAYLOAD + 8];
        let len = secured_pdu.payload_len;
        auth_buffer[..len].copy_from_slice(&secured_pdu.payload[..len]);
        auth_buffer[len..len + 8].copy_from_slice(&secured_pdu.freshness_value.to_be_bytes());

        let mut full_mac = [0u8; 16];
        HardwareSecurityModule::compute_cmac_aes128(
            &self.secret_key,
            &auth_buffer[..len + 8],
            &mut full_mac,
        );

        let is_valid = HardwareSecurityModule::constant_time_eq(
            &secured_pdu.authenticator_mac,
            &full_mac[..SECOC_MAC_LEN_BYTES],
        );

        if !is_valid {
            return Err(SecOcError::AuthenticationFailure);
        }

        Ok(())
    }
}

// =================================================================================================
// 5. ISO 13400-2 DOIP: DIAGNOSTICS OVER IP ROUTING ENGINE
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoIpPayloadType {
    GenericHeaderNegativeAcknowledge = 0x0000,
    VehicleIdentificationRequest = 0x0001,
    VehicleIdentificationRequestEid = 0x0002,
    VehicleIdentificationRequestVin = 0x0003,
    VehicleAnnouncementMessage = 0x0004,
    RoutingActivationRequest = 0x0005,
    RoutingActivationResponse = 0x0006,
    AliveCheckRequest = 0x0007,
    AliveCheckResponse = 0x0008,
    DiagnosticMessage = 0x8001,
    DiagnosticMessagePositiveAck = 0x8002,
    DiagnosticMessageNegativeAck = 0x8003,
    Unknown = 0xFFFF,
}

impl From<u16> for DoIpPayloadType {
    fn from(val: u16) -> Self {
        match val {
            0x0000 => DoIpPayloadType::GenericHeaderNegativeAcknowledge,
            0x0001 => DoIpPayloadType::VehicleIdentificationRequest,
            0x0002 => DoIpPayloadType::VehicleIdentificationRequestEid,
            0x0003 => DoIpPayloadType::VehicleIdentificationRequestVin,
            0x0004 => DoIpPayloadType::VehicleAnnouncementMessage,
            0x0005 => DoIpPayloadType::RoutingActivationRequest,
            0x0006 => DoIpPayloadType::RoutingActivationResponse,
            0x0007 => DoIpPayloadType::AliveCheckRequest,
            0x0008 => DoIpPayloadType::AliveCheckResponse,
            0x8001 => DoIpPayloadType::DiagnosticMessage,
            0x8002 => DoIpPayloadType::DiagnosticMessagePositiveAck,
            0x8003 => DoIpPayloadType::DiagnosticMessageNegativeAck,
            _ => DoIpPayloadType::Unknown,
        }
    }
}

#[derive(Clone, Copy)]
pub struct DoIpMessage {
    pub payload_type: DoIpPayloadType,
    pub length: u32,
    pub payload: [u8; MAX_DOIP_PAYLOAD_SIZE],
}

impl DoIpMessage {
    pub const fn empty() -> Self {
        Self {
            payload_type: DoIpPayloadType::Unknown,
            length: 0,
            payload: [0u8; MAX_DOIP_PAYLOAD_SIZE],
        }
    }
}

pub struct DoIpClientChannel {
    pub is_active: AtomicBool,
    pub client_logical_addr: AtomicU16,
    pub activation_type: AtomicU8,
}

pub struct DoIpServer {
    pub ecu_logical_address: u16,
    pub vin: [u8; 17],
    pub entity_id: [u8; 6], // MAC address format
    pub group_id: [u8; 6],
    channels: [DoIpClientChannel; MAX_DOIP_CHANNELS],
}

impl DoIpServer {
    pub const fn new(logical_addr: u16, vin: [u8; 17], mac: [u8; 6]) -> Self {
        const INIT_CHANNEL: DoIpClientChannel = DoIpClientChannel {
            is_active: AtomicBool::new(false),
            client_logical_addr: AtomicU16::new(0),
            activation_type: AtomicU8::new(0),
        };

        Self {
            ecu_logical_address: logical_addr,
            vin,
            entity_id: mac,
            group_id: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
            channels: [INIT_CHANNEL; MAX_DOIP_CHANNELS],
        }
    }

    /// Serializes a DoIP header and payload into an Ethernet MTU byte buffer
    pub fn serialize_packet(msg: &DoIpMessage, out_buf: &mut [u8]) -> Result<usize, DoIpError> {
        let total_len = 8 + (msg.length as usize);
        if out_buf.len() < total_len {
            return Err(DoIpError::MessageTooLarge);
        }

        out_buf[0] = DOIP_PROTOCOL_VERSION;
        out_buf[1] = DOIP_INVERSE_PROTOCOL_VERSION;
        let pt = (msg.payload_type as u16).to_be_bytes();
        out_buf[2] = pt[0];
        out_buf[3] = pt[1];
        let len_bytes = msg.length.to_be_bytes();
        out_buf[4..8].copy_from_slice(&len_bytes);
        out_buf[8..total_len].copy_from_slice(&msg.payload[..msg.length as usize]);

        Ok(total_len)
    }

    /// Deserializes a raw network stream into a structured DoIP message
    pub fn deserialize_packet(raw_data: &[u8], out_msg: &mut DoIpMessage) -> Result<(), DoIpError> {
        if raw_data.len() < 8 {
            return Err(DoIpError::IncorrectPatternFormat);
        }

        if raw_data[0] != DOIP_PROTOCOL_VERSION || raw_data[1] != DOIP_INVERSE_PROTOCOL_VERSION {
            return Err(DoIpError::IncorrectPatternFormat);
        }

        let p_type = u16::from_be_bytes([raw_data[2], raw_data[3]]);
        let len = u32::from_be_bytes([raw_data[4], raw_data[5], raw_data[6], raw_data[7]]);

        if (raw_data.len() - 8) < (len as usize) {
            return Err(DoIpError::IncorrectPatternFormat);
        }
        if len as usize > MAX_DOIP_PAYLOAD_SIZE {
            return Err(DoIpError::MessageTooLarge);
        }

        out_msg.payload_type = DoIpPayloadType::from(p_type);
        out_msg.length = len;
        out_msg.payload[..len as usize].copy_from_slice(&raw_data[8..8 + len as usize]);

        Ok(())
    }

    /// Processes routing activation request and assigns client channel
    pub fn handle_routing_activation(
        &self,
        client_address: u16,
        activation_type: u8,
        response_msg: &mut DoIpMessage,
    ) -> Result<(), DoIpError> {
        let mut assigned = false;

        for ch in self.channels.iter() {
            if !ch.is_active.load(Ordering::Relaxed) {
                ch.client_logical_addr
                    .store(client_address, Ordering::Relaxed);
                ch.activation_type.store(activation_type, Ordering::Relaxed);
                ch.is_active.store(true, Ordering::Release);
                assigned = true;
                break;
            }
        }

        response_msg.payload_type = DoIpPayloadType::RoutingActivationResponse;
        response_msg.length = 9;

        // Logical Address of Tester (2 bytes)
        response_msg.payload[0..2].copy_from_slice(&client_address.to_be_bytes());
        // Logical Address of ECU (2 bytes)
        response_msg.payload[2..4].copy_from_slice(&self.ecu_logical_address.to_be_bytes());
        // Response Code (0x10 = Routing successfully activated, 0x00 = Denied)
        response_msg.payload[4] = if assigned { 0x10 } else { 0x00 };
        // Reserved (4 bytes)
        response_msg.payload[5..9].fill(0x00);

        if !assigned {
            return Err(DoIpError::RoutingActivationDenied);
        }
        Ok(())
    }

    /// Builds a Vehicle Announcement Message broadcasted when Ethernet link comes up
    pub fn build_vehicle_announcement(&self, response_msg: &mut DoIpMessage) {
        response_msg.payload_type = DoIpPayloadType::VehicleAnnouncementMessage;
        response_msg.length = 32;

        // VIN (17 bytes)
        response_msg.payload[0..17].copy_from_slice(&self.vin);
        // Logical Address (2 bytes)
        response_msg.payload[17..19].copy_from_slice(&self.ecu_logical_address.to_be_bytes());
        // EID (6 bytes)
        response_msg.payload[19..25].copy_from_slice(&self.entity_id);
        // GID (6 bytes)
        response_msg.payload[25..31].copy_from_slice(&self.group_id);
        // Further Action Required (0x00 = None)
        response_msg.payload[31] = 0x00;
    }
}

// =================================================================================================
// 6. AUTOSAR NETWORK MANAGEMENT (CANNM / UDPNM FINITE STATE MACHINE)
// =================================================================================================

pub struct NetworkManagementController {
    current_state: AtomicU8,
    nm_timer_micros: AtomicU64,
    repeat_message_timer_micros: AtomicU64,
    node_id: u8,
    network_requested: AtomicBool,
}

impl NetworkManagementController {
    pub const NM_TIMEOUT_MICROS: u64 = 1_000_000; // 1s
    pub const REPEAT_MSG_TIME_MICROS: u64 = 400_000; // 400ms

    pub const fn new(node_id: u8) -> Self {
        Self {
            current_state: AtomicU8::new(NmState::BusSleep as u8),
            nm_timer_micros: AtomicU64::new(0),
            repeat_message_timer_micros: AtomicU64::new(0),
            node_id,
            network_requested: AtomicBool::new(false),
        }
    }

    pub fn get_state(&self) -> NmState {
        match self.current_state.load(Ordering::Acquire) {
            0 => NmState::BusSleep,
            1 => NmState::PrepareBusSleep,
            2 => NmState::RepeatMessage,
            3 => NmState::NormalOperation,
            _ => NmState::ReadySleep,
        }
    }

    pub fn request_network(&self) {
        self.network_requested.store(true, Ordering::SeqCst);
    }

    pub fn release_network(&self) {
        self.network_requested.store(false, Ordering::SeqCst);
    }

    /// Evaluates cyclic timers and transitions the NM state machine
    pub fn step_cycle(&self, current_time: u64) -> Option<[u8; NM_PDU_SIZE]> {
        let state = self.get_state();
        let is_requested = self.network_requested.load(Ordering::Relaxed);

        match state {
            NmState::BusSleep => {
                if is_requested {
                    // Transition to Network Mode -> Repeat Message State
                    self.current_state
                        .store(NmState::RepeatMessage as u8, Ordering::Release);
                    self.repeat_message_timer_micros.store(
                        current_time + Self::REPEAT_MSG_TIME_MICROS,
                        Ordering::Relaxed,
                    );
                    self.nm_timer_micros
                        .store(current_time + Self::NM_TIMEOUT_MICROS, Ordering::Relaxed);
                    return Some(self.build_nm_pdu(0x01)); // Active wake-up bit set
                }
            }
            NmState::RepeatMessage => {
                let rpt_deadline = self.repeat_message_timer_micros.load(Ordering::Relaxed);
                if current_time >= rpt_deadline {
                    if is_requested {
                        self.current_state
                            .store(NmState::NormalOperation as u8, Ordering::Release);
                    } else {
                        self.current_state
                            .store(NmState::ReadySleep as u8, Ordering::Release);
                    }
                }
                return Some(self.build_nm_pdu(0x00));
            }
            NmState::NormalOperation => {
                if !is_requested {
                    self.current_state
                        .store(NmState::ReadySleep as u8, Ordering::Release);
                }
                // Reset timeout timer during normal operations
                self.nm_timer_micros
                    .store(current_time + Self::NM_TIMEOUT_MICROS, Ordering::Relaxed);
                return Some(self.build_nm_pdu(0x00));
            }
            NmState::ReadySleep => {
                if is_requested {
                    self.current_state
                        .store(NmState::NormalOperation as u8, Ordering::Release);
                } else {
                    let deadline = self.nm_timer_micros.load(Ordering::Relaxed);
                    if current_time >= deadline {
                        // NM-Timeout expired, transition to Prepare Bus Sleep
                        self.current_state
                            .store(NmState::PrepareBusSleep as u8, Ordering::Release);
                        self.nm_timer_micros.store(
                            current_time + Self::REPEAT_MSG_TIME_MICROS,
                            Ordering::Relaxed,
                        );
                    }
                }
            }
            NmState::PrepareBusSleep => {
                if is_requested {
                    self.current_state
                        .store(NmState::RepeatMessage as u8, Ordering::Release);
                } else {
                    let deadline = self.nm_timer_micros.load(Ordering::Relaxed);
                    if current_time >= deadline {
                        self.current_state
                            .store(NmState::BusSleep as u8, Ordering::Release);
                    }
                }
            }
        }
        None
    }

    fn build_nm_pdu(&self, control_bit_vector: u8) -> [u8; NM_PDU_SIZE] {
        let mut pdu = [0u8; NM_PDU_SIZE];
        pdu[0] = self.node_id;
        pdu[1] = control_bit_vector; // CBV (Repeat Message Bit, Active Wake-up Bit)
        pdu
    }
}

// =================================================================================================
// 7. ECU STATE MANAGER (ECUM) & BASIC SOFTWARE MODE MANAGER (BSWM)
// =================================================================================================

pub struct EcuStateManager {
    current_power_state: AtomicU8,
    wake_up_reason: AtomicU32,
    ignition_clamp15_active: AtomicBool,
}

impl EcuStateManager {
    pub const WAKEUP_REASON_CAN: u32 = 0x0000_0001;
    pub const WAKEUP_REASON_ETH: u32 = 0x0000_0002;
    pub const WAKEUP_REASON_IGNITION: u32 = 0x0000_0004;

    pub const fn new() -> Self {
        Self {
            current_power_state: AtomicU8::new(EcuPowerState::Startup as u8),
            wake_up_reason: AtomicU32::new(0),
            ignition_clamp15_active: AtomicBool::new(false),
        }
    }

    pub fn get_power_state(&self) -> EcuPowerState {
        match self.current_power_state.load(Ordering::Acquire) {
            0 => EcuPowerState::Off,
            1 => EcuPowerState::Startup,
            2 => EcuPowerState::Run,
            3 => EcuPowerState::PostRun,
            4 => EcuPowerState::Standby,
            5 => EcuPowerState::Sleep,
            _ => EcuPowerState::EmergencySafeShutdown,
        }
    }

    pub fn notify_wakeup(&self, reason: u32) {
        self.wake_up_reason.fetch_or(reason, Ordering::SeqCst);
        let state = self.get_power_state();
        if state == EcuPowerState::Sleep || state == EcuPowerState::Standby {
            self.current_power_state
                .store(EcuPowerState::Startup as u8, Ordering::Release);
        }
    }

    pub fn set_clamp15(&self, active: bool) {
        self.ignition_clamp15_active.store(active, Ordering::SeqCst);
        if active {
            self.notify_wakeup(Self::WAKEUP_REASON_IGNITION);
        }
    }

    /// Core state machine dispatch loop
    pub fn step_state_machine(&self) {
        let state = self.get_power_state();
        let clamp15 = self.ignition_clamp15_active.load(Ordering::Relaxed);

        match state {
            EcuPowerState::Startup => {
                // Perform non-allocating hardware peripheral initialization
                self.current_power_state
                    .store(EcuPowerState::Run as u8, Ordering::Release);
            }
            EcuPowerState::Run => {
                if !clamp15 {
                    // Driver turned vehicle ignition off, enter PostRun phase
                    self.current_power_state
                        .store(EcuPowerState::PostRun as u8, Ordering::Release);
                }
            }
            EcuPowerState::PostRun => {
                if clamp15 {
                    // Re-ignition occurred before powerdown
                    self.current_power_state
                        .store(EcuPowerState::Run as u8, Ordering::Release);
                } else {
                    // Teardown services, persist non-volatile diagnostic logs, go to sleep
                    self.current_power_state
                        .store(EcuPowerState::Sleep as u8, Ordering::Release);
                }
            }
            EcuPowerState::Sleep => {
                // Waiting for wake-up interrupts (e.g. CAN transceiver or DoIP activity)
            }
            EcuPowerState::Standby | EcuPowerState::Off => {}
            EcuPowerState::EmergencySafeShutdown => {
                // Hard isolated state: actuators disconnected via panic line
            }
        }
    }

    pub fn trigger_emergency_shutdown(&self) {
        self.current_power_state
            .store(EcuPowerState::EmergencySafeShutdown as u8, Ordering::SeqCst);
    }
}

impl Default for EcuStateManager {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. PART 5 INTEGRATION SUITE & VERIFICATION TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secoc_authentication_flow() {
        let key = [
            0x2B, 0x7E, 0x15, 0x16, 0x28, 0xAE, 0xD2, 0xA6, 0xAB, 0xF7, 0x15, 0x88, 0x09, 0xCF,
            0x4F, 0x3C,
        ];
        let secoc = SecOcEngine::new(key);

        let test_payload = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let mut pdu = SecOcPdu {
            pdu_id: 0x120,
            payload_len: 0,
            payload: [0u8; MAX_SECOC_PDU_PAYLOAD],
            freshness_value: 0,
            authenticator_mac: [0u8; SECOC_MAC_LEN_BYTES],
        };

        // Generation
        assert_eq!(
            secoc.transmit_secured_pdu(0x120, &test_payload, &mut pdu),
            Ok(())
        );
        assert_eq!(pdu.payload_len, 8);

        // Verification
        assert_eq!(secoc.receive_verify_pdu(&pdu), Ok(()));

        // Tamper test
        pdu.payload[0] ^= 0xFF;
        assert_eq!(
            secoc.receive_verify_pdu(&pdu),
            Err(SecOcError::AuthenticationFailure)
        );
    }

    #[test]
    fn test_doip_routing_activation() {
        let vin = *b"1HGBH41JXMN109186";
        let mac = [0x02, 0x00, 0x5E, 0x00, 0x53, 0x01];
        let server = DoIpServer::new(0x0E00, vin, mac);

        let mut resp = DoIpMessage::empty();
        let res = server.handle_routing_activation(0x0E80, 0x00, &mut resp);

        assert!(res.is_ok());
        assert_eq!(
            resp.payload_type,
            DoIpPayloadType::RoutingActivationResponse
        );
        assert_eq!(resp.payload[4], 0x10); // Successfully activated
    }

    #[test]
    fn test_nm_state_machine_transition() {
        let nm = NetworkManagementController::new(0x42);
        assert_eq!(nm.get_state(), NmState::BusSleep);

        nm.request_network();
        let pdu = nm.step_cycle(10_000);
        assert!(pdu.is_some());
        assert_eq!(nm.get_state(), NmState::RepeatMessage);
    }

    #[test]
    fn test_ecu_power_state_transition() {
        let ecum = EcuStateManager::new();
        assert_eq!(ecum.get_power_state(), EcuPowerState::Startup);

        ecum.step_state_machine();
        assert_eq!(ecum.get_power_state(), EcuPowerState::Run);

        ecum.set_clamp15(false);
        ecum.step_state_machine();
        assert_eq!(ecum.get_power_state(), EcuPowerState::PostRun);
    }
}

#[cfg(test)]
mod cmac_reference_vectors {
    use super::*;

    fn unhex(h: &str) -> Vec<u8> {
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    fn mac(key: &[u8; 16], msg: &[u8]) -> String {
        let mut out = [0u8; 16];
        HardwareSecurityModule::compute_cmac_aes128(key, msg, &mut out);
        out.iter().map(|b| format!("{b:02x}")).collect()
    }

    const RFC_KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
    const RFC_MSG: &str = "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710";

    // RFC 4493 section 4 examples (message prefix length, CMAC), also confirmed independently.
    const RFC4493: [(usize, &str); 4] = [
        (0, "bb1d6929e95937287fa37d129b756746"),
        (16, "070a16b46b4d4144f79bdd9dd04a287c"),
        (40, "dfa66747de9ae63030ca32611497c827"),
        (64, "51f0bebf7e3b9d92fc49741779363cfe"),
    ];

    // CMACs from Python `cryptography` for key bytes (i*5+1) and message bytes (i*11+7), chosen to
    // straddle every block boundary (empty, partial, exact and multi-block messages).
    const GENERATED: [(usize, &str); 15] = [
        (0, "f1eaa39174ba36560a256ccf227d0952"),
        (1, "5e941cb59ae2900458f48b0a9041050c"),
        (15, "3cf794d5520c6bbce34a7348bd384478"),
        (16, "c61a28d619e59b329161ea68daacee94"),
        (17, "22f9ec73ce9ffa303a7949702177ddc4"),
        (31, "52f038b75e7cf0246736fd2eb5a22ed5"),
        (32, "9e2c43c33cde7ae014b8e0d9545b0776"),
        (33, "12b2d7ea127e4e60044fea99eabeabb7"),
        (63, "4ad4a9cd9d44e1f53e3a670b57abdb8f"),
        (64, "35ae4551adb350ba458043966a4b32c7"),
        (65, "7660588887442a9f58be3020e9a4d3c1"),
        (100, "eee4418637d7aefd0d1c99259d52df31"),
        (127, "9cf9354793befeb5ca68d79640647e2d"),
        (128, "00e84f7082946fc83df28c01df93885a"),
        (129, "00bdc7f8e620feb4f9ca88ea9122aa08"),
    ];

    #[test]
    fn rfc4493_examples() {
        let key: [u8; 16] = unhex(RFC_KEY).try_into().unwrap();
        let msg = unhex(RFC_MSG);
        for (n, want) in RFC4493 {
            assert_eq!(mac(&key, &msg[..n]), want, "length {n}");
        }
    }

    #[test]
    fn matches_an_independent_implementation_at_every_block_boundary() {
        let key: [u8; 16] = core::array::from_fn(|i| ((i * 5 + 1) & 0xFF) as u8);
        for (n, want) in GENERATED {
            let msg: Vec<u8> = (0..n).map(|i| ((i * 11 + 7) & 0xFF) as u8).collect();
            assert_eq!(mac(&key, &msg), want, "length {n}");
        }
    }

    #[test]
    fn aes128_matches_fips_197_appendix_c() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let pt: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        let ct = Aes128::new(&key).encrypt_block(&pt);
        let hex: String = ct.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "69c4e0d86a7b0430d8cdb78070b4c55a");
        // FIPS 197 appendix B example.
        let key = unhex("2b7e151628aed2a6abf7158809cf4f3c");
        let pt = unhex("3243f6a8885a308d313198a2e0370734");
        let ct = Aes128::new(&key.try_into().unwrap()).encrypt_block(&pt.try_into().unwrap());
        let hex: String = ct.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "3925841d02dc09fbdc118597196a0b32");
    }

    #[test]
    fn algebraic_sbox_matches_the_published_table_at_known_points() {
        // Well-known S-box entries from FIPS 197 figure 7.
        for (x, y) in [
            (0x00u8, 0x63u8),
            (0x01, 0x7c),
            (0x02, 0x77),
            (0x53, 0xed),
            (0x7f, 0xd2),
            (0xff, 0x16),
            (0xc9, 0xdd),
            (0x10, 0xca),
        ] {
            assert_eq!(sbox(x), y, "S({x:#04x})");
        }
        // It is a permutation of all 256 byte values.
        let mut seen = [false; 256];
        for x in 0..=255u8 {
            seen[usize::from(sbox(x))] = true;
        }
        assert!(seen.iter().all(|&b| b));
    }

    #[test]
    fn any_bit_flip_changes_the_mac() {
        let key = [0x11u8; 16];
        let msg = [0xA5u8; 40];
        let base = mac(&key, &msg);
        for bit in 0..(msg.len() * 8) {
            let mut m = msg;
            m[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(mac(&key, &m), base, "message bit {bit}");
        }
        let mut k = key;
        k[0] ^= 1;
        assert_ne!(mac(&k, &msg), base);
    }
}
