//! Off-chain side of the nearx402 spend policy: x402 wire types, a policy
//! contract client, an agent that pays through the policy, a vendor server
//! that verifies payments on chain, and a sandbox demo world.

pub mod agent;
pub mod deployment;
pub mod facilitator;
pub mod policy;
pub mod reconcile_check;
pub mod server;
pub mod vendor;
pub mod world;
pub mod x402;
pub mod zk;
