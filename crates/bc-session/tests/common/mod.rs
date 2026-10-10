//! Shared test helpers.

#![allow(dead_code)]

use bc_session::{Connector, FnConnector};

/// Connects to `nvr` through in-memory pipes: with the mock on the test's
/// runtime (`mock_nvr::spawn_in`) and a paused clock, the session and the
/// device run in simulated time.
pub fn mock_connector(nvr: &mock_nvr::MockNvr) -> impl Connector + use<> {
    let addr = nvr.addr();
    FnConnector(move || mock_nvr::connect(addr))
}
