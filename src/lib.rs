#![allow(clippy::collapsible_if)]
//! Rebord : un hôte Windows pour le client iOS de Sill.
//!
//! - `proto` : le protocole, octet pour octet (portable).
//! - `input` : la traduction des entrées de l'app en actions Windows (portable).
//! - `server` : la session et ses règles (portable).
//! - `platform` : Windows réel, ou plateforme synthétique pour les tests.

pub mod cursor;
pub mod input;
pub mod log;
pub mod mdns;
pub mod platform;
pub mod probe;
pub mod proto;
pub mod server;
pub mod tls;
