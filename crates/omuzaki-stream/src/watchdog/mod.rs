//! The watchdog: keeps a stream alive by restarting the process or thread that carries it.
pub mod launch;
pub mod policy;
pub mod supervisor;
