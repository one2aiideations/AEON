//! MOULI-SDV: an AI-native software development environment for vehicles. It provides
//! building blocks and simulators to develop and verify vehicle software against; it is not a
//! vehicle operating system to deploy.
//!
//!
//! EXPERIMENTAL. Nothing here is certified to ISO 26262, ISO 21434 or any other standard, and
//! the safety-level names in module comments (such as "ASIL-D") describe design intent only.
//! Certification needs qualified tooling, independent verification and a safety case that
//! source code cannot provide. Do not put this in a vehicle.
//!
//! Each module is a self-contained part; parts do not depend on each other. The crate is
//! `no_std` unless the default `std` feature is on, so the logic can be built for a bare-metal
//! target with `--no-default-features`; [`fmath`] supplies the math that `core` lacks.
#![cfg_attr(not(feature = "std"), no_std)]
#![allow(dead_code)]
#![allow(unused_variables)]

pub mod fmath;
pub mod p01_bare_metal_hal;
pub mod p02_covesa_vss_v4_0_signal_tree_;
pub mod p03_sensor_fusion;
pub mod p04_motion_control;
pub mod p05_secoc;
pub mod p06_virtio_paravirtualization;
pub mod p07_can_xl_controller;
pub mod p08a_ieee_802_1as_tsn;
pub mod p09_nvme_storage_driver;
pub mod p10_zero_copy_af_xdp_network_dri;
pub mod p11_real_time_posix_pse51_minima;
pub mod p12_high_reliability_flash_trans;
pub mod p13_automotive_hsm;
pub mod p14_automotive_avb;
pub mod p15_v2x_stack;
pub mod p16_real_time_trajectory_planner;
pub mod p17_multi_core_bootloader;
pub mod p18_linker_scripts;
pub mod p19a_autosar_wdgm_program_flow_mo;
pub mod p19b_mipi_csi_2_camera_pipeline;
pub mod p20_ev_battery_management_system;
pub mod p21_high_definition;
pub mod p22_ev_inverter;
pub mod single_context;
