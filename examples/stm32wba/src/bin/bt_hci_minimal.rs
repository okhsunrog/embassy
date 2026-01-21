//! Minimal bt-hci Transport Example for STM32WBA
//!
//! This example demonstrates the basic setup of the bt-hci 0.7.0 transport layer
//! for STM32WBA. It initializes the transport and shows the basic structure needed
//! to use it with TrouBLE or other bt-hci compatible host stacks.
//!
//! Note: This is a minimal example showing the transport setup. To use with TrouBLE,
//! you would add trouble-host as a dependency and pass the controller to it.
//!
//! Hardware: STM32WBA52 or compatible
//!
//! Expected output:
//! - Initialization messages
//! - Transport is ready for use
//! - Can be extended with TrouBLE stack usage

#![no_std]
#![no_main]

use core::cell::RefCell;

use defmt::*;
use embassy_executor::Spawner;
use embassy_stm32::peripherals::RNG;
use embassy_stm32::rcc::{
    AHB5Prescaler, AHBPrescaler, APBPrescaler, PllDiv, PllMul, PllPreDiv, PllSource, Sysclk, VoltageScale, mux,
};
use embassy_stm32::rng::{self, Rng};
use embassy_stm32::{Config, bind_interrupts};
#[cfg(feature = "wba_ble_bt_hci")]
use embassy_stm32_wpan::wba::{BtHciRunner, BtHciState, bt_hci_transport};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<embassy_stm32::peripherals::RNG>;
});

/// Static storage for bt-hci transport state
#[cfg(feature = "wba_ble_bt_hci")]
static BT_HCI_STATE: StaticCell<BtHciState> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = Config::default();

    // Configure PLL1 for 96 MHz system clock
    config.rcc.pll1 = Some(embassy_stm32::rcc::Pll {
        source: PllSource::HSI,
        prediv: PllPreDiv::DIV1,  // HSI / 1 = 16 MHz
        mul: PllMul::MUL30,       // 16 MHz * 30 = 480 MHz VCO
        divr: Some(PllDiv::DIV5), // 480 / 5 = 96 MHz (Sysclk)
        divq: None,
        divp: Some(PllDiv::DIV30), // 16 MHz for SAI
        frac: Some(0),
    });

    config.rcc.ahb_pre = AHBPrescaler::DIV1;
    config.rcc.apb1_pre = APBPrescaler::DIV1;
    config.rcc.apb2_pre = APBPrescaler::DIV1;
    config.rcc.apb7_pre = APBPrescaler::DIV1;
    config.rcc.ahb5_pre = AHB5Prescaler::DIV4; // 24 MHz for radio
    config.rcc.voltage_scale = VoltageScale::RANGE1;
    config.rcc.sys = Sysclk::PLL1_R;

    // Configure RNG clock source to HSI
    config.rcc.mux.rngsel = mux::Rngsel::HSI;

    let p = embassy_stm32::init(config);
    info!("Embassy STM32WBA bt-hci Transport Example");

    // Initialize RNG (required by BLE stack)
    static RNG: StaticCell<Mutex<CriticalSectionRawMutex, RefCell<Rng<'static, RNG>>>> = StaticCell::new();
    let _rng = RNG.init(Mutex::new(RefCell::new(Rng::new(p.RNG, Irqs))));
    info!("RNG initialized");

    #[cfg(feature = "wba_ble_bt_hci")]
    {
        // Initialize bt-hci transport
        let bt_hci_state = BT_HCI_STATE.init(BtHciState::new());
        let (runner, controller) = bt_hci_transport::new(bt_hci_state);

        info!("bt-hci transport initialized");
        info!("Controller ready for use with TrouBLE or other bt-hci host stacks");

        // Spawn the bt-hci runner task
        spawner.spawn(bt_hci_runner_task(runner)).unwrap();
        info!("bt-hci runner task spawned");

        // At this point, 'controller' implements bt_hci::transport::Transport
        // and can be passed to TrouBLE or another bt-hci compatible host stack.
        //
        // Example with TrouBLE (add trouble-host dependency):
        //
        // use trouble_host::{Host, Stack};
        //
        // let host_resources = ...;
        // let host = Host::new(controller, &mut host_resources);
        // let mut stack = Stack::new(host);
        //
        // // Use TrouBLE stack for BLE operations
        // stack.advertise(...).await;

        info!("Transport is ready! Add TrouBLE dependency to use BLE functionality.");
        info!("See: https://github.com/embassy-rs/trouble");

        // For now, just log that the controller is ready
        let _controller_ref = &controller;

        loop {
            info!("bt-hci transport running... (add TrouBLE for BLE functionality)");
            embassy_time::Timer::after_secs(5).await;
        }
    }

    #[cfg(not(feature = "wba_ble_bt_hci"))]
    {
        error!("This example requires the 'wba_ble_bt_hci' feature!");
        error!("Add to Cargo.toml: features = [..., \"wba_ble_bt_hci\"]");
        loop {
            embassy_time::Timer::after_secs(1).await;
        }
    }
}

/// Background task for bt-hci transport runner
///
/// This task processes HCI commands from the host and forwards events
/// from the controller. It must run continuously for the transport to work.
#[cfg(feature = "wba_ble_bt_hci")]
#[embassy_executor::task]
async fn bt_hci_runner_task(mut runner: BtHciRunner<'static>) {
    info!("bt-hci runner task started");
    bt_hci_transport::run_bt_hci_runner(&mut runner).await;
}
