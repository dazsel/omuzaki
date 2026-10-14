//! Tests for the life layer: the `LifeLayer` contract, each behaviour, the prototype's liveliness metrics, and
//! stability. The scenes and runners they share are in `harness`.

#![allow(
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::large_types_passed_by_value,
    clippy::unreadable_literal
)] // exact values copied through; small loop counters and frame indices; the Copy config passed by value for readability; recorded golden numbers

mod behaviour;
mod contract;
mod harness;
mod metrics;
mod stability;
