//! Sentinel 核心库：规则配置加载与文本基础工具。
//!
//! CLI 入口见 `main.rs`；本库供其调用，后续阶段陆续加入
//! 审查（audit）、统计（stats）、一致性（consistency）模块。

pub mod audit;
pub mod config;
pub mod consistency;
pub mod input;
pub mod reports;
pub mod rules;
pub mod stats;
pub mod text;
pub mod tools;
