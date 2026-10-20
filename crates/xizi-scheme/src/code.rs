//! 五笔编码的紧凑表示与字典序区间计算。
//!
//! 编码字母只取 `a..=y`（`z` 为万能键，不进码表）。把每个字母映射为 1..=25，
//! 从高位起每字母占 5 bit（位置 20/15/10/5），于是：
//!
//! - 同一前缀的所有编码在 `u32` 上是**连续区间**，一次二分即可取出全部候选；
//! - 字母值 0 不会被使用，因此尾部零填充可无歧义地区分长度。

use std::fmt;

/// 五笔 86 的最长码长（单字全码与词组码均为 4）。
pub const MAX_CODE_LEN: usize = 4;

/// 已解析的五笔编码（≤ 4 个 `a..=y` 字母）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Code {
    bytes: [u8; MAX_CODE_LEN],
    len: u8,
}

impl Code {
    /// 解析编码字符串；含非法字母、空串或超长返回 `None`。
    pub fn parse(s: &str) -> Option<Self> {
        let raw = s.as_bytes();
        if raw.is_empty() || raw.len() > MAX_CODE_LEN {
            return None;
        }
        let mut bytes = [0u8; MAX_CODE_LEN];
        for (i, c) in raw.iter().enumerate() {
            let c = c.to_ascii_lowercase();
            if !(b'a'..=b'y').contains(&c) {
                return None;
            }
            bytes[i] = c;
        }
        Some(Self {
            bytes,
            len: raw.len() as u8,
        })
    }

    pub fn as_str(&self) -> &str {
        // 构造时已保证全是 ASCII
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 打包为可比较的 `u32`（见模块文档）。
    pub fn packed(&self) -> u32 {
        pack_unchecked(self.as_str())
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 打包编码串；非法输入返回 `None`。长度上限 [`MAX_CODE_LEN`]。
pub fn pack(s: &str) -> Option<u32> {
    Code::parse(s).map(|c| c.packed())
}

fn pack_unchecked(s: &str) -> u32 {
    let mut v = 0u32;
    for (i, b) in s.as_bytes().iter().enumerate() {
        let letter = (*b - b'a' + 1) as u32;
        v |= letter << (20 - 5 * i);
    }
    v
}

/// 前缀对应的编码区间 `[lo, hi)`；`prefix` 非法时返回 `None`。
///
/// 前缀长度 4 时区间退化为单个编码的 32 个槽位，仍是合法区间。
pub fn prefix_bounds(prefix: &str) -> Option<(u32, u32)> {
    let packed = pack(prefix)?;
    let len = prefix.len();
    // len=1 → +1<<20；len=2 → +1<<15；len=3 → +1<<10；len=4 → +1<<5
    let width = 1u32 << (25 - 5 * len);
    Some((packed, packed.saturating_add(width)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_valid_and_rejects_invalid() {
        assert!(Code::parse("vb").is_some());
        assert!(Code::parse("abcd").is_some());
        assert!(Code::parse("").is_none());
        assert!(Code::parse("abcde").is_none());
        assert!(Code::parse("vz").is_none(), "z 是万能键，不进码表");
        assert!(Code::parse("v1").is_none());
    }

    #[test]
    fn packed_is_monotonic_within_same_length() {
        let a = Code::parse("a").unwrap().packed();
        let b = Code::parse("b").unwrap().packed();
        assert!(a < b);
        let aa = Code::parse("aa").unwrap().packed();
        let ab = Code::parse("ab").unwrap().packed();
        assert!(aa < ab);
    }

    #[test]
    fn prefix_range_covers_only_descendants() {
        let (lo, hi) = prefix_bounds("vb").unwrap();
        for code in ["vb", "vbg", "vbgd", "vbyy"] {
            let p = Code::parse(code).unwrap().packed();
            assert!((lo..hi).contains(&p), "{code} 应落在 vb 区间内");
        }
        for code in ["v", "va", "vc", "w"] {
            let p = Code::parse(code).unwrap().packed();
            assert!(!(lo..hi).contains(&p), "{code} 不应落在 vb 区间内");
        }
    }

    #[test]
    fn sibling_prefixes_do_not_overlap() {
        let (lo_a, hi_a) = prefix_bounds("a").unwrap();
        let (lo_b, _) = prefix_bounds("b").unwrap();
        assert_eq!(hi_a, lo_b, "相邻前缀区间必须首尾相接");
        assert!(hi_a > lo_a);
    }
}
