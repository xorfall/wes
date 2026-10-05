//! Sequential test composition over an exclusively owned session; execution stays in the engine.
mod model;
mod runner;
pub use model::{Scenario, ScenarioError};
pub use runner::{Report, run};
