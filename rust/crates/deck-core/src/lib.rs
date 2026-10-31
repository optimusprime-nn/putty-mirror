//! Everything in PuTTYDeck that doesn't touch a window: saved sessions, host
//! keys, the terminal, key encoding and the network sessions.

pub mod algorithms;
mod authplugin;
pub mod compose;
pub mod config;
pub mod filter;
pub mod hostkeys;
pub mod keys;
pub mod ldisc;
pub mod logging;
pub mod options;
pub mod ppk;
pub mod proxy;
mod rlogin;
mod serial;
pub mod session;
pub mod settings;
pub mod ssh;
mod supdup;
mod telnet;
pub mod term;
mod x11auth;
