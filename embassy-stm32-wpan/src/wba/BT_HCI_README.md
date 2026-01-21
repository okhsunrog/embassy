# bt-hci 0.7.0 Transport for STM32WBA

This document explains the bt-hci transport implementation for STM32WBA chips, which enables using the [TrouBLE BLE stack](https://github.com/embassy-rs/trouble) and other bt-hci compatible host stacks.

## Overview

The STM32WBA series features an integrated Bluetooth Low Energy (BLE) 5.4 controller. This implementation provides a standard HCI (Host Controller Interface) transport layer that bridges between the ST C BLE stack and Rust BLE host stacks like TrouBLE.

### Architecture

```
┌─────────────────────────────────────┐
│   Application / TrouBLE Host Stack  │
└──────────────┬──────────────────────┘
               │ bt-hci 0.7.0 trait
┌──────────────┴──────────────────────┐
│      BtHciDriver (Transport)        │
│   - Implements bt_hci::Transport    │
│   - Zero-copy packet channels        │
└──────────────┬──────────────────────┘
               │
┌──────────────┴──────────────────────┐
│         BtHciRunner                 │
│   - Command dispatch                │
│   - Event/ACL forwarding            │
└──────────────┬──────────────────────┘
               │
┌──────────────┴──────────────────────┐
│   ST C BLE Stack (libstm32wba_ble)  │
│   - HCI command functions           │
│   - Event callbacks                 │
│   - Link layer                      │
└─────────────────────────────────────┘
```

## Features

- ✅ Implements `bt_hci::transport::Transport` trait (bt-hci 0.7.0)
- ✅ Zero-copy packet passing using embassy channels
- ✅ Async/await interface
- ✅ Compatible with TrouBLE and other bt-hci host stacks
- ⚠️  Command dispatching (currently logs commands - full implementation needed)
- ⚠️  Event forwarding (needs integration with existing callback system)
- ⚠️  ACL data support (requires additional implementation)

## Usage

### 1. Enable the feature

Add the `wba_ble_bt_hci` feature to your `Cargo.toml`:

```toml
[dependencies]
embassy-stm32-wpan = {
    version = "0.1.0",
    features = ["stm32wba52cg", "wba_ble_bt_hci", "ble-stack-basic", "linklayer-ble-basic"]
}
```

### 2. Initialize the transport

```rust
use embassy_stm32_wpan::wba::{BtHciState, bt_hci_transport};
use static_cell::StaticCell;

// Create static state for the HCI transport
static BT_HCI_STATE: StaticCell<BtHciState> = StaticCell::new();

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Initialize state
    let bt_hci_state = BT_HCI_STATE.init(BtHciState::new());

    // Create transport (runner + driver)
    let (runner, controller) = bt_hci_transport::new(bt_hci_state);

    // Spawn runner task to process HCI commands/events
    spawner.spawn(bt_hci_runner_task(runner)).unwrap();

    // Use controller with TrouBLE or other bt-hci host stack
    // (see TrouBLE documentation for details)
}

#[embassy_executor::task]
async fn bt_hci_runner_task(mut runner: BtHciRunner<'static>) {
    bt_hci_transport::run_bt_hci_runner(&mut runner).await;
}
```

### 3. Use with TrouBLE

```rust
use trouble_host::{Host, Stack};

// Create TrouBLE host with the bt-hci controller
let host = Host::new(controller, &mut resources);
let mut stack = Stack::new(host);

// Now use TrouBLE stack for BLE operations
// See: https://github.com/embassy-rs/trouble
```

## Implementation Status

### Current State (Phase 1)

The current implementation provides:

1. **Core Infrastructure**
   - `BtHciState`: Packet buffer storage
   - `BtHciDriver`: Implements `bt_hci::transport::Transport`
   - `BtHciRunner`: Background task for command/event processing
   - Zero-copy channels for efficient packet passing

2. **Command Interface**
   - Command packets are received from the host
   - Opcodes are decoded (OGF/OCF extraction)
   - Commands are logged (not yet dispatched to C functions)

3. **Event Interface**
   - Framework for receiving events from C callback
   - Events can be forwarded to the host via channels

### What's Missing (Phase 2)

To make this fully functional, the following is needed:

1. **Command Dispatch Implementation**
   - Map HCI command opcodes to ST C library functions
   - Call functions like `HCI_RESET`, `HCI_LE_SET_ADVERTISING_PARAMETERS`, etc.
   - Handle command completion events synchronously

   Example:
   ```rust
   match (ogf, ocf) {
       (0x03, 0x0003) => {
           // HCI_Reset
           unsafe { hci_reset() };
           Ok(())
       }
       (0x08, 0x0006) => {
           // HCI_LE_Set_Advertising_Parameters
           // Parse params and call hci_le_set_advertising_parameters
           Ok(())
       }
       // ... more commands
   }
   ```

2. **Event Forwarding Integration**
   - Hook into existing `hci_host_callback` in `hci/host_if.rs`
   - Forward raw HCI events to `BtHciRunner::receive_packet()`
   - Ensure events reach the host stack

3. **ACL Data Support**
   - Implement ACL packet sending (for GATT operations)
   - Implement ACL packet receiving
   - May require finding ACL send/receive functions in C stack

4. **Link Layer Integration**
   - Ensure BLE stack initialization happens before HCI transport
   - Coordinate with existing `ble_runner` task
   - Handle reset and initialization sequences properly

## Comparison with Other Implementations

### embassy-rp (CYW43 - Pico W)

- ✅ Complete implementation with firmware upload
- ✅ Full command/event/ACL support
- ✅ Works with TrouBLE out of the box
- Hardware: External CYW43439 WiFi+BT chip over SDIO/SPI

### embassy-stm32-wpan (STM32WB55)

- ✅ Uses different crate: `stm32wb-hci` (not bt-hci 0.7.0)
- ✅ Dual-core architecture (M4 + M0+ coprocessor)
- ✅ IPCC (Inter-Processor Communication Controller) for HCI
- Hardware: Dual-core with BLE coprocessor

### embassy-stm32-wpan (STM32WBA) - This Implementation

- ✅ Uses bt-hci 0.7.0 (compatible with TrouBLE)
- ⚠️  Partial implementation (framework complete, dispatch needed)
- 🆕 Single-core architecture with integrated BLE controller
- 🆕 Uses ST C library via FFI
- Hardware: Single-core Cortex-M33 with integrated BLE controller

## Benefits of bt-hci Approach

1. **Standard Interface**: Compatible with any bt-hci host stack
2. **Host Stack Choice**: Can use TrouBLE, Apache Nimble, or custom stacks
3. **Portable Code**: BLE application code is portable across platforms
4. **Modern Rust**: Full async/await support, no C BLE host code needed

## Next Steps

To complete the implementation:

1. **Implement Command Dispatch**
   - Add FFI declarations for C HCI functions (if not already present)
   - Implement parameter parsing for each command
   - Call corresponding C functions from `dispatch_command()`

2. **Integrate Event Forwarding**
   - Modify `hci_host_callback` to forward events to transport
   - Ensure events are properly formatted with HCI packet type

3. **Add ACL Data Support**
   - Find ACL data send/receive functions in ST stack
   - Implement ACL packet handling in transport

4. **Create Full Example**
   - Add TrouBLE as dependency
   - Create working advertiser/scanner example
   - Document initialization sequence

5. **Testing**
   - Verify with real hardware (STM32WBA52, WBA54, WBA55, etc.)
   - Test advertising, scanning, connections
   - Validate GATT operations

## Resources

- [TrouBLE BLE Stack](https://github.com/embassy-rs/trouble)
- [bt-hci crate](https://github.com/embassy-rs/bt-hci)
- [Embassy Documentation](https://embassy.dev)
- [STM32WBA Reference Manual](https://www.st.com/resource/en/reference_manual/rm0498-stm32wba5x-advanced-armbased-32bit-mcus-with-80mhz-rf-and-lowpower-2pt4-ghz-transceiver-stmicroelectronics.pdf)

## Contributing

Contributions to complete this implementation are welcome! Key areas:

- Command dispatch implementation
- Event forwarding integration
- ACL data support
- Testing and validation
- Documentation and examples

See the Embassy contribution guidelines for details.
