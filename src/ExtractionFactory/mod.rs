#![allow(non_snake_case)]
pub mod cExtractor;
pub mod cppExtractor;
pub mod csharpExtractor;
pub mod goExtractor;
pub mod javaExtractor;
pub mod jsExtractor;
pub mod jsxExtractor;
pub mod kotlinExtractor;
pub mod phpExtractor;
pub mod pythonExtractor;
pub mod rubyExtractor;
pub mod rustExtractor;
pub mod swiftExtractor;
pub mod tsExtractor;
pub mod tsxExtractor;

#[allow(unused_imports)]
pub use javaExtractor::{extract, extract_to_json};
