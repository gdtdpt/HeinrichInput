//! 亨利输入法 · 方案数据层：编码表示、词库装载、索引与校验。
//!
//! 本 crate **零第三方依赖、无平台分支**：装载逻辑只读本地文件，可在任何平台
//! 与任何前端（server / cli / 未来的 macOS、Linux 壳）复用。
//!
//! ```
//! use heinrich_scheme::Code;
//! assert_eq!(Code::parse("vb").unwrap().len(), 2);
//! assert!(Code::parse("vz").is_none());
//! ```

mod code;
mod hash;
mod manifest;
mod scheme;

pub use code::{pack, prefix_bounds, Code, MAX_CODE_LEN};
pub use hash::sha256_hex;
pub use manifest::{Manifest, ManifestError};
pub use scheme::{CodeKind, Entry, LoadError, Scheme, SchemeMeta, SchemeStats};
