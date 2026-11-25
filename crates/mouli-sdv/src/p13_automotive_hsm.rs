//! Part 13: AUTOMOTIVE HSM, NIST SP 800-90A HMAC-DRBG, ED25519 & SECURE KEYSTORE

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 13 - AUTOMOTIVE HSM, NIST SP 800-90A HMAC-DRBG, ED25519 & SECURE KEYSTORE
// CRITICALITY: ISO 26262 ASIL-D / ISO/SAE 21434 / NIST SP 800-90A / FIPS 186-5
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Constant-Time Cryptographic Execution
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

// =================================================================================================
// 1. CONSTANTS: CRYPTOGRAPHIC SIZES, KEY SLOTS & HSM REGISTERS
// =================================================================================================

/// Total isolated cryptographic hardware key slots in the vehicle HSM
pub const HSM_MAX_KEY_SLOTS: usize = 32;

/// Maximum payload supported in a single hardware mailbox command
pub const HSM_MAILBOX_PAYLOAD_CAPACITY: usize = 256;

/// HMAC-SHA256 Output Digest and Key Size (32 bytes / 256 bits)
pub const HMAC_SHA256_SIZE: usize = 32;

/// SHA-512 Output Digest Length (64 bytes / 512 bits)
pub const SHA512_DIGEST_LENGTH: usize = 64;
pub const SHA512_BLOCK_SIZE: usize = 128;

/// Ed25519 Parameters (RFC 8032)
pub const ED25519_PUBLIC_KEY_SIZE: usize = 32;
pub const ED25519_SIGNATURE_SIZE: usize = 64;
pub const ED25519_PRIVATE_KEY_SEED_SIZE: usize = 32;

/// NIST SP 800-90A DRBG Limits
pub const DRBG_MAX_REQUEST_BYTES: usize = 65536;
pub const DRBG_RESEED_INTERVAL: u64 = 100_000; // Force reseed after 100k requests

// =================================================================================================
// 2. CRYPTOGRAPHIC ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HsmError {
    Ok = 0,
    MailboxBusy = 1,
    MailboxTimeout = 2,
    InvalidKeySlot = 3,
    KeySlotLocked = 4,
    KeySlotEmpty = 5,
    KeyUsageForbidden = 6,
    SignatureVerificationFailed = 7,
    EntropyDepleted = 8,
    DrbgReseedRequired = 9,
    BufferOverflow = 10,
    CryptoHardwareFault = 11,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClassification {
    Unassigned = 0,
    SymmetricMacKey = 1,     // AES-128 / HMAC-SHA256
    SymmetricPayloadKey = 2, // AES-GCM / XTS
    AsymmetricIdentity = 3,  // Ed25519 / ECC P-256 Public
    AsymmetricRootCa = 4,    // OEM Root Trust Anchor
    OtaVerificationKey = 5,  // Uptane Targets Signer
    V2xPseudonymKey = 6,     // IEEE 1609.2 Ephemeral
}

// =================================================================================================
// 3. CONSTANT-TIME UTILITIES (SIDE-CHANNEL HARDENING)
// =================================================================================================

/// Constant-time comparison to prevent timing side-channel attacks on secret keys and MACs
#[inline(never)]
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

/// Constant-time memory clearing for cryptographic key hygiene
#[inline(never)]
pub fn secure_zero_memory(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        unsafe {
            core::ptr::write_volatile(byte, 0);
        }
    }
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

// =================================================================================================
// 4. BARE-METAL SHA-512 ENGINE (FOR ED25519 DIGITAL SIGNATURES)
// =================================================================================================

/// Non-allocating SHA-512 state engine (FIPS PUB 180-4)
pub struct Sha512Engine {
    state: [u64; 8],
    total_len_bytes: u128,
    buffer: [u8; SHA512_BLOCK_SIZE],
    buffer_len: usize,
}

impl Sha512Engine {
    const K: [u64; 80] = [
        0x428a2f98d728ae22,
        0x7137449123ef65cd,
        0xb5c0fbcfec4d3b2f,
        0xe9b5dba58189dbbc,
        0x3956c25bf348b538,
        0x59f111f1b605d019,
        0x923f82a4af194f9b,
        0xab1c5ed5da6d8118,
        0xd807aa98a3030242,
        0x12835b0145706fbe,
        0x243185be4ee4b28c,
        0x550c7dc3d5ffb4e2,
        0x72be5d74f27b896f,
        0x80deb1fe3b1696b1,
        0x9bdc06a725c71235,
        0xc19bf174cf692694,
        0xe49b69c19ef14ad2,
        0xefbe4786384f25e3,
        0x0fc19dc68b8cd5b5,
        0x240ca1cc77ac9c65,
        0x2de92c6f592b0275,
        0x4a7484aa6ea6e483,
        0x5cb0a9dcbd41fbd4,
        0x76f988da831153b5,
        0x983e5152ee66dfab,
        0xa831c66d2db43210,
        0xb00327c898fb213f,
        0xbf597fc7beef0ee4,
        0xc6e00bf33da88fc2,
        0xd5a79147930aa725,
        0x06ca6351e003826f,
        0x142929670a0e6e70,
        0x27b70a8546d22ffc,
        0x2e1b21385c26c926,
        0x4d2c6dfc5ac42aed,
        0x53380d139d95b3df,
        0x650a73548baf63de,
        0x766a0abb3c77b2a8,
        0x81c2c92e47edaee6,
        0x92722c851482353b,
        0xa2bfe8a14cf10364,
        0xa81a664bbc423001,
        0xc24b8b70d0f89791,
        0xc76c51a30654be30,
        0xd192e819d6ef5218,
        0xd69906245565a910,
        0xf40e35855771202a,
        0x106aa07032bbd1b8,
        0x19a4c116b8d2d0c8,
        0x1e376c085141ab53,
        0x2748774cdf8eeb99,
        0x34b0bcb5e19b48a8,
        0x391c0cb3c5c95a63,
        0x4ed8aa4ae3418acb,
        0x5b9cca4f7763e373,
        0x682e6ff3d6b2b8a3,
        0x748f82ee5defb2fc,
        0x78a5636f43172f60,
        0x84c87814a1f0ab72,
        0x8cc702081a6439ec,
        0x90befffa23631e28,
        0xa4506cebde82bde9,
        0xbef9a3f7b2c67915,
        0xc67178f2e372532b,
        0xca273eceea26619c,
        0xd186b8c721c0c207,
        0xeada7dd6cde0eb1e,
        0xf57d4f7fee6ed178,
        0x06f067aa72176fba,
        0x0a637dc5a2c898a6,
        0x113f9804bef90dae,
        0x1b710b35131c471b,
        0x28db77f523047d84,
        0x32caab7b40c72493,
        0x3c9ebe0a15c9bebc,
        0x431d67c49c100d4c,
        0x4cc5d4becb3e42b6,
        0x597f299cfc657e2a,
        0x5fcb6fab3ad6faec,
        0x6c44198c4a475817,
    ];

    pub const fn new() -> Self {
        Self {
            state: [
                0x6a09e667f3bcc908,
                0xbb67ae8584caa73b,
                0x3c6ef372fe94f82b,
                0xa54ff53a5f1d36f1,
                0x510e527fade682d1,
                0x9b05688c2b3e6c1f,
                0x1f83d9abfb41bd6b,
                0x5be0cd19137e2179,
            ],
            total_len_bytes: 0,
            buffer: [0u8; SHA512_BLOCK_SIZE],
            buffer_len: 0,
        }
    }

    #[inline(always)]
    fn rotr(x: u64, n: u32) -> u64 {
        x.rotate_right(n)
    }

    fn transform_block(&mut self, block: &[u8; 128]) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut chunk = [0u8; 8];
            chunk.copy_from_slice(&block[i * 8..i * 8 + 8]);
            w[i] = u64::from_be_bytes(chunk);
        }

        for i in 16..80 {
            let s0 = Self::rotr(w[i - 15], 1) ^ Self::rotr(w[i - 15], 8) ^ (w[i - 15] >> 7);
            let s1 = Self::rotr(w[i - 2], 19) ^ Self::rotr(w[i - 2], 61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..80 {
            let s1 = Self::rotr(e, 14) ^ Self::rotr(e, 18) ^ Self::rotr(e, 41);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(Self::K[i])
                .wrapping_add(w[i]);
            let s0 = Self::rotr(a, 28) ^ Self::rotr(a, 34) ^ Self::rotr(a, 39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    pub fn update(&mut self, data: &[u8]) {
        self.total_len_bytes += data.len() as u128;
        let mut offset = 0;

        while offset < data.len() {
            let to_copy = core::cmp::min(data.len() - offset, SHA512_BLOCK_SIZE - self.buffer_len);
            self.buffer[self.buffer_len..self.buffer_len + to_copy]
                .copy_from_slice(&data[offset..offset + to_copy]);
            self.buffer_len += to_copy;
            offset += to_copy;

            if self.buffer_len == SHA512_BLOCK_SIZE {
                let blk = self.buffer;
                self.transform_block(&blk);
                self.buffer_len = 0;
            }
        }
    }

    pub fn finalize(mut self, out_digest: &mut [u8; SHA512_DIGEST_LENGTH]) {
        let bit_len = self.total_len_bytes * 8;
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;

        if self.buffer_len > 112 {
            self.buffer[self.buffer_len..SHA512_BLOCK_SIZE].fill(0);
            let blk = self.buffer;
            self.transform_block(&blk);
            self.buffer.fill(0);
        } else {
            self.buffer[self.buffer_len..112].fill(0);
        }

        self.buffer[112..128].copy_from_slice(&bit_len.to_be_bytes());
        let blk = self.buffer;
        self.transform_block(&blk);

        for (i, &val) in self.state.iter().enumerate() {
            out_digest[i * 8..i * 8 + 8].copy_from_slice(&val.to_be_bytes());
        }
    }
}

impl Default for Sha512Engine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. NIST SP 800-90A HMAC-DRBG (PSEUDO-RANDOM BIT GENERATOR)
// =================================================================================================

/// HMAC-SHA256 Primitive for HMAC-DRBG
pub fn hmac_sha256(key: &[u8], data: &[u8], out_mac: &mut [u8; HMAC_SHA256_SIZE]) {
    let mut k_pad = [0u8; 64];
    if key.len() > 64 {
        // Hash key if longer than block
        let mut hash = crate::p09_nvme_storage_driver::Sha256Engine::new();
        hash.update(key);
        let mut short_key = [0u8; 32];
        hash.finalize(&mut short_key);
        k_pad[..32].copy_from_slice(&short_key);
    } else {
        k_pad[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = k_pad[i] ^ 0x36;
        opad[i] = k_pad[i] ^ 0x5c;
    }

    // Inner hash: H(ipad || data)
    let mut inner = crate::p09_nvme_storage_driver::Sha256Engine::new();
    inner.update(&ipad);
    inner.update(data);
    let mut inner_digest = [0u8; 32];
    inner.finalize(&mut inner_digest);

    // Outer hash: H(opad || inner_digest)
    let mut outer = crate::p09_nvme_storage_driver::Sha256Engine::new();
    outer.update(&opad);
    outer.update(&inner_digest);
    outer.finalize(out_mac);
}

/// NIST SP 800-90A HMAC-DRBG State
pub struct HmacDrbg {
    key: [u8; 32],
    v: [u8; 32],
    reseed_counter: AtomicU64,
    is_initialized: AtomicBool,
}

impl HmacDrbg {
    pub const fn new() -> Self {
        Self {
            key: [0u8; 32],
            v: [0u8; 32],
            reseed_counter: AtomicU64::new(0),
            is_initialized: AtomicBool::new(false),
        }
    }

    pub fn instantiate(&mut self, entropy: &[u8], nonce: &[u8], personalization: &[u8]) {
        self.key = [0x00u8; 32];
        self.v = [0x01u8; 32];

        // Seed Material concatenation
        let mut seed_material = [0u8; 128];
        let mut len = 0;

        let to_copy = core::cmp::min(entropy.len(), 64);
        seed_material[len..len + to_copy].copy_from_slice(&entropy[..to_copy]);
        len += to_copy;

        let to_copy = core::cmp::min(nonce.len(), 32);
        seed_material[len..len + to_copy].copy_from_slice(&nonce[..to_copy]);
        len += to_copy;

        let to_copy = core::cmp::min(personalization.len(), 32);
        seed_material[len..len + to_copy].copy_from_slice(&personalization[..to_copy]);
        len += to_copy;

        self.update(Some(&seed_material[..len]));
        self.reseed_counter.store(1, Ordering::SeqCst);
        self.is_initialized.store(true, Ordering::SeqCst);
    }

    fn update(&mut self, provided_data: Option<&[u8]>) {
        // Step 1: K = HMAC(K, V || 0x00 || provided_data)
        let mut input1 = [0u8; 160];
        input1[..32].copy_from_slice(&self.v);
        input1[32] = 0x00;
        let mut in_len = 33;
        if let Some(data) = provided_data {
            let c = core::cmp::min(data.len(), 120);
            input1[in_len..in_len + c].copy_from_slice(&data[..c]);
            in_len += c;
        }
        {
            let mut tmp = [0u8; HMAC_SHA256_SIZE];
            hmac_sha256(&self.key, &input1[..in_len], &mut tmp);
            self.key = tmp;
        }

        // Step 2: V = HMAC(K, V)
        {
            let mut tmp = [0u8; HMAC_SHA256_SIZE];
            hmac_sha256(&self.key, &self.v, &mut tmp);
            self.v = tmp;
        }

        if let Some(data) = provided_data {
            // Step 4: K = HMAC(K, V || 0x01 || provided_data)
            let mut input2 = [0u8; 160];
            input2[..32].copy_from_slice(&self.v);
            input2[32] = 0x01;
            let mut in2_len = 33;
            let c = core::cmp::min(data.len(), 120);
            input2[in2_len..in2_len + c].copy_from_slice(&data[..c]);
            in2_len += c;
            {
                let mut tmp = [0u8; HMAC_SHA256_SIZE];
                hmac_sha256(&self.key, &input2[..in2_len], &mut tmp);
                self.key = tmp;
            }

            // Step 5: V = HMAC(K, V)
            {
                let mut tmp = [0u8; HMAC_SHA256_SIZE];
                hmac_sha256(&self.key, &self.v, &mut tmp);
                self.v = tmp;
            }
        }
    }

    pub fn generate(&mut self, out_bytes: &mut [u8]) -> Result<(), HsmError> {
        if !self.is_initialized.load(Ordering::Relaxed) {
            return Err(HsmError::CryptoHardwareFault);
        }
        let counter = self.reseed_counter.load(Ordering::Relaxed);
        if counter > DRBG_RESEED_INTERVAL {
            return Err(HsmError::DrbgReseedRequired);
        }

        let mut generated = 0;
        while generated < out_bytes.len() {
            {
                let mut tmp = [0u8; HMAC_SHA256_SIZE];
                hmac_sha256(&self.key, &self.v, &mut tmp);
                self.v = tmp;
            }
            let to_copy = core::cmp::min(out_bytes.len() - generated, 32);
            out_bytes[generated..generated + to_copy].copy_from_slice(&self.v[..to_copy]);
            generated += to_copy;
        }

        self.update(None);
        self.reseed_counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Default for HmacDrbg {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. CONSTANT-TIME ED25519 VERIFICATION (RFC 8032)
// =================================================================================================

/// Ed25519 signature verification (RFC 8032) using the workspace-approved `ed25519-dalek`.
///
/// This replaces an earlier placeholder that accepted every signature. Verification is strict:
/// it rejects non-canonical signatures, malformed public keys and small-order (weak) keys.
pub struct Ed25519Verifier;

impl Ed25519Verifier {
    /// Checks `signature` over `message` against `public_key`.
    pub fn verify(
        public_key: &[u8; ED25519_PUBLIC_KEY_SIZE],
        message: &[u8],
        signature: &[u8; ED25519_SIGNATURE_SIZE],
    ) -> Result<(), HsmError> {
        use ed25519_dalek::{Signature, VerifyingKey};
        let key = VerifyingKey::from_bytes(public_key)
            .map_err(|_| HsmError::SignatureVerificationFailed)?;
        let sig = Signature::from_bytes(signature);
        key.verify_strict(message, &sig)
            .map_err(|_| HsmError::SignatureVerificationFailed)
    }
}

// =================================================================================================
// 7. SECURE KEYSTORE & HARDWARE KEY SLOT ENGINE
// =================================================================================================

/// Single Cryptographic Key Descriptor in Isolated HSM Memory
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HsmKeySlot {
    pub slot_id: u8,
    pub classification: KeyClassification,
    pub is_locked: bool,
    pub exportable: bool,
    pub usage_counter: u32,
    pub key_bytes: [u8; 32],
}

impl HsmKeySlot {
    pub const fn empty(id: u8) -> Self {
        Self {
            slot_id: id,
            classification: KeyClassification::Unassigned,
            is_locked: false,
            exportable: false,
            usage_counter: 0,
            key_bytes: [0u8; 32],
        }
    }
}

/// ASIL-D Compliant Hardware Cryptographic Keystore
pub struct SecureKeyStore {
    slots: UnsafeCell<[HsmKeySlot; HSM_MAX_KEY_SLOTS]>,
    lock: AtomicBool,
}

unsafe impl Sync for SecureKeyStore {}

impl SecureKeyStore {
    pub const fn new() -> Self {
        let mut slots = [HsmKeySlot::empty(0); HSM_MAX_KEY_SLOTS];
        let mut idx = 0;
        while idx < HSM_MAX_KEY_SLOTS {
            slots[idx].slot_id = idx as u8;
            idx += 1;
        }
        Self {
            slots: UnsafeCell::new(slots),
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

    /// Injects an authenticated OEM cryptographic key into a hardware slot
    pub fn provision_key(
        &self,
        slot_id: usize,
        classification: KeyClassification,
        key_data: &[u8; 32],
        is_locked: bool,
    ) -> Result<(), HsmError> {
        if slot_id >= HSM_MAX_KEY_SLOTS {
            return Err(HsmError::InvalidKeySlot);
        }

        self.acquire();
        let slots = unsafe { &mut *self.slots.get() };
        let slot = &mut slots[slot_id];

        if slot.is_locked {
            self.release();
            return Err(HsmError::KeySlotLocked);
        }

        slot.classification = classification;
        slot.key_bytes.copy_from_slice(key_data);
        slot.is_locked = is_locked;
        slot.usage_counter = 0;

        self.release();
        Ok(())
    }

    /// Accesses a key for internal operations without exposing raw memory
    pub fn execute_with_key<F>(
        &self,
        slot_id: usize,
        allowed_type: KeyClassification,
        op: F,
    ) -> Result<(), HsmError>
    where
        F: FnOnce(&[u8; 32]) -> Result<(), HsmError>,
    {
        if slot_id >= HSM_MAX_KEY_SLOTS {
            return Err(HsmError::InvalidKeySlot);
        }

        self.acquire();
        let slots = unsafe { &mut *self.slots.get() };
        let slot = &mut slots[slot_id];

        if slot.classification != allowed_type {
            self.release();
            return Err(HsmError::KeyUsageForbidden);
        }

        slot.usage_counter = slot.usage_counter.saturating_add(1);
        let res = op(&slot.key_bytes);
        self.release();

        res
    }
}

impl Default for SecureKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. AUTOMOTIVE HARDWARE SECURITY MODULE (HSM) MAILBOX INTERFACE
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HsmCommandOpcode {
    Nop = 0x0000,
    GenerateRandom = 0x0010,
    DeriveKey = 0x0020,
    SignEd25519 = 0x0030,
    VerifyEd25519 = 0x0031,
    ComputeHmac = 0x0040,
    ZeroizeSlot = 0x00FF,
}

/// Memory-Mapped Inter-Core Mailbox Register Protocol
#[repr(C, align(64))]
pub struct HsmMailboxController {
    keystore: SecureKeyStore,
    drbg: UnsafeCell<HmacDrbg>,
    cmd_reg: AtomicU16,
    status_reg: AtomicU16,
    cmd_payload: UnsafeCell<[u8; HSM_MAILBOX_PAYLOAD_CAPACITY]>,
    resp_payload: UnsafeCell<[u8; HSM_MAILBOX_PAYLOAD_CAPACITY]>,
    lock: AtomicBool,
}

// `HsmMailboxController` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl HsmMailboxController {
    pub const fn new() -> Self {
        Self {
            keystore: SecureKeyStore::new(),
            drbg: UnsafeCell::new(HmacDrbg::new()),
            cmd_reg: AtomicU16::new(HsmCommandOpcode::Nop as u16),
            status_reg: AtomicU16::new(HsmError::Ok as u16),
            cmd_payload: UnsafeCell::new([0u8; HSM_MAILBOX_PAYLOAD_CAPACITY]),
            resp_payload: UnsafeCell::new([0u8; HSM_MAILBOX_PAYLOAD_CAPACITY]),
            lock: AtomicBool::new(false),
        }
    }

    /// Initializes the internal DRBG from hardware physical entropy sources (TRNG)
    pub fn seed_drbg_from_trng(&self, trng_entropy: &[u8; 32], nonce: &[u8; 16]) {
        let drbg = unsafe { &mut *self.drbg.get() };
        drbg.instantiate(trng_entropy, nonce, b"mouli_OS_HSM_BOOT");
    }

    /// Submits a command synchronously to the automotive security coprocessor
    pub fn submit_command(
        &self,
        opcode: HsmCommandOpcode,
        payload: &[u8],
        out_response: &mut [u8],
    ) -> Result<usize, HsmError> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }

        if payload.len() > HSM_MAILBOX_PAYLOAD_CAPACITY {
            self.lock.store(false, Ordering::Release);
            return Err(HsmError::BufferOverflow);
        }

        let cmd_buf = unsafe { &mut *self.cmd_payload.get() };
        cmd_buf[..payload.len()].copy_from_slice(payload);
        self.cmd_reg.store(opcode as u16, Ordering::Release);

        // Dispatch execution to secure coprocessor core
        let mut resp_len = 0;
        let mut err = HsmError::Ok;

        match opcode {
            HsmCommandOpcode::GenerateRandom => {
                let drbg = unsafe { &mut *self.drbg.get() };
                let req_len = core::cmp::min(out_response.len(), HSM_MAILBOX_PAYLOAD_CAPACITY);
                match drbg.generate(&mut out_response[..req_len]) {
                    Ok(_) => resp_len = req_len,
                    Err(e) => err = e,
                }
            }
            HsmCommandOpcode::ComputeHmac => {
                if payload.len() < 33 {
                    err = HsmError::BufferOverflow;
                } else {
                    let slot_id = payload[0] as usize;
                    let data = &payload[1..];
                    let mut mac = [0u8; HMAC_SHA256_SIZE];

                    match self.keystore.execute_with_key(
                        slot_id,
                        KeyClassification::SymmetricMacKey,
                        |key| {
                            hmac_sha256(key, data, &mut mac);
                            Ok(())
                        },
                    ) {
                        Ok(()) => {
                            let to_copy = core::cmp::min(out_response.len(), HMAC_SHA256_SIZE);
                            out_response[..to_copy].copy_from_slice(&mac[..to_copy]);
                            resp_len = to_copy;
                        }
                        Err(e) => err = e,
                    }
                }
            }
            _ => {
                err = HsmError::KeyUsageForbidden;
            }
        }

        self.status_reg.store(err as u16, Ordering::Release);
        self.cmd_reg
            .store(HsmCommandOpcode::Nop as u16, Ordering::Release);
        self.lock.store(false, Ordering::Release);

        if err == HsmError::Ok {
            Ok(resp_len)
        } else {
            Err(err)
        }
    }

    pub fn keystore(&self) -> &SecureKeyStore {
        &self.keystore
    }
}

impl Default for HsmMailboxController {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 9. VERIFICATION SUITE & UNIT TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_equality() {
        let a = [0xde, 0xad, 0xbe, 0xef];
        let b = [0xde, 0xad, 0xbe, 0xef];
        let c = [0xde, 0xad, 0xbe, 0x00];

        assert!(constant_time_eq(&a, &b));
        assert!(!constant_time_eq(&a, &c));
    }

    #[test]
    fn test_sha512_known_vector() {
        // Known vector: SHA512("abc") = ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f
        let mut hasher = Sha512Engine::new();
        hasher.update(b"abc");
        let mut digest = [0u8; 64];
        hasher.finalize(&mut digest);

        assert_eq!(digest[0], 0xdd);
        assert_eq!(digest[1], 0xaf);
        assert_eq!(digest[62], 0xa4);
        assert_eq!(digest[63], 0x9f);
    }

    #[test]
    fn test_hmac_drbg_generation() {
        let mut drbg = HmacDrbg::new();
        let entropy = [0x42u8; 32];
        let nonce = [0x12u8; 16];
        drbg.instantiate(&entropy, &nonce, b"UNIT_TEST");

        let mut random_bytes = [0u8; 32];
        assert_eq!(drbg.generate(&mut random_bytes), Ok(()));
        assert_ne!(random_bytes, [0u8; 32]);
    }

    #[test]
    fn test_keystore_provisioning_and_usage() {
        let keystore = SecureKeyStore::new();
        let key_data = [0xa5u8; 32];

        // Provision HMAC key into Slot 2
        assert_eq!(
            keystore.provision_key(2, KeyClassification::SymmetricMacKey, &key_data, true),
            Ok(())
        );

        // Attempting to overwrite locked key must fail
        assert_eq!(
            keystore.provision_key(2, KeyClassification::SymmetricMacKey, &key_data, false),
            Err(HsmError::KeySlotLocked)
        );

        // Execute valid key usage
        let mut mac = [0u8; 32];
        let exec_res = keystore.execute_with_key(2, KeyClassification::SymmetricMacKey, |k| {
            hmac_sha256(k, b"TestMessage", &mut mac);
            Ok(())
        });

        assert_eq!(exec_res, Ok(()));
        assert_ne!(mac, [0u8; 32]);
    }
}

#[cfg(test)]
mod reference_vectors {
    use super::*;
    use crate::p09_nvme_storage_driver::Sha256Engine;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    // Digests produced independently with Python's hashlib over `data(n)`.
    const SHA512_VECTORS: [(usize, &str); 16] = [
        (0, "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"),
        (1, "e45bf5817ddf94aa2f7a407071f0eedc6beb98f768b4cd33d1176d44d1563a45a5d7212290eb7670c6786b13591aedac86478993895e8b24e612014abaa6ba04"),
        (3, "8853167ab14c043919981a6409307ef68b5b726f4b80dcab259daa24b7cabcb8b4bbfd3e2e36ed98465059661f672e6529cdcc673e7eabfa78dd3186745b112a"),
        (55, "14fd424b1fcadee624da946ab03f7e1def7c0d6e00f689594319881a26ff30b875ba4c622ac13100c8cc784c9c2eb23159aecbb4a02e3999062f551193e2b256"),
        (56, "480fa85be41ef55a41208ca28ffc8743c91cf7d24758defe6f95bfb16de614fc86b701034896b047dd571de4318853d80e0809df162f1752cb26da6ddb94a0dd"),
        (63, "ecd42a703a4e93e163d60d55e3785b1a763838b0351bc2e6f7c94b4bfb24f9aa15da5d744ebcebe11f0fc4315d45ba3a047b6e60e07448357f2795bf34b73502"),
        (64, "8f3cc30b3fb5bf963688a46488249248ac2c67f0f85a145233c6c1e3c16dcd1df634c07d1d31da02576f65b9cf64e1c3fdb318b689b8a14e2e9552bcf30fb133"),
        (111, "68cffa6d0d76f309c9ce0d35280939f8e25990c43b7b086ccdf709be35b07d4ddba599541ff2b1c19d34ea49aeafb9659adb7ac3c0b078bb30a22d57fc6687ef"),
        (112, "d0865c524d1dddf7c23b799c413f5adcd7caefd3f66a9b49750ec81066012c25a8bcf94ddea6dc525691673097ca40e0101e897fc97218cfdb0704084e2bef4b"),
        (113, "606314353bd419f9f7720e8297f2af8a9ed5f0bb0bed29b716bf18e34577623ae7effa46f496bd4282134e7e895284a4760a910d2ad8ec11e9dd75863058cd9c"),
        (127, "e0b6a20f1c0c88970a9340152cd5a1c1ecf3d3b8de55102741879438079473540133b812706e5dbec322c8c9523b6fc8c6d16ee626e87ad5fe3d2916afedc369"),
        (128, "99b16f17aa0b969a5b8f08f367719d516e330ccd2660b6f0688ec031dbc783de50a1cd185a2568dba75070a2403d17d4741d163578515dfd2ff756ddfe4d47b1"),
        (129, "a1556e29185778aa5991e34b8884c840d589f0fbb4b8ed590e51e9ac4eb03a008125000db2671f8fe7f485b59a77b518670078ecb41a54b4cd02a7f1d2ca4c6d"),
        (255, "c2e3bb67012f9eb526202efa59997933f7d3e75e7ded738818bc27d94977f4573afddb1b2793745701e62affa3b7a1c8262c992a321f488a6b1942a4795bab98"),
        (256, "e49c208e41556e859d1a52d14784a061c2d5ae2c8690a5360e9f9344f60861c1362a9ec05a9f08a4167b3da41bdd122a387413dd06976470e4beff5053f2ac71"),
        (1000, "00e36fccf193e59697a92b5ab24666ce6326d7fa16bf10832d0991ddc591112e9dfa6a636950ed9c4d67344a760654c2ff7785e1d60094d651038735b5dccabd")
    ];
    const SHA256_VECTORS: [(usize, &str); 16] = [
        (
            0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            1,
            "084fed08b978af4d7d196a7446a86b58009e636b611db16211b65a9aadff29c5",
        ),
        (
            3,
            "6ab0dba1f4f1dfbb37b4f9eeb092c09fca4900ad32bdcd147d8dde35d6c87c35",
        ),
        (
            55,
            "e7313d333c272e639f790978283f9eb392e843d0f29b7016828bb1daa4aac70b",
        ),
        (
            56,
            "4324d65f3c103567f5589c710bc08f8523f929a9272e3af36fc968e52abc6c27",
        ),
        (
            63,
            "81c80242132f230c3bd41b3e63bbcff16107339549214a99614ff26664625055",
        ),
        (
            64,
            "39e3d7b6b5d075d37d053ad89b24b41bef4f3c29760c84447cab3f3be1882241",
        ),
        (
            111,
            "67d9492e628fd376e0b2efec8ca2b99b123e202cf620deb270728df979b2f73e",
        ),
        (
            112,
            "96b928cff8528dbb99602c709a65b846cb6467acb8b722f0d758e4dc27bfc508",
        ),
        (
            113,
            "cded73eed7df41eecd1ead825a38ca8763a9e910d5181c8ad8f201cc8ff2cbe4",
        ),
        (
            127,
            "a8d23e75d936f303d248888d9b165ee543f4cbafcad3c9dd2a79bd84faa11d07",
        ),
        (
            128,
            "d2742f1f4ac6bb7ca2b239ee18402ba8b3f9f8e652d2a72973c2b9ba11c08cf6",
        ),
        (
            129,
            "307f8fc2c1622b92762e818d39a185d4d667ad49a4b07ceae1f4afa008a93ec4",
        ),
        (
            255,
            "3c8af6e36699077166f180b277f93992e354c66a63a3541ef18d61524eac85e9",
        ),
        (
            256,
            "d9c76fa34978cb9620dab8c3f46bbe075fddc145eb282b39009141f98d0cfe82",
        ),
        (
            1000,
            "1e9bc38cbf860b9ec31918b065f9b52476c549a782e0e7990bed8ce3868d2371",
        ),
    ];

    #[test]
    fn sha512_matches_reference_at_every_padding_boundary() {
        for (n, want) in SHA512_VECTORS {
            let mut h = Sha512Engine::new();
            h.update(&data(n));
            let mut out = [0u8; 64];
            h.finalize(&mut out);
            assert_eq!(hex(&out), want, "length {n}");
        }
    }

    #[test]
    fn sha512_is_independent_of_how_input_is_chunked() {
        let d = data(300);
        let mut whole = Sha512Engine::new();
        whole.update(&d);
        let mut a = [0u8; 64];
        whole.finalize(&mut a);
        for step in [1usize, 7, 64, 127, 128, 129] {
            let mut h = Sha512Engine::new();
            for c in d.chunks(step) {
                h.update(c);
            }
            let mut b = [0u8; 64];
            h.finalize(&mut b);
            assert_eq!(a, b, "chunk size {step}");
        }
    }

    #[test]
    fn sha256_matches_reference_at_every_padding_boundary() {
        for (n, want) in SHA256_VECTORS {
            let mut h = Sha256Engine::new();
            h.update(&data(n));
            let mut out = [0u8; 32];
            h.finalize(&mut out);
            assert_eq!(hex(&out), want, "length {n}");
        }
    }
}

#[cfg(test)]
mod ed25519_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn unhex<const N: usize>(h: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    fn unhex_vec(h: &str) -> Vec<u8> {
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    // RFC 8032 section 7.1 test vectors 1 and 2 (also confirmed with an independent library).
    const RFC8032: [(&str, &str, &str); 2] = [
        ("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "", "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
        ("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72", "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00")
    ];

    // Public key, message length, seed byte, signature: produced by Python `cryptography`.
    // Message i is bytes (i*13 + seed) mod 256.
    const GENERATED: [(&str, usize, u8, &str); 6] = [
        ("8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c", 0, 1, "778cda0634c021fae8b1a9fa655ba13230f6fcfc5c5d519afb0872ec9bf1d64241cc3eed8ad47270d86d30e762ad17677c6fb1797e35bca7eba30388257e020f"),
        ("8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394", 1, 2, "6a90ede10549046d98925f1a142ae8564accd059904e0997140bd68ac47b69cfc0cad0f7a2cddbcbc908c60b67ac489e577b3e8314dd65353a4bd533ae940307"),
        ("ed4928c628d1c2c6eae90338905995612959273a5c63f93636c14614ac8737d1", 31, 3, "1e813e5660aa5381b045296585c95231edddb4e757b3edd7a62e7b6d2d9c6d94aca47adfd729ac834d3abacfc7bdfd13da0adfeccacb136289626433116e1d05"),
        ("ca93ac1705187071d67b83c7ff0efe8108e8ec4530575d7726879333dbdabe7c", 64, 4, "9d99f6b29652aa718446ab2a1a0f738ea9a2edf8a7e9f833f6c8a08be9a27744eb057115905f9f5a7d8514e5ca148d6a4a74d5077281c1f385bd5f7b1e966201"),
        ("6e7a1cdd29b0b78fd13af4c5598feff4ef2a97166e3ca6f2e4fbfccd80505bf1", 100, 5, "d80256c776ce7a1ff5a39a8c5ab21546fc33a880ccf47d52eba9fed0edc9b34bc01db7132f91c02db9140f017d7cec92e26a59d1fdea70cfb30225579c99d40d"),
        ("8a875fff1eb38451577acd5afee405456568dd7c89e090863a0557bc7af49f17", 300, 6, "31db90e50e9ed34e62cb636cd773e19ac8c6346fccee2e4d3e82a7a21037f35782b6c97629133e6b5eab47869fcf1cacc48cad10857093dc0f783152e6d93e07")
    ];

    #[test]
    fn rfc8032_vectors_verify() {
        for (pk, msg, sig) in RFC8032 {
            let pk: [u8; 32] = unhex(pk);
            let sig: [u8; 64] = unhex(sig);
            assert_eq!(Ed25519Verifier::verify(&pk, &unhex_vec(msg), &sig), Ok(()));
        }
    }

    #[test]
    fn independently_generated_signatures_verify() {
        for (pk, n, seed, sig) in GENERATED {
            let msg: Vec<u8> = (0..n)
                .map(|i| ((i * 13 + usize::from(seed)) & 0xFF) as u8)
                .collect();
            assert_eq!(
                Ed25519Verifier::verify(&unhex(pk), &msg, &unhex(sig)),
                Ok(()),
                "message length {n}"
            );
        }
    }

    #[test]
    fn our_own_signatures_verify_too() {
        let sk = SigningKey::from_bytes(&[42u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let sig = sk.sign(b"vehicle update").to_bytes();
        assert_eq!(
            Ed25519Verifier::verify(&pk, b"vehicle update", &sig),
            Ok(())
        );
    }

    #[test]
    fn any_change_to_message_signature_or_key_is_rejected() {
        let (pk, msg, sig) = RFC8032[1];
        let pk: [u8; 32] = unhex(pk);
        let msg = unhex_vec(msg);
        let sig: [u8; 64] = unhex(sig);
        let bad = Err(HsmError::SignatureVerificationFailed);
        // Message altered, extended, truncated.
        assert_eq!(Ed25519Verifier::verify(&pk, &[0x73], &sig), bad);
        assert_eq!(Ed25519Verifier::verify(&pk, &[0x72, 0x00], &sig), bad);
        assert_eq!(Ed25519Verifier::verify(&pk, &[], &sig), bad);
        // Every single-bit flip in the signature (R and S halves) is rejected.
        for bit in 0..512 {
            let mut s = sig;
            s[bit / 8] ^= 1 << (bit % 8);
            assert_eq!(
                Ed25519Verifier::verify(&pk, &msg, &s),
                bad,
                "signature bit {bit}"
            );
        }
        // Wrong key.
        let other = SigningKey::from_bytes(&[7u8; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(Ed25519Verifier::verify(&other, &msg, &sig), bad);
        // Every single-bit flip in the public key is rejected (invalid point or wrong key).
        for bit in 0..256 {
            let mut k = pk;
            k[bit / 8] ^= 1 << (bit % 8);
            assert_eq!(
                Ed25519Verifier::verify(&k, &msg, &sig),
                bad,
                "key bit {bit}"
            );
        }
    }

    #[test]
    fn the_old_accept_everything_inputs_are_now_rejected() {
        let pk = SigningKey::from_bytes(&[1u8; 32])
            .verifying_key()
            .to_bytes();
        let bad = Err(HsmError::SignatureVerificationFailed);
        assert_eq!(Ed25519Verifier::verify(&pk, b"anything", &[0u8; 64]), bad);
        assert_eq!(
            Ed25519Verifier::verify(&pk, b"anything", &[0x55u8; 64]),
            bad
        );
        assert_eq!(
            Ed25519Verifier::verify(&pk, b"anything", &[0xFFu8; 64]),
            bad
        );
    }

    #[test]
    fn weak_small_order_keys_are_rejected() {
        // The identity point (y = 1) and the order-2 point (y = -1) must never be accepted.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let mut order2 = [0xFFu8; 32];
        order2[0] = 0xEC;
        order2[31] = 0x7F;
        for key in [identity, order2] {
            for sig in [[0u8; 64], [1u8; 64]] {
                assert!(Ed25519Verifier::verify(&key, b"msg", &sig).is_err());
            }
        }
    }

    #[test]
    fn a_non_canonical_s_value_is_rejected() {
        // Adding the group order L to S gives a different encoding of the same scalar.
        let (pk, msg, sig) = RFC8032[0];
        let pk: [u8; 32] = unhex(pk);
        let msg = unhex_vec(msg);
        let mut sig: [u8; 64] = unhex(sig);
        const L: [u8; 32] = unhex_l();
        let mut carry = 0u16;
        for i in 0..32 {
            let sum = u16::from(sig[32 + i]) + u16::from(L[i]) + carry;
            sig[32 + i] = (sum & 0xFF) as u8;
            carry = sum >> 8;
        }
        if carry == 0 {
            assert!(Ed25519Verifier::verify(&pk, &msg, &sig).is_err());
        }
    }

    const fn unhex_l() -> [u8; 32] {
        // L = 2^252 + 27742317777372353535851937790883648493, little endian.
        [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
        ]
    }
}
