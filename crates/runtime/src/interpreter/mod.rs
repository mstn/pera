#[cfg(feature = "monty")]
mod monty;

#[cfg(feature = "monty")]
pub use monty::MontyInterpreter;
