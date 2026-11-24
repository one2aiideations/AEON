# MOULI-SDV

Software-defined-vehicle building blocks in `no_std` Rust: kernel scheduling and lock-free queues,
vehicle networking (SOME/IP, CAN-FD/XL, ISO-TP, DoIP, TSN), safety supervision (watchdog manager,
lockstep, DEM), secure communication (SecOC, E2E, Uptane OTA, HSM), storage, perception and
control (sensor fusion, MPC, motor and battery control), and V2X.

## Status: experimental. Not certified. Do not put this in a vehicle.

The module headers mention ISO 26262 ASIL-D, ISO 21434 and similar. Those describe design intent
only. Certification needs qualified tooling, independent verification and a safety case; source
code cannot provide any of that, and nothing here has been assessed by anyone.

## Origin

The code came from a large, AI-drafted monolith in 22+ parts. It had never been compiled. Taking
it into the workspace meant splitting it into one module per part (`pNN_*.rs`), removing a
duplicated Part 8, completing a truncated Part 19a, and then testing each part. Most tests that
exist check behaviour against independent references (RFC and FIPS vectors, Python `hashlib` and
`cryptography`, zlib, a simulated plant), not just against the code's own output.

## Defects found and fixed in that first pass

Security and safety, in rough order of seriousness:

| Where | Problem |
|---|---|
| p13 HSM | Ed25519 "verification" accepted **every** signature (`... & 0x00`). Now uses `ed25519-dalek` (already approved in this workspace) with strict verification. |
| p04 OTA | Update check only required the signature bytes not to sum to zero. Now requires valid Director **and** Image-repository Ed25519 signatures over a domain-separated message; fails closed with no keys. |
| p05 SecOC | "AES-CMAC" contained no AES (add-and-rotate mixing; forgeable). Now real AES-128 with an algebraic (table-free) S-box and RFC 4493 CMAC. |
| p13 HSM | One SHA-512 round constant had a typo, so every SHA-512 digest was wrong. |
| p22 inverter | SVPWM sector/dwell-time logic produced negative dwell times and the wrong voltage vector in most sectors; also `NaN` commands reached the PWM registers. |
| p20 battery | EKF voltage model had the wrong current sign convention and a 192-fold unit error on the polarization voltage. |
| p10 network | Receive descriptors (untrusted) were used to slice memory unchecked: a remote panic of the IDS loop. |
| p03, p04, p22 | Angle-wrapping `while x > pi { x -= 2*pi }` loops never terminate for infinity or very large values (hangs steering, tracking and motor control). Replaced by a bounded `wrap_pi`. |
| p17 boot | Cluster barrier was not reusable and could deadlock; replaced with a generation-counter barrier. |
| p08a | Execution-budget overrun was flagged one cycle late (`fetch_add` returns the old value). |
| p09 EDR | Post-crash window was 101 samples instead of 100 (same cause). |
| p21 map | Lane containment returned `false` for every basic lane (needed 6 vertices instead of 3). |
| p01, p11 | Uninitialised-memory (`assume_init`) undefined behaviour in the ring buffer and file-descriptor table. |
| p06 virtio | DMA status byte read non-volatilely inside a poll loop. |

## Building

```
cargo test -p mouli-sdv                          # host build with std, runs the tests
cargo check -p mouli-sdv --no-default-features   # no_std build (what bare metal would use)
```

`fmath` supplies `sqrt`, `sin`, `cos`, `tan`, `atan`, `atan2` and `powi` for `no_std` (no new
dependencies). With `std` on, the standard library's methods are used instead.

## Known limits and open work

* About 200 `unsafe` uses remain (`unsafe_code` is allowed crate-wide). Each needs a review for a
  `// SAFETY:` justification; the ones found so far were fixed, but the audit is not finished.
* Several `Mock`/`simulated` parts (flash, NVMe, DMA, PCIe) model hardware in memory and have
  never touched real devices. Register addresses and layouts are unverified against any datasheet.
* The master orchestrator builds its OTA controller **without keys**, so updates are refused until
  `UptaneDualBankController::with_role_keys` is called with provisioned keys.
* The AES S-box is table-free, but nothing else here has been analysed for timing side channels,
  and key material is not zeroized after use.
* Parts have not been exercised together; they were written as independent modules.
* The lint allow-list in `Cargo.toml` covers style lints only; see the comments there.
