//! An in-memory CAN bus for tests: what the engine sends is recorded, and what a test injects
//! is delivered. No serial port, no adapter, no timing.

#![allow(non_snake_case, non_upper_case_globals)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use can::CanFrame;
use serial_can::SerialError;

use crate::bus::CanBusPort;

/// The frames travelling in each direction, shared with whatever is driving the test.
#[derive(Debug, Default)]
struct MockBusState {
    m_vecTransmitted: Vec<CanFrame>,
    m_queueInbound: VecDeque<CanFrame>,
}

/// A handle a test uses to play the other end of the bus.
#[derive(Debug, Clone, Default)]
pub struct MockBusHandle {
    m_arcState: Arc<Mutex<MockBusState>>,
}

impl MockBusHandle {
    /// Deliver a frame to the engine, as if a tester had put it on the bus.
    pub fn InjectFrame(&self, frame: CanFrame) {
        let mut state = self.m_arcState.lock().expect("mock bus poisoned");
        state.m_queueInbound.push_back(frame);
    }

    /// Deliver several frames in order.
    pub fn InjectFrames(&self, vecFrames: Vec<CanFrame>) {
        for frame in vecFrames {
            self.InjectFrame(frame);
        }
    }

    /// Everything the engine has put on the bus so far.
    pub fn TransmittedFrames(&self) -> Vec<CanFrame> {
        let state = self.m_arcState.lock().expect("mock bus poisoned");
        state.m_vecTransmitted.clone()
    }

    /// Everything the engine has sent, then forget it — handy between phases of a test.
    pub fn TakeTransmittedFrames(&self) -> Vec<CanFrame> {
        let mut state = self.m_arcState.lock().expect("mock bus poisoned");
        std::mem::take(&mut state.m_vecTransmitted)
    }

    /// A bus port wired to this handle.
    pub fn Bus(&self) -> MockCanBus {
        MockCanBus {
            m_arcState: Arc::clone(&self.m_arcState),
        }
    }
}

/// The engine's end of the mock bus.
pub struct MockCanBus {
    m_arcState: Arc<Mutex<MockBusState>>,
}

impl CanBusPort for MockCanBus {
    fn SendFrame(&mut self, frame: &CanFrame) -> Result<(), SerialError> {
        let mut state = self.m_arcState.lock().expect("mock bus poisoned");
        state.m_vecTransmitted.push(frame.clone());
        Ok(())
    }

    fn ReceiveFrames(&mut self, _f64TimestampSec: f64) -> Result<Vec<CanFrame>, SerialError> {
        let mut state = self.m_arcState.lock().expect("mock bus poisoned");
        Ok(state.m_queueInbound.drain(..).collect())
    }

    fn Describe(&self) -> String {
        "in-memory mock bus".to_string()
    }
}
