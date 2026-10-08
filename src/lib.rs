//! acp-tsp: run Agent Client Protocol agents natively in Tern.
//!
//! The process runs in a Tern agent block. [`ui`] owns the tty and speaks TSP
//! through `tern-sdk`; [`acp`] spawns the agent and speaks ACP as a client.
//! They exchange [`event::BridgeEvent`] and [`event::UiCmd`] over [`bus`].

pub mod acp;
pub mod agents;
pub mod bus;
pub mod config;
pub mod event;
pub mod host;
pub mod launch;
pub mod model;
pub mod tern_settings;
pub mod ui;
