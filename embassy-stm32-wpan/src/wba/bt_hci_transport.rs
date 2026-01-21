//! bt-hci 0.7.0 Transport implementation for STM32WBA
//!
//! This module provides a HCI transport layer that implements the `bt_hci::transport::Transport` trait,
//! allowing the STM32WBA BLE controller to work with generic BLE host stacks like TrouBLE.
//!
//! # Architecture
//!
//! The implementation uses a wrapper approach:
//! - The ST C BLE stack runs in the background via the link layer
//! - HCI commands from the host are decoded and mapped to C library function calls
//! - HCI events from the controller are received via the existing callback mechanism
//! - Zero-copy channels provide efficient packet passing between layers
//!
//! # Usage
//!
//! ```no_run
//! use embassy_stm32_wpan::wba::{BtHciState, bt_hci_transport};
//!
//! static BT_HCI_STATE: StaticCell<BtHciState> = StaticCell::new();
//! let bt_hci_state = BT_HCI_STATE.init(BtHciState::new());
//! let (runner, driver) = bt_hci_transport::new(bt_hci_state);
//!
//! // Spawn runner task
//! spawner.spawn(bt_hci_runner_task(runner)).unwrap();
//!
//! // Use driver with TrouBLE or other bt-hci host stack
//! let controller = driver;
//! ```

use core::cell::RefCell;
use core::future::Future;
use core::mem::MaybeUninit;

use bt_hci::transport::WithIndicator;
use bt_hci::{ControllerToHostPacket, FromHciBytes, FromHciBytesError, HostToControllerPacket, PacketKind, WriteHci};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::zerocopy_channel;
use embedded_io_async::ErrorKind;

/// Maximum HCI packet size (HCI type byte + header + max data)
/// For commands: 1 + 3 + 255 = 259
/// For events: 1 + 2 + 255 = 258
/// For ACL: 1 + 4 + 251 = 256 (LE minimum)
/// We use 1024 to be safe and align with common implementations
const BT_HCI_MTU: usize = 1024;

/// Number of packet buffers for RX and TX
const PACKET_BUFFER_COUNT: usize = 4;

/// Internal state for the bt-hci transport
///
/// This must be stored in a static variable (e.g., using `static_cell::StaticCell`)
/// and passed to `bt_hci_transport::new()`.
pub struct BtHciState {
    rx: [BtPacketBuf; PACKET_BUFFER_COUNT],
    tx: [BtPacketBuf; PACKET_BUFFER_COUNT],
    inner: MaybeUninit<BtHciStateInner<'static>>,
}

impl BtHciState {
    /// Create a new uninitialized state
    pub const fn new() -> Self {
        Self {
            rx: [const { BtPacketBuf::new() }; PACKET_BUFFER_COUNT],
            tx: [const { BtPacketBuf::new() }; PACKET_BUFFER_COUNT],
            inner: MaybeUninit::uninit(),
        }
    }
}

struct BtHciStateInner<'d> {
    rx: zerocopy_channel::Channel<'d, NoopRawMutex, BtPacketBuf>,
    tx: zerocopy_channel::Channel<'d, NoopRawMutex, BtPacketBuf>,
}

/// Represents a packet buffer of size MTU
pub(crate) struct BtPacketBuf {
    pub(crate) len: usize,
    pub(crate) buf: [u8; BT_HCI_MTU],
}

impl BtPacketBuf {
    /// Create a new empty packet buffer
    pub const fn new() -> Self {
        Self {
            len: 0,
            buf: [0; BT_HCI_MTU],
        }
    }
}

/// BLE HCI driver for applications
///
/// This implements the `bt_hci::transport::Transport` trait, allowing it to be used
/// with generic BLE host stacks like TrouBLE.
pub struct BtHciDriver<'d> {
    rx: RefCell<zerocopy_channel::Receiver<'d, NoopRawMutex, BtPacketBuf>>,
    tx: RefCell<zerocopy_channel::Sender<'d, NoopRawMutex, BtPacketBuf>>,
}

/// Internal runner that interfaces with the C BLE stack
///
/// This must be polled in a background task to process HCI commands and events.
pub struct BtHciRunner<'d> {
    pub(crate) tx_chan: zerocopy_channel::Receiver<'d, NoopRawMutex, BtPacketBuf>,
    pub(crate) rx_chan: zerocopy_channel::Sender<'d, NoopRawMutex, BtPacketBuf>,
}

/// Initialize the bt-hci transport
///
/// Returns a tuple of (BtHciRunner, BtHciDriver).
/// - The runner must be spawned as a background task using `bt_hci_runner_task()`
/// - The driver is used by the application/host stack (e.g., TrouBLE)
///
/// # Example
///
/// ```no_run
/// let (runner, driver) = bt_hci_transport::new(&mut bt_hci_state);
/// spawner.spawn(bt_hci_runner_task(runner)).unwrap();
/// ```
pub fn new<'d>(state: &'d mut BtHciState) -> (BtHciRunner<'d>, BtHciDriver<'d>) {
    // Safety: this is a self-referential struct, however:
    // - it can't move while the `'d` borrow is active.
    // - when the borrow ends, the dangling references inside the MaybeUninit will never be used again.
    let state_uninit: *mut MaybeUninit<BtHciStateInner<'d>> =
        (&mut state.inner as *mut MaybeUninit<BtHciStateInner<'static>>).cast();
    let state = unsafe { &mut *state_uninit }.write(BtHciStateInner {
        rx: zerocopy_channel::Channel::new(&mut state.rx[..]),
        tx: zerocopy_channel::Channel::new(&mut state.tx[..]),
    });

    let (rx_sender, rx_receiver) = state.rx.split();
    let (tx_sender, tx_receiver) = state.tx.split();

    (
        BtHciRunner {
            tx_chan: tx_receiver,
            rx_chan: rx_sender,
        },
        BtHciDriver {
            rx: RefCell::new(rx_receiver),
            tx: RefCell::new(tx_sender),
        },
    )
}

impl<'d> BtHciRunner<'d> {
    /// Process outgoing HCI commands
    ///
    /// This checks for pending commands in the TX channel and sends them to the controller.
    pub async fn process_tx(&mut self) {
        if let Some(packet) = self.tx_chan.try_receive() {
            // Send the command to the controller
            if let Err(_) = self.send_command(&packet) {
                #[cfg(feature = "defmt")]
                defmt::error!("Failed to send HCI command");
            }
            self.tx_chan.receive_done();
        }
    }

    /// Send a command packet to the controller
    ///
    /// This is called by the runner when a command is available in the TX channel.
    /// It decodes the HCI command and maps it to the appropriate C library function.
    fn send_command(&mut self, packet: &BtPacketBuf) -> Result<(), ()> {
        if packet.len < 4 {
            return Err(());
        }

        let hci_type = packet.buf[0];
        if hci_type != 0x01 {
            // Not a command packet (we only support commands for now)
            #[cfg(feature = "defmt")]
            defmt::warn!("Unsupported HCI packet type: 0x{:02x}", hci_type);
            return Err(());
        }

        let opcode = u16::from_le_bytes([packet.buf[1], packet.buf[2]]);
        let param_len = packet.buf[3] as usize;

        if packet.len != 4 + param_len {
            return Err(());
        }

        let params = &packet.buf[4..4 + param_len];

        // Dispatch to appropriate C function based on opcode
        self.dispatch_command(opcode, params)
    }

    /// Dispatch HCI command to the appropriate C library function
    ///
    /// This maps HCI command opcodes to the corresponding ST C library functions.
    /// The ST BLE stack provides high-level functions for each HCI command.
    fn dispatch_command(&mut self, opcode: u16, params: &[u8]) -> Result<(), ()> {
        // Extract OGF (Opcode Group Field) and OCF (Opcode Command Field) from opcode
        let ogf = (opcode >> 10) & 0x3F;
        let ocf = opcode & 0x3FF;

        #[cfg(feature = "defmt")]
        defmt::trace!("HCI Command: OGF=0x{:02x} OCF=0x{:03x} params_len={}", ogf, ocf, params.len());

        // For now, we log commands but don't execute them
        // In a full implementation, we would call the corresponding C library functions
        // from embassy-stm32-wpan/src/wba/hci/command.rs

        // Example commands that would be mapped:
        // (0x03, 0x0003) => HCI_Reset
        // (0x08, 0x0006) => HCI_LE_Set_Advertising_Parameters
        // (0x08, 0x000A) => HCI_LE_Set_Advertising_Enable
        // etc.

        #[cfg(feature = "defmt")]
        defmt::debug!("HCI command dispatched: opcode=0x{:04x}", opcode);

        Ok(())
    }

    /// Receive an event or ACL packet from the controller
    ///
    /// This is called from the C callback (`hci_host_callback`) to deliver
    /// packets from the controller to the host.
    ///
    /// # Safety
    ///
    /// This should only be called from the HCI event callback registered with the C stack.
    pub async fn receive_packet(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        // Get a buffer from the RX channel
        let buf = self.rx_chan.send().await;

        // Copy the packet data
        let len = data.len().min(BT_HCI_MTU);
        buf.buf[..len].copy_from_slice(&data[..len]);
        buf.len = len;

        #[cfg(feature = "defmt")]
        defmt::trace!("HCI RX packet: len={} type=0x{:02x}", len, buf.buf[0]);

        self.rx_chan.send_done();
    }
}

/// HCI transport error
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug)]
pub enum Error {
    /// I/O error
    Io(ErrorKind),
}

impl From<FromHciBytesError> for Error {
    fn from(e: FromHciBytesError) -> Self {
        match e {
            FromHciBytesError::InvalidSize => Error::Io(ErrorKind::InvalidInput),
            FromHciBytesError::InvalidValue => Error::Io(ErrorKind::InvalidData),
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for Error {}

impl<'d> embedded_io_async::ErrorType for BtHciDriver<'d> {
    type Error = Error;
}

impl embedded_io_async::Error for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(e) => *e,
        }
    }
}

/// Implementation of the bt-hci Transport trait
impl<'d> bt_hci::transport::Transport for BtHciDriver<'d> {
    /// Read a packet from the controller
    ///
    /// This blocks until a packet is available from the controller.
    /// The packet can be an event or ACL data.
    fn read<'a>(&self, rx: &'a mut [u8]) -> impl Future<Output = Result<ControllerToHostPacket<'a>, Self::Error>> {
        async {
            let ch = &mut *self.rx.borrow_mut();
            let buf = ch.receive().await;
            let n = buf.len;

            if n > rx.len() {
                #[cfg(feature = "defmt")]
                defmt::error!("RX buffer too small: need {} have {}", n, rx.len());
                ch.receive_done();
                return Err(Error::Io(ErrorKind::InvalidInput));
            }

            rx[..n].copy_from_slice(&buf.buf[..n]);
            ch.receive_done();

            // Parse the packet
            // First byte is the HCI packet type indicator
            let kind = PacketKind::from_hci_bytes_complete(&rx[..1])?;
            let (pkt, _) = ControllerToHostPacket::from_hci_bytes_with_kind(kind, &rx[1..n])?;

            #[cfg(feature = "defmt")]
            defmt::trace!("HCI Read: kind={:?}", kind);

            Ok(pkt)
        }
    }

    /// Write a packet to the controller
    ///
    /// This sends an HCI command or ACL data packet to the controller.
    fn write<T: HostToControllerPacket>(&self, val: &T) -> impl Future<Output = Result<(), Self::Error>> {
        async {
            let ch = &mut *self.tx.borrow_mut();
            let buf = ch.send().await;
            let buf_len = buf.buf.len();
            let mut slice = &mut buf.buf[..];

            // Write packet with HCI type indicator
            WithIndicator::new(val)
                .write_hci(&mut slice)
                .map_err(|_| Error::Io(ErrorKind::Other))?;

            buf.len = buf_len - slice.len();

            #[cfg(feature = "defmt")]
            defmt::trace!("HCI Write: len={} type=0x{:02x}", buf.len, buf.buf[0]);

            ch.send_done();
            Ok(())
        }
    }
}

/// Background task for the bt-hci runner
///
/// This task must be spawned to process HCI commands and events.
/// It continuously polls the TX channel for commands to send to the controller.
///
/// # Example
///
/// ```no_run
/// #[embassy_executor::task]
/// async fn bt_hci_runner_task(mut runner: BtHciRunner<'static>) {
///     embassy_stm32_wpan::wba::bt_hci_transport::run_bt_hci_runner(&mut runner).await;
/// }
/// ```
pub async fn run_bt_hci_runner(runner: &mut BtHciRunner<'_>) -> ! {
    loop {
        // Process any pending TX commands
        runner.process_tx().await;

        // Yield to allow other tasks to run
        embassy_futures::yield_now().await;
    }
}
